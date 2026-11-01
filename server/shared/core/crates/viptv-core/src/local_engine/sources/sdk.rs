//! Preserve SDK-required playback context inside the opaque native grant.
use super::*;
use viptv_engine::presentation::Card;

pub(in crate::local_engine) fn sdk_playable(
    scope: Scope,
    card: &Card,
    provider_id: &str,
    job_id: &str,
    stream: viptv_engine::vsources_runtime::SdkStream,
) -> (String, Playable) {
    let id = uuid::Uuid::new_v4().to_string();
    let candidate = SourceCandidate {
        presentation: Some(viptv_engine::vsources_runtime::presentation(&stream)),
        id: id.clone(),
        provider_id: provider_id.into(),
        label: stream
            .meta
            .source_label
            .clone()
            .unwrap_or_else(|| "VIPTV Sources".into()),
        detail: stream.meta.resolution.map(|r| format!("{r}p")),
        quality_height: stream.meta.resolution.map(u32::from),
        audio_languages: if stream.meta.languages.is_empty() {
            stream
                .meta
                .audio_selection
                .as_ref()
                .map(|audio| vec![audio.language.as_str().into()])
                .unwrap_or_default()
        } else {
            stream
                .meta
                .languages
                .iter()
                .map(|language| language.as_str().into())
                .collect()
        },
        audio_flags: stream
            .meta
            .dubbed
            .filter(|dubbed| *dubbed)
            .map(|_| vec!["dubbed".into()])
            .unwrap_or_default(),
        subbed: stream.meta.subbed.unwrap_or(false) || !stream.meta.subtitles.is_empty(),
        subtitle_languages: stream
            .meta
            .subtitles
            .iter()
            .filter_map(|subtitle| subtitle.language.clone())
            .fold(Vec::new(), |mut languages, language| {
                if !languages.contains(&language) {
                    languages.push(language);
                }
                languages
            }),
        ..Default::default()
    };
    let playable = Playable {
        scope,
        metadata_origin: card.metadata_origin.clone(),
        provider_origin: None,
        metadata_card: card
            .metadata_origin
            .as_ref()
            .map(|_| Arc::new(card.clone())),
        candidate,
        kind: card.identity.kind.clone(),
        media_id: card.identity.id.clone(),
        url: stream.url.to_string(),
        headers: stream.meta.request_headers.into_iter().collect(),
        required_audio_index: stream
            .meta
            .audio_selection
            .as_ref()
            .map(|audio| audio.audio_index),
        job_id: Some(job_id.into()),
        expires: Some(
            std::time::Instant::now() + stream.ttl.min(std::time::Duration::from_secs(1800)),
        ),
    };
    (id, playable)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Secrets;
    impl SecretStore for Secrets {
        fn get(&self, _: String) -> Result<Option<String>, CoreError> {
            Ok(None)
        }
        fn put(&self, _: String, _: String) -> Result<(), CoreError> {
            Ok(())
        }
        fn remove(&self, _: String) -> Result<(), CoreError> {
            Ok(())
        }
    }
    #[test]
    fn required_audio_ordinal_survives_opaque_open_and_revision_fence() {
        let dir = std::env::temp_dir().join(format!("viptv-sdk-audio-{}", uuid::Uuid::new_v4()));
        let engine = LocalEngine::new(
            dir.join("local.sqlite").to_string_lossy().into(),
            Arc::new(Secrets),
        )
        .unwrap();
        let profile: viptv_engine::profile::ProfileConfiguration =
            serde_json::from_str(&engine.profile("default".into()).unwrap()).unwrap();
        let card = viptv_engine::stremio::cards(
            r#"{"metas":[{"id":"tt1234567","type":"movie","name":"Fixture"}]}"#,
            "metadata",
        )
        .unwrap()
        .remove(0);
        let mut stream = viptv_engine::vsources_runtime::SdkStream::new(
            url::Url::parse("https://media.example/master.m3u8").unwrap(),
            serde_json::from_str(r#""hls""#).unwrap(),
        );
        stream.meta.audio_selection =
            Some(serde_json::from_str(r#"{"language":"En","audio_index":1}"#).unwrap());
        stream.meta.languages = serde_json::from_str(r#"["En","Fr"]"#).unwrap();
        stream.meta.dubbed = Some(true);
        stream.meta.subbed = Some(false);
        stream.meta.subtitles = serde_json::from_str(
            r#"[{"label":"English","language":"en","url":"https://subs.example/en.vtt"}]"#,
        )
        .unwrap();
        stream
            .meta
            .request_headers
            .insert("Referer".into(), "https://provider.example/".into());
        let (id, grant) = sdk_playable(profile.scope, &card, "vsources:owned", "job", stream);
        assert_eq!(grant.candidate.audio_languages, ["en", "fr"]);
        assert_eq!(grant.candidate.audio_flags, ["dubbed"]);
        assert!(grant.candidate.subbed);
        assert_eq!(grant.candidate.subtitle_languages, ["en"]);
        let public = serde_json::to_string(&grant.candidate).unwrap();
        assert!(!public.contains("media.example") && !public.contains("provider.example"));
        engine.playable.lock().unwrap().insert(id.clone(), grant);
        let opened: Value = serde_json::from_str(&engine.open_source(id.clone()).unwrap()).unwrap();
        assert_eq!(opened["required_audio_index"], 1);
        assert_eq!(opened["headers"][0][0], "Referer");
        engine.install_addon("default".into(), "https://metadata.example/manifest.json".into(),
            r#"{"id":"metadata","name":"Metadata","version":"1","resources":["meta"],"types":["movie"]}"#.into()).unwrap();
        assert!(
            engine.open_source(id).is_err(),
            "required selection does not bypass grant revocation"
        );
        drop(engine);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
