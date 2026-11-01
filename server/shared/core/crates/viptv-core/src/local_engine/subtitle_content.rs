use super::*;
#[uniffi::export]
impl LocalEngine {
    /// SRT/VTT normalization is additive; openSubtitle retains raw native paths.
    pub fn subtitle_content(&self, candidate_id: String) -> Result<String, CoreError> {
        #[cfg(not(any(feature = "live-runtime", feature = "vsources-runtime")))]
        {
            let _ = candidate_id;
            Err(CoreError::InvalidInput)
        }
        #[cfg(any(feature = "live-runtime", feature = "vsources-runtime"))]
        {
            let selected = self
                .subtitle_selections
                .lock()
                .map_err(|_| CoreError::Bridge)?
                .get(&candidate_id)
                .cloned()
                .ok_or(CoreError::InvalidInput)?;
            if selected.created.elapsed() >= std::time::Duration::from_secs(600) {
                return Err(CoreError::InvalidInput);
            }
            self.validate_subtitle_selection(&selected)?;
            let scope = selected.scope.clone();
            let origins = selected
                .metadata_origin
                .iter()
                .chain(selected.provider_origin.iter())
                .cloned()
                .collect::<Vec<_>>();
            let store = self.store.clone();
            let guard: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
                store.lock().ok().is_some_and(|store| {
                    store
                        .profile(&scope)
                        .ok()
                        .flatten()
                        .is_some_and(|profile| profile.scope == scope)
                        && origins
                            .iter()
                            .all(|origin| store.validate_metadata_origin(&scope, origin).is_ok())
                })
            });
            let url = url::Url::parse(&selected.file.url).map_err(|_| CoreError::InvalidInput)?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| CoreError::Bridge)?;
            let response = runtime.block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    viptv_engine::egress::ScopedHttp::new(guard).fetch(
                        viptv_engine::egress::HttpRequest::get(url),
                        2 * 1024 * 1024,
                        false,
                    ),
                )
                .await
                .map_err(|_| CoreError::Bridge)?
                .map_err(|_| CoreError::Bridge)
            })?;
            self.project_subtitle_content(&candidate_id, &selected, response)
        }
    }
}
impl LocalEngine {
    #[cfg(any(feature = "live-runtime", feature = "vsources-runtime"))]
    fn project_subtitle_content(
        &self,
        id: &str,
        selected: &subtitles::SelectedSubtitle,
        response: viptv_engine::egress::HttpResponse,
    ) -> Result<String, CoreError> {
        if selected.created.elapsed() >= std::time::Duration::from_secs(600) {
            return Err(CoreError::InvalidInput);
        }
        self.validate_subtitle_selection(&selected)?;
        if !(200..300).contains(&response.status) || response.truncated {
            return Err(CoreError::Bridge);
        }
        let body = viptv_engine::stremio::subtitle_bytes_vtt(
            &response.body,
            response.headers.get("content-type").map(String::as_str),
        );
        self.validate_subtitle_selection(&selected)?;
        let body = body.map_err(|_| CoreError::SubtitleProjectionUnsupported)?;
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        if !store
            .profile(&selected.scope)
            .map_err(|_| CoreError::Bridge)?
            .is_some_and(|profile| profile.scope == selected.scope)
        {
            return Err(CoreError::InvalidInput);
        }
        for origin in selected
            .metadata_origin
            .iter()
            .chain(selected.provider_origin.iter())
        {
            store
                .validate_metadata_origin(&selected.scope, origin)
                .map_err(|_| CoreError::InvalidInput)?;
        }
        serde_json::to_string(&serde_json::json!({"id":id,"language":selected.file.language,"content_type":"text/vtt; charset=utf-8","body":body})).map_err(|_|CoreError::Bridge)
    }
    #[cfg(any(feature = "live-runtime", feature = "vsources-runtime"))]
    fn validate_subtitle_selection(
        &self,
        selected: &subtitles::SelectedSubtitle,
    ) -> Result<(), CoreError> {
        let store = self.store.lock().map_err(|_| CoreError::Bridge)?;
        if !store
            .profile(&selected.scope)
            .map_err(|_| CoreError::Bridge)?
            .is_some_and(|profile| profile.scope == selected.scope)
        {
            return Err(CoreError::InvalidInput);
        }
        for origin in selected
            .metadata_origin
            .iter()
            .chain(selected.provider_origin.iter())
        {
            store
                .validate_metadata_origin(&selected.scope, origin)
                .map_err(|_| CoreError::InvalidInput)?;
        }
        Ok(())
    }
}

#[cfg(all(test, any(feature = "live-runtime", feature = "vsources-runtime")))]
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
    fn normalized_projection_is_scoped_and_raw_native_formats_remain_available() {
        let dir = std::env::temp_dir().join(format!("viptv-subtitle-{}", uuid::Uuid::new_v4()));
        let engine = LocalEngine::new(
            dir.join("local.sqlite").to_string_lossy().into(),
            Arc::new(Secrets),
        )
        .unwrap();
        let scope = engine
            .store
            .lock()
            .unwrap()
            .profile(&local_scope("default"))
            .unwrap()
            .unwrap()
            .scope;
        let mut selected = subtitles::SelectedSubtitle {
            metadata_origin: None,
            media_context: origins::playback_media_context("movie", "tt1234567", None),
            provider_origin: None,
            scope,
            file: viptv_engine::stremio::SubtitleFile {
                language: "fr".into(),
                url: "https://subtitles.example/raw.ass?private=credential".into(),
            },
            created: std::time::Instant::now(),
        };
        engine
            .subtitle_selections
            .lock()
            .unwrap()
            .insert("own".into(), selected.clone());
        let raw = engine.open_subtitle("own".into()).unwrap();
        assert!(raw.contains("raw.ass"));
        assert!(engine.subtitle_content("unknown".into()).is_err());
        let response = || viptv_engine::egress::HttpResponse {
            url: url::Url::parse(&selected.file.url).unwrap(),
            status: 200,
            headers: std::collections::BTreeMap::from([(
                "content-type".into(),
                "application/x-subrip; charset=windows-1252".into(),
            )]),
            body: b"1\n00:00:01,000 --> 00:00:02,000\nCaf\xe9\n".to_vec(),
            truncated: false,
        };
        let projected = engine
            .project_subtitle_content("own", &selected, response())
            .unwrap();
        assert!(projected.contains("Café"));
        assert!(!projected.contains("credential"));
        assert!(!projected.contains("subtitles.example"));
        let mut unsupported = response();
        unsupported.body = b"[Script Info]\nTitle: Styled subtitle\n".to_vec();
        assert!(matches!(
            engine.project_subtitle_content("own", &selected, unsupported),
            Err(CoreError::SubtitleProjectionUnsupported)
        ));
        assert!(
            engine
                .open_subtitle("own".into())
                .unwrap()
                .contains("raw.ass")
        );
        let received = response();
        selected.created = std::time::Instant::now() - std::time::Duration::from_secs(601);
        assert!(
            engine
                .project_subtitle_content("own", &selected, received)
                .is_err()
        );
        selected.created = std::time::Instant::now();
        let received = viptv_engine::egress::HttpResponse {
            url: url::Url::parse(&selected.file.url).unwrap(),
            status: 200,
            headers: Default::default(),
            body: b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nText\n".to_vec(),
            truncated: false,
        };
        engine
            .rename_profile("default".into(), "Changed".into(), "".into())
            .unwrap();
        assert!(
            engine
                .project_subtitle_content("own", &selected, received)
                .is_err()
        );
        assert!(engine.open_subtitle("own".into()).is_err());
        drop(engine);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
