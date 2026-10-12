use viptv_engine::{profile::ProfileConfiguration, store::Store};

#[test]
fn local_profiles_keep_separate_history_and_remove_deleted_data() {
    let mut store = Store::memory().unwrap();
    let first = ProfileConfiguration::local("local".into(), "first".into(), "First".into());
    let second = ProfileConfiguration::local("local".into(), "second".into(), "Second".into());
    store.save_profile(&first, None).unwrap();
    store.save_profile(&second, None).unwrap();
    store.set_profile_pin(&first.scope, "", "1234").unwrap();
    assert!(store.has_profile_pin(&first.scope).unwrap());
    assert!(!store.has_profile_pin(&second.scope).unwrap());
    assert!(!store.verify_profile_pin(&first.scope, "9999").unwrap());
    assert!(store.verify_profile_pin(&first.scope, "1234").unwrap());
    assert!(
        store
            .rename_profile(&first.scope, "Renamed", "9999")
            .is_err()
    );
    store
        .rename_profile(&first.scope, "Renamed", "1234")
        .unwrap();
    assert_eq!(
        store.profile(&first.scope).unwrap().unwrap().name,
        "Renamed"
    );
    assert!(store.delete_profile(&first.scope, "9999").is_err());
    store.delete_profile(&first.scope, "1234").unwrap();
    assert!(store.profile(&first.scope).unwrap().is_none());
    assert!(!store.has_profile_pin(&first.scope).unwrap());
    assert!(store.profile(&second.scope).unwrap().is_some());
    assert!(store.delete_profile(&second.scope, "").is_err());
}
