#![cfg(feature = "runtime")]
use viptv_engine::{
    Error,
    preferences::{HdrPreference, ProfilePreferences, SourceSelection, SubtitleMode},
    profile::ProfileConfiguration,
    store::Store,
};
#[test]
fn preference_revisions_are_scoped_persistent_and_do_not_revoke_media_configuration() {
    let mut store = Store::memory().unwrap();
    let profile = ProfileConfiguration::local("local".into(), "default".into(), "Default".into());
    store.save_profile(&profile, None).unwrap();
    let first = store.preferences(&profile.scope).unwrap();
    assert_eq!(first.revision, 0);
    let preferences = ProfilePreferences {
        preferred_audio_language: Some("en-US".into()),
        preferred_subtitle_language: Some("es".into()),
        subtitles_enabled: false,
        subtitle_mode: SubtitleMode::Never,
        source_selection: SourceSelection::Best,
        max_quality_height: Some(1080),
        ..ProfilePreferences::default()
    };
    let saved = store
        .update_preferences(&profile.scope, 0, preferences.clone())
        .unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(
        store.preferences(&profile.scope).unwrap().preferences,
        preferences
    );
    assert_eq!(
        store
            .profile(&profile.scope)
            .unwrap()
            .unwrap()
            .scope
            .revision,
        0
    );
    assert!(matches!(
        store.update_preferences(&profile.scope, 0, preferences.clone()),
        Err(Error::Conflict)
    ));
    let mut other = profile.scope.clone();
    other.profile_id = "other".into();
    assert!(store.preferences(&other).is_err());
    let mut invalid = preferences;
    invalid.preferred_audio_language = Some("private\nvalue".into());
    assert!(matches!(
        store.update_preferences(&profile.scope, 1, invalid),
        Err(Error::InvalidQuery)
    ));
}
#[test]
fn subtitle_styles_migrate_defaults_and_reject_invalid_scale_or_css_values(){
 let old=serde_json::json!({"preferred_audio_language":null,"preferred_subtitle_language":null,"subtitles_enabled":true,"source_selection":"ask","max_quality_height":null});let mut prefs:ProfilePreferences=serde_json::from_value(old).unwrap();assert_eq!(prefs.subtitle_font_scale,1.0);assert_eq!(prefs.subtitle_text_color,"#FFFFFF");assert_eq!(prefs.subtitle_background_color,"#00000000");prefs.subtitle_text_color="url(private)".into();assert!(prefs.validate().is_err());prefs.subtitle_text_color="#00FF00".into();prefs.subtitle_background_color="#000000C0".into();prefs.subtitle_font_scale=1.4;assert!(prefs.validate().is_ok());prefs.subtitle_font_scale=0.0;assert!(prefs.validate().is_err());
}
#[test]
fn subtitle_mode_derives_from_legacy_rows_and_decides_enabled() {
    let legacy = |enabled: bool| {
        serde_json::json!({"preferred_audio_language":null,"preferred_subtitle_language":null,"subtitles_enabled":enabled,"source_selection":"ask","max_quality_height":null})
    };
    let on: ProfilePreferences = serde_json::from_value(legacy(true)).unwrap();
    assert_eq!((on.subtitle_mode, on.subtitles_enabled), (SubtitleMode::Always, true));
    let off: ProfilePreferences = serde_json::from_value(legacy(false)).unwrap();
    assert_eq!((off.subtitle_mode, off.subtitles_enabled), (SubtitleMode::Never, false));
    assert_eq!(on.hdr_preference, HdrPreference::Any);
    // A sent mode decides subtitles_enabled, even against a stale boolean.
    let mut foreign = legacy(false);
    foreign["subtitle_mode"] = serde_json::json!("foreign");
    foreign["hdr_preference"] = serde_json::json!("avoid");
    let foreign: ProfilePreferences = serde_json::from_value(foreign).unwrap();
    assert_eq!((foreign.subtitle_mode, foreign.subtitles_enabled), (SubtitleMode::Foreign, true));
    assert!(foreign.validate().is_ok());
    let wire = serde_json::to_value(&foreign).unwrap();
    assert_eq!(wire["subtitle_mode"], "foreign");
    assert_eq!(wire["subtitles_enabled"], true);
    assert_eq!(wire["hdr_preference"], "avoid");
    assert_eq!(serde_json::from_value::<ProfilePreferences>(wire).unwrap(), foreign);
    // GET always carries both fields, defaults included.
    let defaults = serde_json::to_value(ProfilePreferences::default()).unwrap();
    assert_eq!(defaults["subtitle_mode"], "always");
    assert_eq!(defaults["hdr_preference"], "any");
    for (key, value) in [("subtitle_mode", "sometimes"), ("hdr_preference", "maybe")] {
        let mut invalid = legacy(true);
        invalid[key] = serde_json::json!(value);
        assert!(serde_json::from_value::<ProfilePreferences>(invalid).is_err(), "{key}");
    }
    let mut unknown = legacy(true);
    unknown["autoplay"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ProfilePreferences>(unknown).is_err());
    // An on/off-only client keeps `foreign` while subtitles stay on.
    assert_eq!(
        ProfilePreferences::from_update(legacy(true), &foreign).unwrap().subtitle_mode,
        SubtitleMode::Foreign
    );
    assert_eq!(
        ProfilePreferences::from_update(legacy(false), &foreign).unwrap().subtitle_mode,
        SubtitleMode::Never
    );
    let mut always = legacy(true);
    always["subtitle_mode"] = serde_json::json!("always");
    assert_eq!(
        ProfilePreferences::from_update(always, &foreign).unwrap().subtitle_mode,
        SubtitleMode::Always
    );
    let mut broken = foreign.clone();
    broken.subtitles_enabled = false;
    assert!(broken.validate().is_err());
}
