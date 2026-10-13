#![cfg(feature = "runtime")]
use viptv_engine::{
    Error,
    preferences::{ProfilePreferences, SourceSelection},
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
