#![cfg(feature = "runtime")]
use viptv_engine::{Error, profile::ProfileConfiguration, store::Store, stremio};

#[test]
fn cache_expires_is_bounded_and_cannot_cross_profile_or_revision() {
    let mut store = Store::memory().unwrap();
    let mut alice = ProfileConfiguration::local("local".into(), "alice".into(), "Alice".into());
    let bob = ProfileConfiguration::local("local".into(), "bob".into(), "Bob".into());
    store.save_profile(&alice, None).unwrap();
    store.save_profile(&bob, None).unwrap();
    let mut cards = stremio::cards(
        r#"{"metas":[{"id":"tt123","type":"movie","name":"Film"}]}"#,
        "addon:one",
    )
    .unwrap();
    cards[0].progress = Some(0.7);
    store
        .cache_catalog(&alice.scope, "popular", &cards, 1000)
        .unwrap();
    assert_eq!(
        store
            .cached_catalog(&alice.scope, "popular", 1001)
            .unwrap()
            .unwrap()[0]
            .progress,
        None
    );
    assert!(
        store
            .cached_catalog(&bob.scope, "popular", 1001)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .cached_catalog(&alice.scope, "popular", 1300)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .cached_catalog(&alice.scope, "popular", 999)
            .unwrap()
            .is_none()
    );
    for index in 0..12 {
        store
            .cache_catalog(&alice.scope, &format!("row:{index}"), &[], 1001 + index)
            .unwrap();
    }
    assert!(
        store
            .cached_catalog(&alice.scope, "popular", 1013)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .cached_catalog(&alice.scope, "row:11", 1013)
            .unwrap()
            .unwrap()
            .is_empty()
    );
    let old = alice.scope.clone();
    alice.scope.revision = 1;
    store.save_profile(&alice, Some(0)).unwrap();
    assert!(
        store
            .cached_catalog(&alice.scope, "row:11", 1013)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.cache_catalog(&old, "popular", &cards, 1013),
        Err(Error::Conflict)
    );
    store.delete_profile(&alice.scope, "").unwrap();
    alice.scope.revision = 0;
    store.save_profile(&alice, None).unwrap();
    assert!(
        store
            .cached_catalog(&alice.scope, "row:11", 1013)
            .unwrap()
            .is_none()
    );
}
