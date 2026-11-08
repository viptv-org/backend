use serde_json::{json, Value};
use viptv_engine::vsources_runtime::SdkStream;

pub(super) fn sdk_source(stream: &SdkStream) -> Value {
    json!({
        "url": stream.url.as_str(),
        "name": stream.meta.source_label.as_deref().unwrap_or("VIPTV Sources"),
        "title": stream.meta.resolution.map(|height| format!("{height}p")).unwrap_or_default(),
        "presentation": viptv_engine::vsources_runtime::presentation(stream),
        "quality_height": stream.meta.resolution,
        "audio_languages": if let Some(audio) = stream.meta.audio_selection.as_ref() {
            vec![audio.language.as_str()]
        } else {
            stream.meta.languages.iter().map(|language| language.as_str()).collect::<Vec<_>>()
        },
        "audio_flags": stream.meta.dubbed.filter(|dubbed| *dubbed).map(|_| vec!["dubbed"]).unwrap_or_default(),
        "subbed": stream.meta.subbed.unwrap_or(false) || !stream.meta.subtitles.is_empty(),
        "subtitle_languages": stream.meta.subtitles.iter().filter_map(|subtitle| subtitle.language.as_deref()).fold(Vec::new(), |mut languages, language| {
            if !languages.contains(&language) {
                languages.push(language);
            }
            languages
        }),
        "required_audio_index": stream.meta.audio_selection.as_ref().map(|audio| audio.audio_index),
        "behaviorHints": { "proxyHeaders": { "request": stream.meta.request_headers } }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app_state::ResourceLease, auth, auth_integration_tests::fixture, test_support::request,
    };

    fn verified_origin_title() -> (crate::App, crate::ResourceLease, serde_json::Value, u64, crate::engine_v3::origins::MetadataProof) {
        let app = fixture().with_lease(ResourceLease {
            policy_revision: 0,
            principal: auth::Principal::Account { account_id: 1, role: "owner".into(), profile_id: Some(1), session_id: Some("s1".into()) },
            session_id: Some("s1".into()),
        });
        app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id)VALUES(301,'Metadata','https://fixture.invalid/manifest.json',?1,1)", rusqlite::params![json!({"id":"fixture-metadata","name":"Metadata","resources":["meta"],"types":["movie"]}).to_string()]).unwrap();
        crate::test_support::encrypt_fixture_sources(&app);
        let lease = app.request_lease();
        let revision = crate::engine_v3::configuration::read(&app, &lease, 1).unwrap().0.scope.revision;
        let mut card: viptv_engine::presentation::Card = serde_json::from_value(json!({
            "identity":{"id":"tt1375666","kind":"movie","ids":[{"namespace":"imdb","value":"tt1375666"}]},
            "provider_id":"addon:301","title":"Verified title","badges":[]
        })).unwrap();
        crate::engine_v3::origins::stamp(&app, std::slice::from_mut(&mut card), "addon:301").unwrap();
        let proof = crate::engine_v3::origins::MetadataProof::capture(&app, card.clone()).unwrap();
        let value = crate::engine_v3::origins::source_value(&card);
        (app, lease, value, revision, proof)
    }

    #[tokio::test]
    async fn preferred_origin_reuses_verified_metadata_without_another_network_request() {
        let (app, lease, value, revision, proof) = verified_origin_title();
        // This metadata host does not exist. The verified job proof must be reused
        // before any DNS, detail or recommendation lookup can run.
        let card = tokio::time::timeout(std::time::Duration::from_millis(250), super::super::canonical_card(&app, &value, &lease, revision, Some(&proof))).await.unwrap().unwrap();
        assert_eq!(card.title, "Verified title");
        assert_eq!(card.metadata_origin, serde_json::from_value(value["metadata_origin"].clone()).ok());
    }

    #[tokio::test]
    async fn reused_metadata_does_not_bypass_origin_or_configuration_revocation() {
        let (app, lease, value, revision, proof) = verified_origin_title();
        assert!(super::super::canonical_card(&app, &value, &lease, revision + 1, Some(&proof)).await.is_none());
        app.db.lock().unwrap().execute("UPDATE addons SET enabled=0 WHERE id=301", []).unwrap();
        assert!(super::super::canonical_card(&app, &value, &lease, revision, Some(&proof)).await.is_none());
    }

    #[tokio::test]
    async fn reused_metadata_rejects_changed_identity_assertions() {
        let (app, lease, mut value, revision, proof) = verified_origin_title();
        value["card"]["identity"]["ids"] = json!([{"namespace":"imdb","value":"tt9999999"}]);
        assert!(super::super::canonical_card(&app, &value, &lease, revision, Some(&proof)).await.is_none());
    }

    #[test]
    fn sdk_source_projects_languages_dubbing_and_subtitles() {
        let mut stream = SdkStream::new(
            url::Url::parse("https://media.example/master.m3u8").unwrap(),
            serde_json::from_str(r#""hls""#).unwrap(),
        );
        stream.meta.languages = serde_json::from_str(r#"["En","Fr"]"#).unwrap();
        stream.meta.dubbed = Some(true);
        stream.meta.subbed = Some(false);
        stream.meta.subtitles = serde_json::from_str(
            r#"[{"label":"English","language":"en","url":"https://subs.example/en.vtt"},{"label":"English CC","language":"en","url":"https://subs.example/en-cc.vtt"},{"label":"French","language":"fr","url":"https://subs.example/fr.vtt"}]"#,
        )
        .unwrap();

        let source = sdk_source(&stream);
        assert_eq!(source["audio_languages"], json!(["en", "fr"]));
        assert_eq!(source["audio_flags"], json!(["dubbed"]));
        assert_eq!(source["subbed"], true);
        assert_eq!(source["subtitle_languages"], json!(["en", "fr"]));
    }

    #[tokio::test]
    async fn sdk_registration_retains_required_audio_and_transport_headers_privately() {
        let app = fixture();
        let (status, connection) = request(&app, "member-token-1", "PUT", "/api/profiles/1/source-integrations",
            json!({"revision":0,"plugin_id":"vsources","configuration":{"tmdb_api_key":"fixture-sdk-key"}})).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let source = connection["configuration"]["assignments"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|assignment| assignment["connection_id"].as_str())
            .find(|id| id.starts_with("vsources:"))
            .unwrap();
        let scoped = app.clone().with_lease(ResourceLease {
            policy_revision: 0,
            principal: auth::Principal::Account {
                account_id: 1,
                role: "owner".into(),
                profile_id: Some(1),
                session_id: Some("s1".into()),
            },
            session_id: Some("s1".into()),
        });
        let mut stream = SdkStream::new(
            url::Url::parse("https://media.example/master.m3u8").unwrap(),
            serde_json::from_str(r#""hls""#).unwrap(),
        );
        stream.meta.audio_selection =
            Some(serde_json::from_str(r#"{"language":"En","audio_index":1}"#).unwrap());
        stream.meta.request_headers.insert(
            "Referer".into(),
            "https://provider.example/owned-context".into(),
        );
        stream
            .meta
            .request_headers
            .insert("Origin".into(), "https://provider.example".into());
        let (cards, error) = scoped.register(source, vec![sdk_source(&stream)], "movie");
        assert!(error.is_none());
        assert_eq!(cards.len(), 1);
        let public = cards[0].to_string();
        assert!(!public.contains("media.example") && !public.contains("provider.example"));
        {
            let entries = app.streams.lock().unwrap();
            let entry = &entries[cards[0]["id"].as_str().unwrap()];
            assert_eq!(entry.required_audio_index, Some(1));
            assert_eq!(
                entry.headers["referer"],
                "https://provider.example/owned-context"
            );
            assert_eq!(entry.headers["origin"], "https://provider.example");
        }
        stream.meta.audio_selection.as_mut().unwrap().audio_index = 0;
        let (other, error) = scoped.register(source, vec![sdk_source(&stream)], "movie");
        assert!(error.is_none());
        assert_ne!(
            cards[0]["source_fingerprint"],
            other[0]["source_fingerprint"]
        );
        stream.meta.audio_selection.as_mut().unwrap().audio_index = 65536;
        assert!(scoped
            .register(source, vec![sdk_source(&stream)], "movie")
            .0
            .is_empty());
    }
}
