#![cfg(feature="runtime")]
use viptv_engine::{preferences::{ProfilePreferences,SourceSelection},profile::ProfileConfiguration,store::Store,Error};
#[test]
fn preference_revisions_are_scoped_persistent_and_do_not_revoke_media_configuration(){
 let mut store=Store::memory().unwrap();let profile=ProfileConfiguration::local("local".into(),"default".into(),"Default".into());store.save_profile(&profile,None).unwrap();let first=store.preferences(&profile.scope).unwrap();assert_eq!(first.revision,0);
 let preferences=ProfilePreferences{preferred_audio_language:Some("en-US".into()),preferred_subtitle_language:Some("es".into()),subtitles_enabled:false,source_selection:SourceSelection::Best,max_quality_height:Some(1080)};
 let saved=store.update_preferences(&profile.scope,0,preferences.clone()).unwrap();assert_eq!(saved.revision,1);assert_eq!(store.preferences(&profile.scope).unwrap().preferences,preferences);assert_eq!(store.profile(&profile.scope).unwrap().unwrap().scope.revision,0);assert!(matches!(store.update_preferences(&profile.scope,0,preferences.clone()),Err(Error::Conflict)));
 let mut other=profile.scope.clone();other.profile_id="other".into();assert!(store.preferences(&other).is_err());let mut invalid=preferences;invalid.preferred_audio_language=Some("private\nvalue".into());assert!(matches!(store.update_preferences(&profile.scope,1,invalid),Err(Error::InvalidQuery)));
}
