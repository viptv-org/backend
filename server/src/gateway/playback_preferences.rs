use super::*;

pub(super) fn apply(
    db: &Connection,
    lease: &ResourceLease,
    request: &mut Start,
) -> Result<(), Error> {
    lease.validate(db)?;
    let crate::auth::Principal::Account {
        profile_id: Some(profile),
        account_id,
        ..
    } = lease.principal
    else {
        return Ok(());
    };
    let preferences = crate::engine_v3::preferences::read(db, account_id, profile)?.preferences;
    apply_v3(request, &preferences)?;
    Ok(())
}

fn apply_v3(
    request: &mut Start,
    preferences: &viptv_engine::preferences::ProfilePreferences,
) -> Result<(), Error> {
    use viptv_engine::preferences::SubtitleMode;
    if request.preferred_audio_language.is_none()
        && request.audio_language.is_none()
        && request.audio_track.is_none()
    {
        request.preferred_audio_language = preferences.preferred_audio_language.clone();
    }
    let explicit = request.subtitles_off
        || request.subtitle_track.is_some()
        || request.preferred_subtitle_language.is_some();
    if !explicit {
        match preferences.subtitle_mode {
            SubtitleMode::Never => request.subtitles_off = true,
            SubtitleMode::Always => request.preferred_subtitle_language = preferences.preferred_subtitle_language.clone(),
            SubtitleMode::Foreign if request.client.can_play_direct => {}
            SubtitleMode::Foreign => request.preferred_subtitle_language = preferences.preferred_subtitle_language.clone(),
        }
    }
    request.subtitle_appearance = Some(
        json!({"font_scale":preferences.subtitle_font_scale,"text_color":preferences.subtitle_text_color,"background_color":preferences.subtitle_background_color}),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Start {
        serde_json::from_value(json!({"request_id":"preferences","stream_id":"source","client":{"platform":"roku","can_play_direct":false,"max_width":1920,"max_height":1080,"video_codecs":["h264"],"audio_codecs":["aac"]}})).unwrap()
    }
    #[test]
    fn v3_defaults_and_explicit_track_intents_are_distinct() {
        let mut prefs = viptv_engine::preferences::ProfilePreferences::default();
        prefs.preferred_audio_language = Some("ja".into());
        prefs.preferred_subtitle_language = Some("en".into());
        let mut start = request();
        apply_v3(&mut start, &prefs).unwrap_or_else(|_| panic!("valid preferences"));
        assert_eq!(start.preferred_audio_language.as_deref(), Some("ja"));
        assert_eq!(start.preferred_subtitle_language.as_deref(), Some("en"));
        assert_eq!(start.subtitle_appearance.unwrap()["font_scale"], 1.0);
        prefs.subtitle_mode = viptv_engine::preferences::SubtitleMode::Never;
        let mut start = request();
        apply_v3(&mut start, &prefs).unwrap_or_else(|_| panic!("never supported"));
        assert!(start.subtitles_off);
        let mut start = request();
        start.subtitle_track = Some(3);
        apply_v3(&mut start, &prefs).unwrap_or_else(|_| panic!("explicit supported"));
        assert!(!start.subtitles_off);
        assert_eq!(start.subtitle_track, Some(3));
        prefs.subtitle_mode = viptv_engine::preferences::SubtitleMode::Foreign;
        assert!(apply_v3(&mut start, &prefs).is_ok());
        assert_eq!(start.subtitle_track, Some(3));
    }
    #[test]
    fn foreign_mode_is_admitted_and_left_to_direct_clients() {
        let mut prefs = viptv_engine::preferences::ProfilePreferences::default();
        prefs.preferred_subtitle_language = Some("en".into());
        prefs.subtitle_mode = viptv_engine::preferences::SubtitleMode::Foreign;
        let mut gateway = request();
        apply_v3(&mut gateway, &prefs).unwrap_or_else(|_| panic!("foreign admitted on a gateway"));
        assert_eq!(gateway.preferred_subtitle_language.as_deref(), Some("en"));
        assert!(!gateway.subtitles_off);
        let mut direct: Start = serde_json::from_value(json!({"request_id":"preferences","stream_id":"source","client":{"platform":"android","can_play_direct":true,"max_width":1080,"max_height":2412,"video_codecs":["h264"],"audio_codecs":["aac"]}})).unwrap();
        apply_v3(&mut direct, &prefs).unwrap_or_else(|_| panic!("foreign admitted for direct play"));
        assert!(direct.preferred_subtitle_language.is_none());
        assert!(!direct.subtitles_off);
    }
}
