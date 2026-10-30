use serde_json::{json, Value};
use viptv_engine::vsources_runtime::SdkStream;

pub(super) fn sdk_source(stream: &SdkStream) -> Value {
    json!({
        "url": stream.url.as_str(),
        "name": stream.meta.source_label.as_deref().unwrap_or("VIPTV Sources"),
        "title": stream.meta.resolution.map(|height| format!("{height}p")).unwrap_or_default(),
        "presentation": viptv_engine::vsources_runtime::presentation(stream),
        "quality_height": stream.meta.resolution,
        "audio_languages": stream.meta.audio_selection.as_ref().map(|audio| vec![audio.language.as_str()]).unwrap_or_default(),
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
