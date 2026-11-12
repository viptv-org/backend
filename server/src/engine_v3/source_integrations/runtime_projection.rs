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

    async fn sdk_registration_fixture() -> (crate::App, String, [u8; 32]) {
        let app = fixture();
        let (status, connection) = request(&app, "member-token-1", "PUT", "/api/profiles/1/source-integrations",
            json!({"revision":0,"plugin_id":"vsources","configuration":{"tmdb_api_key":"fixture-sdk-key"}})).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let source = connection["configuration"]["assignments"].as_array().unwrap().iter()
            .filter_map(|assignment| assignment["connection_id"].as_str())
            .find(|id| id.starts_with("vsources:")).unwrap().to_owned();
        let scoped = app.with_lease(ResourceLease {
            policy_revision: 0,
            principal: auth::Principal::Account {
                account_id: 1, role: "owner".into(), profile_id: Some(1), session_id: Some("s1".into()),
            },
            session_id: Some("s1".into()),
        });
        let proof = crate::sources::source_configuration(&scoped.db.lock().unwrap(), &source).unwrap().unwrap();
        (scoped, source, proof)
    }

    #[tokio::test]
    async fn sdk_unknown_facts_are_not_reconstructed_from_display_text() {
        let (app, source, _) = sdk_registration_fixture().await;
        for display in ["Johnny English", "French Kiss", "Hindi Medium", "Catalog 1080p HEVC AAC"] {
            let mut stream = SdkStream::new(
                url::Url::parse("https://media.example/opaque.m3u8").unwrap(),
                serde_json::from_str(r#""hls""#).unwrap(),
            );
            stream.label = Some(format!("{display} — Server 1"));
            stream.meta.source_label = Some(display.into());
            let (cards, error) = app.register(&source, vec![sdk_source(&stream)], "movie");
            assert!(error.is_none()); assert_eq!(cards.len(), 1);
            let card = &cards[0];
            assert_eq!(card["audio_languages"], json!([]), "display: {display}");
            assert!(card["quality_height"].is_null(), "display: {display}");
            assert!(card["presentation"]["video_codec"].is_null());
            assert_eq!(card["presentation"]["audio_codecs"], json!([]));
            let candidate = crate::engine_v3::source_ranking::candidate_from_card("candidate".into(), &source, card);
            assert!(candidate.audio_languages.is_empty()); assert!(candidate.quality_height.is_none());
            assert!(candidate.presentation.unwrap().video_codec.is_none());
        }
    }

    #[tokio::test]
    async fn sdk_declared_audio_and_subtitles_remain_independent() {
        let (app, source, _) = sdk_registration_fixture().await;
        let mut stream = SdkStream::new(url::Url::parse("https://media.example/opaque.m3u8").unwrap(),
            serde_json::from_str(r#""hls""#).unwrap());
        stream.meta.source_label = Some("Johnny English 1080p HEVC".into());
        stream.meta.audio_selection = Some(serde_json::from_str(r#"{"language":"Fr","audio_index":0}"#).unwrap());
        stream.meta.resolution = Some(720);
        let mut raw = sdk_source(&stream);
        raw["subbed"] = json!(true);
        raw["subtitle_languages"] = json!(["en"]);
        let (cards, error) = app.register(&source, vec![raw], "movie");
        assert!(error.is_none());
        let card = &cards[0];
        assert_eq!(card["audio_languages"], json!(["fr"]));
        assert_eq!(card["quality_height"], 720);
        assert_eq!(card["subtitle_languages"], json!(["en"]));
        let candidate = crate::engine_v3::source_ranking::candidate_from_card("candidate".into(), &source, card);
        assert_eq!(candidate.audio_languages, ["fr"]);
        assert!(candidate.subbed);
        assert_eq!(candidate.subtitle_languages, ["en"]);
    }

    #[tokio::test]
    async fn sdk_header_context_keeps_its_own_expiry_and_source_identity() {
        let (app, source, proof) = sdk_registration_fixture().await;
        let mut short = SdkStream::new(
            url::Url::parse("https://media.example/shared.m3u8").unwrap(),
            serde_json::from_str(r#""hls""#).unwrap(),
        );
        short.ttl = std::time::Duration::from_secs(10);
        short.meta.audio_selection = Some(serde_json::from_str(r#"{"language":"En","audio_index":1}"#).unwrap());
        short.meta.request_headers.insert("Referer".into(), "https://first.example/".into());
        let mut long = short.clone(); long.ttl = std::time::Duration::from_secs(300);
        long.meta.request_headers.insert("Referer".into(), "https://second.example/".into());
        let job = std::sync::Arc::new(crate::app_state::Job {
            metadata_proof: None, source_slots: std::sync::atomic::AtomicUsize::new(0),
            cancelled: std::sync::atomic::AtomicBool::new(false), configuration_revision: None,
            media_id: None, exact_vod: None, kind: "movie".into(), created: std::time::Instant::now(),
            state: std::sync::Mutex::new(crate::app_state::JobState { events: vec![], pending: 0 }),
            notify: tokio::sync::Notify::new(),
        });
        let authorize: std::sync::Arc<dyn Fn() -> bool + Send + Sync> = std::sync::Arc::new(|| true);
        let mut duplicate = short.clone(); duplicate.ttl = std::time::Duration::from_secs(300);
        duplicate.meta.request_headers.remove("Referer");
        duplicate.meta.request_headers.insert("referer".into(), "https://first.example/".into());
        super::super::publish_batch(&app, &job, &source, proof, &authorize, vec![short, long, duplicate]);
        let cards = job.state.lock().unwrap().events[0]["streams"].as_array().unwrap().clone();
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0]["source_fingerprint"], cards[2]["source_fingerprint"]);
        let distinct_identity = cards[0]["source_fingerprint"] != cards[1]["source_fingerprint"];
        let entries = app.streams.lock().unwrap();
        let first = &entries[cards[0]["id"].as_str().unwrap()];
        let second = &entries[cards[1]["id"].as_str().unwrap()];
        assert_eq!(first.required_audio_index, Some(1));
        assert_eq!(first.headers["referer"], "https://first.example/");
        assert_eq!(second.headers["referer"], "https://second.example/");
        let duplicate = &entries[cards[2]["id"].as_str().unwrap()];
        assert!(duplicate.upstream_deadline.unwrap().saturating_duration_since(std::time::Instant::now()) <= std::time::Duration::from_secs(10));
        let distinct_expiry = second.upstream_deadline.unwrap().duration_since(first.upstream_deadline.unwrap()) >= std::time::Duration::from_secs(250);
        assert!(distinct_identity && distinct_expiry, "header-scoped identity: {distinct_identity}; header-scoped expiry: {distinct_expiry}");
        assert!(!cards[0].to_string().contains("first.example"));
    }

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
        stream.meta.audio_selection.as_mut().unwrap().language = serde_json::from_str(r#""Fr""#).unwrap();
        let (french, error) = scoped.register(source, vec![sdk_source(&stream)], "movie");
        assert!(error.is_none());
        assert_ne!(other[0]["source_fingerprint"], french[0]["source_fingerprint"]);
        assert_eq!(french[0]["audio_languages"], json!(["fr"]));
        stream.meta.audio_selection.as_mut().unwrap().audio_index = 65536;
        assert!(scoped
            .register(source, vec![sdk_source(&stream)], "movie")
            .0
            .is_empty());
    }
}
