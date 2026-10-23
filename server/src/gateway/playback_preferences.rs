//! Admission snapshots for shared v3 preferences. An explicit track/language
//! intent wins; old clients with only a legacy saved row retain its behavior.
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
    let value: Option<String> = db
        .query_row(
            "SELECT value FROM engine_profile_preferences WHERE profile_id=?1 AND account_id=?2",
            params![profile, account_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| Error::Code("provider_storage_unavailable"))?;
    if let Some(value) = value {
        let preferences: viptv_engine::preferences::ProfilePreferences =
            serde_json::from_str(&value)
                .map_err(|_| Error::Code("provider_storage_unavailable"))?;
        apply_v3(request, &preferences)?;
    } else {
        let preferences = crate::preferences::load(db, profile)?;
        request
            .preferred_audio_language
            .get_or_insert(preferences.audio_language);
        if !request.subtitles_off
            && request.subtitle_track.is_none()
            && preferences.subtitles_enabled
        {
            request
                .preferred_subtitle_language
                .get_or_insert(preferences.subtitle_language);
        }
    }
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
            // The current gateway protocol cannot compare the actual selected
            // audio language with profile policy. Do not silently execute a
            // different policy, or ask a thin client to guess that language.
            SubtitleMode::Foreign => return Err(ApiError(StatusCode::CONFLICT,
                "Foreign-audio subtitles are unavailable on this gateway. Choose Always, Never, or an explicit subtitle track.".into()).into()),
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
        assert!(apply_v3(&mut request(), &prefs).is_err());
        assert!(apply_v3(&mut start, &prefs).is_ok());
    }
}
