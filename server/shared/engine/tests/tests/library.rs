#![cfg(feature = "runtime")]
use viptv_engine::{
    Error, identity::MediaIdentity, presentation::Card, profile::ProfileConfiguration, store::Store,
};
fn card(id: &str, title: &str) -> Card {
    serde_json::from_value(serde_json::json!({"identity":{"id":id,"kind":"movie","ids":[{"namespace":"addon","value":id},{"namespace":"imdb","value":"tt1234567"}],"episode":null,"edition":null},"provider_id":"stremio","title":title,"subtitle":null,"description":"Provider description","poster":"https://images.example/signed?token=private","landscape":"https://images.example/frame.jpg","logo":null,"progress":0.8,"badges":[],"parent_identity":null})).unwrap()
}
#[test]
fn favorites_reopen_page_and_preserve_provider_identity_with_revision_and_profile_fences() {
    let path = std::env::temp_dir().join(format!(
        "viptv-library-{}.sqlite",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut store = Store::open(&path).unwrap();
    let mut a = ProfileConfiguration::local("local".into(), "alice".into(), "Alice".into());
    let b = ProfileConfiguration::local("local".into(), "bob".into(), "Bob".into());
    store.save_profile(&a, None).unwrap();
    store.save_profile(&b, None).unwrap();
    store
        .save_favorite(&a.scope, &card("opaque:a", "Alpha"))
        .unwrap();
    store
        .save_favorite(&a.scope, &card("opaque:a", "Alpha"))
        .unwrap();
    assert_eq!(store.library_revision(&a.scope).unwrap(), 1);
    std::thread::sleep(std::time::Duration::from_millis(2));
    store
        .save_favorite(&a.scope, &card("opaque:b", "Beta"))
        .unwrap();
    store
        .save_favorite(&b.scope, &card("opaque:b", "Bob title"))
        .unwrap();
    let page = store.favorites(&a.scope, 0, 1).unwrap();
    assert_eq!(page.cards[0].identity, card("opaque:b", "Beta").identity);
    assert_eq!(page.cards[0].provider_id, "stremio");
    assert!(page.cards[0].progress.is_none());
    assert!(page.cards[0].poster.is_none());
    assert_eq!(
        page.cards[0].landscape.as_deref(),
        Some("https://images.example/frame.jpg")
    );
    assert_eq!(page.next_cursor.as_deref(), Some("1"));
    assert!(!page.complete);
    let mut stale = a.scope.clone();
    stale.revision = 99;
    assert!(matches!(
        store.save_favorite(&stale, &card("opaque:c", "Gamma")),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        store.remove_favorite(&stale, "movie", "opaque:a"),
        Err(Error::Conflict)
    ));
    let mut unknown = a.scope.clone();
    unknown.profile_id = "missing".into();
    assert!(matches!(
        store.favorites(&unknown, 0, 40),
        Err(Error::Forbidden)
    ));
    let mut invalid = card("", "Invalid");
    assert!(matches!(
        store.save_favorite(&a.scope, &invalid),
        Err(Error::InvalidQuery)
    ));
    invalid.identity = MediaIdentity {
        id: "opaque:invalid".into(),
        kind: "movie".into(),
        ids: vec![],
        episode: None,
        edition: None,
    };
    invalid.parent_identity = Some(invalid.identity.clone());
    assert!(matches!(
        store.save_favorite(&a.scope, &invalid),
        Err(Error::InvalidQuery)
    ));
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store.favorites(&a.scope, 1, 1).unwrap().cards[0].title,
        "Alpha"
    );
    store
        .remove_favorite(&a.scope, "movie", "opaque:a")
        .unwrap();
    let revision = store.library_revision(&a.scope).unwrap();
    store
        .remove_favorite(&a.scope, "movie", "opaque:a")
        .unwrap();
    assert_eq!(store.library_revision(&a.scope).unwrap(), revision);
    assert_eq!(
        store.favorites(&b.scope, 0, 40).unwrap().cards[0].title,
        "Bob title"
    );
    store.delete_profile(&a.scope, "").unwrap();
    a.scope.revision=store.new_profile_revision(&a.scope).unwrap();
    store.save_profile(&a, None).unwrap();
    assert!(store.favorites(&a.scope, 0, 40).unwrap().cards.is_empty());
    assert_eq!(store.library_revision(&a.scope).unwrap(), 0);
    drop(store);
    std::fs::remove_file(path).unwrap();
}
#[test]
fn legacy_unknown_added_dates_stay_unknown_and_new_upserts_preserve_first_add(){
 let path=std::env::temp_dir().join(format!("viptv-library-dates-{}.sqlite",std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));let mut store=Store::open(&path).unwrap();let profile=ProfileConfiguration::local("local".into(),"alice".into(),"Alice".into());store.save_profile(&profile,None).unwrap();drop(store);
 let db=rusqlite::Connection::open(&path).unwrap();db.execute_batch("ALTER TABLE engine_favorites DROP COLUMN added_at_millis").unwrap();db.execute("INSERT INTO engine_favorites VALUES('local','local','alice','movie','legacy','Legacy',?1)",[serde_json::to_string(&card("legacy","Legacy")).unwrap()]).unwrap();drop(db);
 let mut store=Store::open(&path).unwrap();assert_eq!(store.favorites(&profile.scope,0,10).unwrap().cards[0].added_at_millis,None);store.save_favorite(&profile.scope,&card("legacy","Updated legacy")).unwrap();assert_eq!(store.favorites(&profile.scope,0,10).unwrap().cards[0].added_at_millis,None);
 let mut incoming=card("new","New");incoming.added_at_millis=Some(1);store.save_favorite(&profile.scope,&incoming).unwrap();let first=store.favorites(&profile.scope,0,10).unwrap();assert_eq!(first.cards[0].identity.id,"new");let timestamp=first.cards[0].added_at_millis.unwrap();assert!(timestamp>1);store.save_favorite(&profile.scope,&card("new","Updated")).unwrap();assert_eq!(store.favorites(&profile.scope,0,10).unwrap().cards[0].added_at_millis,Some(timestamp));drop(store);std::fs::remove_file(path).unwrap();
}
