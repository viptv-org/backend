use super::*;
use crate::{auth_integration_tests::fixture, test_support::request};
use axum::routing::get;
use std::sync::atomic::{AtomicBool, Ordering};
async fn stack() -> (App, tokio::task::JoinHandle<()>, Arc<AtomicBool>) {
    let changed = Arc::new(AtomicBool::new(false));
    let router=Router::new().route("/a/:addon/meta/:kind/:id",get({let changed=changed.clone();move|Path((addon,kind,id)):Path<(i64,String,String)>|{let changed=changed.clone();async move{let id=id.trim_end_matches(".json");let alias=if id=="shared-drop"&&changed.load(Ordering::SeqCst){None}else if id.starts_with("shared"){Some(if addon==101&&changed.load(Ordering::SeqCst){"tt456"}else{"tt123"})}else{None};axum::Json(json!({"meta":{"id":id,"type":kind,"name":format!("Fixture {addon}"),"imdb_id":alias}}))}}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut app = fixture();
    app.addons.allow_test_loopback = true;
    for id in [101, 102, 103] {
        let manifest = json!({"id":format!("scoped{id}"),"name":"Scoped fixture","resources":if id==103{json!(["stream"])}else{json!(["meta"])},"types":["movie","series"]});
        app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id)VALUES(?1,'Scoped',?2,?3,1)",params![id,format!("http://{address}/a/{id}/manifest.json"),manifest.to_string()]).unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&app);
    (app, task, changed)
}
fn profile_app(app: &App) -> App {
    app.clone().with_lease(ResourceLease {
        policy_revision: 0,
        principal: auth::Principal::Account {
            account_id: 1,
            role: "owner".into(),
            profile_id: Some(1),
            session_id: Some("s1".into()),
        },
        session_id: Some("s1".into()),
    })
}
async fn call(app: &App, method: &str, path: &str, body: Value) -> Value {
    let (status, result) = request(app, "member-token-1", method, path, body).await;
    assert_eq!(status, StatusCode::OK, "{method} {path}: {result}");
    result
}
async fn detail(app: &App, addon: i64, id: &str) -> Value {
    call(
        app,
        "GET",
        &format!("/api/meta/movie/{id}?provider_id=stremio&addon_id={addon}"),
        Value::Null,
    )
    .await["item"]
        .clone()
}
async fn save(app: &App, card: &Value) -> Value {
    call(
        app,
        "PUT",
        "/api/profiles/1/library",
        json!({"card":card}),
    )
    .await
}
fn removal(card: &Value) -> String {
    let mut q = url::form_urlencoded::Serializer::new(String::new());
    q.append_pair("metadata_origin", &card["metadata_origin"].to_string())
        .append_pair("card", &card.to_string());
    format!(
        "/api/profiles/1/library/movie/{}?{}",
        card["identity"]["id"].as_str().unwrap(),
        q.finish()
    )
}
async fn progress(app: &App, card: &Value, position: u64) -> Value {
    let mut record = json!({"identity":card["identity"],"metadata_origin":card["metadata_origin"],"parent_identity":card["parent_identity"],"provider_id":card["provider_id"],"title":"UNTRUSTED presentation","poster":null,"landscape":null,"position_millis":position,"duration_millis":100000,"updated_at_millis":1,"completed_at_millis":1,"times_watched":999,"presentation":card});
    if card["metadata_origin"].is_null() {
        record.as_object_mut().unwrap().remove("metadata_origin");
    }
    call(
        app,
        "PUT",
        "/api/profiles/1/progress",
        json!({"record":record}),
    )
    .await
}
#[tokio::test]
async fn same_opaque_ids_are_separate_from_each_other_and_from_legacy_for_every_read() {
    let (app, task, _) = stack().await;
    let a = detail(&app, 101, "same").await;
    let b = detail(&app, 102, "same").await;
    let mut legacy = a.clone();
    legacy.as_object_mut().unwrap().remove("metadata_origin");
    legacy["title"] = json!("Legacy");
    for c in [&legacy, &a, &b] {
        save(&app, c).await;
    }
    progress(&app, &a, 20000).await;
    progress(&app, &b, 99000).await;
    progress(&app, &legacy, 40000).await;
    let owned = profile_app(&app);
    {
        let db = app.db.lock().unwrap();
        assert_eq!(
            resume_db(&db, &owned, 1, &serde_json::from_value(a.clone()).unwrap()).unwrap(),
            20.0
        );
        assert_eq!(
            resume_db(&db, &owned, 1, &serde_json::from_value(b.clone()).unwrap()).unwrap(),
            0.0
        );
    }
    let membership = call(
        &app,
        "GET",
        "/api/profiles/1/library/membership",
        Value::Null,
    )
    .await;
    assert_eq!(membership["identities"], json!([]));
    assert_eq!(membership["scoped_identities"].as_array().unwrap().len(), 2);
    assert_eq!(membership["legacy_identities"].as_array().unwrap().len(), 1);
    let library = call(&app, "GET", "/api/profiles/1/library", Value::Null).await;
    let entries = library["cards"]
        .as_array()
        .or_else(|| library["entries"].as_array());
    assert_eq!(entries.unwrap().len(), 3, "{library}");
    let statistics = library["statistics"].as_array().unwrap();
    let a_stats = statistics
        .iter()
        .find(|s| s["metadata_origin"] == a["metadata_origin"])
        .unwrap();
    let b_stats = statistics
        .iter()
        .find(|s| s["metadata_origin"] == b["metadata_origin"])
        .unwrap();
    assert_eq!(a_stats["watched"], false);
    assert_eq!(b_stats["watched"], true);
    assert_eq!(b_stats["times_watched"], 1);
    let history = call(&app, "GET", "/api/profiles/1/history", Value::Null).await;
    assert_eq!(history.as_array().unwrap().len(), 3);
    let ah = history
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["card"]["metadata_origin"] == a["metadata_origin"])
        .unwrap();
    assert_eq!(ah["card"]["progress"], 0.2);
    assert_eq!(ah["card"]["title"], a["title"]);
    let queue = call(&app, "GET", "/api/profiles/1/continue", Value::Null).await;
    assert_eq!(queue["entries"].as_array().unwrap().len(), 2, "{queue}");
    let bootstrap = call(&app, "GET", "/api/bootstrap", Value::Null).await;
    let rows = bootstrap["home"]["rows"].as_array().unwrap();
    let favorites = rows.iter().find(|r| r["id"] == "favorites").unwrap();
    assert_eq!(
        favorites["cards"].as_array().unwrap().len(),
        3,
        "{bootstrap}"
    );
    let continue_row = rows.iter().find(|r| r["id"] == "continue").unwrap();
    assert_eq!(
        continue_row["cards"].as_array().unwrap().len(),
        2,
        "{bootstrap}"
    );
    let removed = call(&app, "DELETE", &removal(&a), Value::Null).await;
    assert_eq!(
        removed["library_membership"]["scoped_identities"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        removed["library_membership"]["legacy_identities"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    task.abort();
}
#[tokio::test]
async fn verified_aliases_coalesce_but_remapped_opaque_lookup_retains_both_bindings() {
    let (app, task, changed) = stack().await;
    let a = detail(&app, 101, "shared-a").await;
    let b = detail(&app, 102, "shared-b").await;
    save(&app, &a).await;
    let receipt = save(&app, &b).await;
    assert_eq!(
        receipt["library_membership"]["identities"][0]["id"],
        "tt123"
    );
    assert_eq!(
        receipt["library_membership"]["scoped_identities"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    changed.store(true, Ordering::SeqCst);
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE addons SET name='Changed' WHERE id=101", [])
        .unwrap();
    let newer = detail(&app, 101, "shared-a").await;
    assert_ne!(a["identity"]["ids"], newer["identity"]["ids"]);
    let receipt = save(&app, &newer).await;
    assert_eq!(
        receipt["library_membership"]["identities"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        receipt["library_membership"]["scoped_identities"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let receipt = call(&app, "DELETE", &removal(&a), Value::Null).await;
    assert_eq!(
        receipt["library_membership"]["identities"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        receipt["library_membership"]["identities"][0]["id"],
        "tt456"
    );
    task.abort();
}
#[tokio::test]
async fn stale_install_selector_cannot_delete_legacy_or_reinstalled_private_record() {
    let (app, task, _) = stack().await;
    let a = detail(&app, 101, "same").await;
    save(&app, &a).await;
    let mut legacy = a.clone();
    legacy.as_object_mut().unwrap().remove("metadata_origin");
    save(&app, &legacy).await;
    {
        let db = app.db.lock().unwrap();
        let secret: String = db
            .query_row(
                "SELECT secret FROM addon_credentials_v2 WHERE addon_id=101",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let payload = app
            .secret_vault
            .as_ref()
            .unwrap()
            .open_addon(1, "101", &secret)
            .unwrap();
        let payload: Value = serde_json::from_slice(payload.expose()).unwrap();
        let row = (
            "Scoped fixture",
            payload["url"].as_str().unwrap().to_owned(),
            payload["manifest"].to_string(),
        );
        db.execute("DELETE FROM addons WHERE id=101", []).unwrap();
        db.execute(
            "INSERT INTO addons(id,name,manifest_url,manifest,account_id)VALUES(101,?1,?2,?3,1)",
            params![row.0, row.1, row.2],
        )
        .unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&app);
    let new = detail(&app, 101, "same").await;
    assert_ne!(a["metadata_origin"], new["metadata_origin"]);
    save(&app, &new).await;
    let receipt = call(&app, "DELETE", &removal(&a), Value::Null).await;
    assert_eq!(
        receipt["library_membership"]["scoped_identities"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        receipt["library_membership"]["scoped_identities"][0]["metadata_origin"],
        new["metadata_origin"]
    );
    assert_eq!(
        receipt["library_membership"]["legacy_identities"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    call(&app, "DELETE", &removal(&a), Value::Null).await;
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM favorites WHERE profile_id=1 AND id='same'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
    task.abort();
}
#[tokio::test]
async fn progress_timestamps_counts_and_profile_scope_are_server_owned() {
    let (app, task, _) = stack().await;
    let a = detail(&app, 101, "same").await;
    save(&app, &a).await;
    for pos in [20000, 99000, 99000] {
        progress(&app, &a, pos).await;
    }
    let history = call(&app, "GET", "/api/profiles/1/history", Value::Null).await;
    assert!(history[0]["updated_at_millis"].as_u64().unwrap() > 1000);
    let db = app.db.lock().unwrap();
    let raw: String = db
        .query_row(
            "SELECT value FROM engine_scoped_records WHERE profile_id=1 AND record_kind='playback'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let record: PlaybackRecord = decode(&raw).unwrap();
    assert_eq!(record.times_watched, Some(1));
    drop(db);
    assert_eq!(
        request(&app, "member-token-2", "DELETE", &removal(&a), Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    task.abort();
}
#[tokio::test]
async fn unverified_catalog_assertions_remain_private_and_cannot_rewrite_old_constraints() {
    let (app, task, _) = stack().await;
    let origin = origins::for_provider_db(&app.db.lock().unwrap(), 1, "addon:103")
        .unwrap()
        .unwrap();
    let card = json!({"identity":{"id":"catalog-only","kind":"movie","ids":[{"namespace":"imdb","value":"tt123"}]},"metadata_origin":origin,"provider_id":"addon:103","title":"Catalog title","badges":[]});
    let receipt = save(&app, &card).await;
    assert!(receipt["library_membership"]["identities"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        receipt["library_membership"]["scoped_identities"][0]["identity"]["ids"][0]["value"],
        "tt123"
    );
    let mut changed = card.clone();
    changed["identity"]["ids"][0]["value"] = json!("tt456");
    let (status, _) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/library",
        json!({"card":changed}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let membership = call(
        &app,
        "GET",
        "/api/profiles/1/library/membership",
        Value::Null,
    )
    .await;
    assert_eq!(
        membership["scoped_identities"][0]["identity"]["ids"][0]["value"],
        "tt123"
    );
    task.abort();
}
#[tokio::test]
async fn proof_configuration_change_between_fetch_and_commit_cannot_authorize_alias() {
    let (app, task, _) = stack().await;
    let card: Card = serde_json::from_value(detail(&app, 101, "shared-a").await).unwrap();
    let principal = auth::Principal::Account {
        account_id: 1,
        role: "owner".into(),
        profile_id: Some(1),
        session_id: Some("s1".into()),
    };
    let owned = app.clone().with_lease(ResourceLease {
        policy_revision: 0,
        principal,
        session_id: Some("s1".into()),
    });
    let evidence = origins::persistence_card(&owned, &card).await.unwrap();
    let mut db = app.db.lock().unwrap();
    db.execute(
        "UPDATE addons SET manifest=json_set(manifest,'$.description','Rotated after fetch') WHERE id=101",
        [],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    assert_eq!(
        save_db(&tx, &owned, 1, &evidence).unwrap_err().0,
        StatusCode::CONFLICT
    );
    drop(tx);
    assert_eq!(count_db(&db, 1, "favorite").unwrap(), 0);
    drop(db);
    task.abort();
}
#[tokio::test]
async fn malformed_or_origin_only_removal_never_falls_back_to_old_raw_id() {
    let (app, task, _) = stack().await;
    let a = detail(&app, 101, "same").await;
    let mut legacy = a.clone();
    legacy.as_object_mut().unwrap().remove("metadata_origin");
    save(&app, &legacy).await;
    let mut q = url::form_urlencoded::Serializer::new(String::new());
    q.append_pair("metadata_origin", &a["metadata_origin"].to_string());
    let (status, _) = request(
        &app,
        "member-token-1",
        "DELETE",
        &format!("/api/profiles/1/library/movie/same?{}", q.finish()),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let receipt = call(&app, "DELETE", &removal(&a), Value::Null).await;
    assert_eq!(
        receipt["library_membership"]["legacy_identities"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    task.abort();
}

#[tokio::test]
async fn legacy_shared_unknown_dates_and_counts_stay_unknown_on_new_origin_write() {
    let (app, task, _) = stack().await;
    app.db.lock().unwrap().execute("INSERT INTO favorites(profile_id,id,type,name)VALUES(1,'tt123','movie','Old canonical')",[]).unwrap();
    app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at)VALUES(1,'tt123','movie','Old canonical',20,100,1)",[]).unwrap();
    let a = detail(&app, 101, "shared-a").await;
    save(&app, &a).await;
    progress(&app, &a, 99000).await;
    let db = app.db.lock().unwrap();
    let (added,raw):(Option<u64>,String)=db.query_row("SELECT added_at_millis,value FROM engine_scoped_records WHERE profile_id=1 AND record_kind='favorite'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(added, None);
    let _: Card = decode(&raw).unwrap();
    let raw: String = db
        .query_row(
            "SELECT value FROM engine_scoped_records WHERE profile_id=1 AND record_kind='playback'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(decode::<PlaybackRecord>(&raw).unwrap().times_watched, None);
    drop(db);
    task.abort();
}
#[tokio::test]
async fn scoped_queue_visibility_cannot_hide_colliding_legacy_or_other_addon() {
    let (app, task, _) = stack().await;
    let a = detail(&app, 101, "same").await;
    let b = detail(&app, 102, "same").await;
    let mut legacy = a.clone();
    legacy.as_object_mut().unwrap().remove("metadata_origin");
    for c in [&a, &b, &legacy] {
        progress(&app, c, 20000).await;
    }
    let input = json!({"identity":a["identity"],"parent_identity":a["parent_identity"],"metadata_origin":a["metadata_origin"],"card":a,"hidden":true});
    call(
        &app,
        "PUT",
        "/api/profiles/1/continue/visibility",
        input.clone(),
    )
    .await;
    let queue = call(&app, "GET", "/api/profiles/1/continue", Value::Null).await;
    assert_eq!(queue["entries"].as_array().unwrap().len(), 2, "{queue}");
    assert!(queue["entries"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["card"]["metadata_origin"] != a["metadata_origin"]));
    let mut show = input;
    show["hidden"] = json!(false);
    call(&app, "PUT", "/api/profiles/1/continue/visibility", show).await;
    let queue = call(&app, "GET", "/api/profiles/1/continue", Value::Null).await;
    assert_eq!(queue["entries"].as_array().unwrap().len(), 3);
    task.abort();
}

#[tokio::test]
async fn legacy_canonical_and_verified_private_progress_share_one_continue_group() {
    let (app, task, _) = stack().await;
    app.db.lock().unwrap().execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at)VALUES(1,'tt123','movie','Legacy canonical',20,100,1)",[]).unwrap();
    let a = detail(&app, 101, "shared-a").await;
    progress(&app, &a, 30000).await;
    let queue = call(&app, "GET", "/api/profiles/1/continue", Value::Null).await;
    assert_eq!(queue["entries"].as_array().unwrap().len(), 1, "{queue}");
    assert_eq!(queue["entries"][0]["card"]["identity"]["id"], "shared-a");
    assert_eq!(queue["entries"][0]["card"]["progress"], 0.3);
    let bootstrap = call(&app, "GET", "/api/bootstrap", Value::Null).await;
    let rows = bootstrap["home"]["rows"].as_array().unwrap();
    let continue_row = rows.iter().find(|r| r["id"] == "continue").unwrap();
    assert_eq!(
        continue_row["cards"].as_array().unwrap().len(),
        1,
        "{bootstrap}"
    );
    assert_eq!(continue_row["cards"][0]["identity"]["id"], "shared-a");
    task.abort();
}
#[test]
fn intrinsic_canonical_raw_namespaces_do_not_promote_unverified_external_assertions() {
    let origin =
        json!({"provider_id":"stremio","connection_id":"addon:101","incarnation":"a".repeat(32)});
    let mut card:Card=serde_json::from_value(json!({"identity":{"id":"private","kind":"movie","ids":[{"namespace":"imdb","value":"tt123"}]},"provider_id":"addon:101","metadata_origin":origin,"title":"Fixture","badges":[]})).unwrap();
    assert_ne!(raw_key(&card).unwrap().0, "shared");
    card.identity.id = "tt123".into();
    assert_eq!(raw_key(&card).unwrap().0, "shared");
    card.identity.id = "simkl:movie:123".into();
    assert_ne!(raw_key(&card).unwrap().0, "shared");
    card.metadata_origin = Some(
        serde_json::from_value(
            json!({"provider_id":"simkl","connection_id":null,"incarnation":null}),
        )
        .unwrap(),
    );
    assert_eq!(raw_key(&card).unwrap().0, "shared");
}

#[tokio::test]
async fn losing_verified_alias_does_not_copy_shared_state_into_new_private_lookup() {
    let (app, task, changed) = stack().await;
    let a = detail(&app, 101, "shared-drop").await;
    save(&app, &a).await;
    progress(&app, &a, 99000).await;
    changed.store(true, Ordering::SeqCst);
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE addons SET name='Metadata changed' WHERE id=101", [])
        .unwrap();
    let private = detail(&app, 101, "shared-drop").await;
    assert!(
        private["identity"]["ids"].as_array().unwrap().is_empty(),
        "{private}"
    );
    save(&app, &private).await;
    progress(&app, &private, 20000).await;
    progress(&app, &private, 20000).await;
    let db = app.db.lock().unwrap();
    let mut q=db.prepare("SELECT identity_scope,value FROM engine_scoped_records WHERE profile_id=1 AND record_kind='playback' ORDER BY identity_scope").unwrap();
    let records = q
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(records.len(), 2);
    for (scope, raw) in records {
        let record: PlaybackRecord = decode(&raw).unwrap();
        if scope == "shared" {
            assert_eq!(record.times_watched, Some(1));
            assert_eq!(record.position_millis, 99000)
        } else {
            assert_eq!(record.times_watched, Some(0));
            assert_eq!(record.position_millis, 20000)
        }
    }
    drop(q);
    drop(db);
    task.abort();
}

#[tokio::test]
async fn membership_reads_identity_columns_without_deserializing_full_card_payloads() {
    let (app, task, _) = stack().await;
    let a = detail(&app, 101, "shared-a").await;
    let b = detail(&app, 102, "shared-b").await;
    save(&app, &a).await;
    let before = save(&app, &b).await["library_membership"].clone();
    {
        let db = app.db.lock().unwrap();
        db.execute("UPDATE engine_scoped_records SET value='{}' WHERE profile_id=1 AND record_kind='favorite'",[]).unwrap();
        db.execute(
            "UPDATE engine_verified_lookup_bindings SET card='{}' WHERE profile_id=1",
            [],
        )
        .unwrap();
    }
    let after = call(
        &app,
        "GET",
        "/api/profiles/1/library/membership",
        Value::Null,
    )
    .await;
    assert_eq!(after["identities"], before["identities"]);
    assert_eq!(after["scoped_identities"], before["scoped_identities"]);
    assert_eq!(after["complete"], true);
    assert_eq!(after["scoped_identities"].as_array().unwrap().len(), 2);
    task.abort();
}

#[tokio::test]
async fn ids_only_membership_still_filters_each_origin_by_current_parental_policy() {
    let (app, task, _) = stack().await;
    let a = detail(&app, 101, "same").await;
    let b = detail(&app, 102, "same").await;
    save(&app, &a).await;
    save(&app, &b).await;
    call(&app,"POST","/api/profiles/1/approvals",json!({"type":"movie","id":"same","approved":true,"metadata_origin":a["metadata_origin"],"card":a})).await;
    {
        let db = app.db.lock().unwrap();
        db.execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision)VALUES(1,1,7,1) ON CONFLICT(profile_id) DO UPDATE SET enabled=1,max_age=7,revision=kids_profiles.revision+1",
            [],
        )
        .unwrap();
        db.execute("UPDATE engine_scoped_records SET value='{}' WHERE profile_id=1 AND record_kind='favorite'",[]).unwrap();
    }
    let membership = call(
        &app,
        "GET",
        "/api/profiles/1/library/membership",
        Value::Null,
    )
    .await;
    assert_eq!(
        membership["scoped_identities"].as_array().unwrap().len(),
        1,
        "{membership}"
    );
    assert_eq!(
        membership["scoped_identities"][0]["metadata_origin"],
        a["metadata_origin"]
    );
    assert_eq!(membership["complete"], true);
    task.abort();
}

#[tokio::test]
async fn verified_shared_local_watch_preserves_import_rating_totals_and_provenance_only_for_that_key(
) {
    use viptv_engine::library::{LibraryProvenance, LibraryStatistics};
    let (app, task, _) = stack().await;
    let a = detail(&app, 101, "shared-a").await;
    let mut canonical: Card = serde_json::from_value(a.clone()).unwrap();
    canonical.identity.id = "tt123".into();
    canonical.metadata_origin = None;
    let mut imported = LibraryStatistics::empty(canonical.identity.clone());
    imported.user_rating = Some(8.0);
    imported.total_episode_count = Some(24);
    imported.aired_episode_count = Some(20);
    imported.watched = true;
    imported.last_watched_millis = Some(1);
    imported.times_watched = None;
    imported.provenance = vec![LibraryProvenance {
        provider_id: "simkl".into(),
        external_identity: serde_json::from_value(
            json!({"id":"simkl:movie:10","kind":"movie","ids":[]}),
        )
        .unwrap(),
        imported_at_millis: 1,
        source_updated_at_millis: Some(1),
        link_revision: Some(1),
    }];
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO favorites(profile_id,id,type,name)VALUES(1,'tt123','movie','Canonical legacy')",[]).unwrap();
        db.execute("INSERT INTO simkl_import_entries VALUES(1,'simkl:movie:10','movie','tt123','movie',?1,?2,1)",params![encode(&canonical).unwrap(),encode(&imported).unwrap()]).unwrap();
    }
    save(&app, &a).await;
    let mut legacy = a.clone();
    legacy.as_object_mut().unwrap().remove("metadata_origin");
    save(&app, &legacy).await;
    let source_only = origins::for_provider_db(&app.db.lock().unwrap(), 1, "addon:103")
        .unwrap()
        .unwrap();
    let mut private = a.clone();
    private["metadata_origin"] = serde_json::to_value(source_only).unwrap();
    private["provider_id"] = json!("addon:103");
    save(&app, &private).await;
    for position in [20000, 99000] {
        progress(&app, &a, position).await;
        progress(&app, &private, 20000).await;
        let library = call(&app, "GET", "/api/profiles/1/library", Value::Null).await;
        let statistics = library["statistics"].as_array().unwrap();
        let actual = statistics
            .iter()
            .find(|s| s["metadata_origin"] == a["metadata_origin"])
            .unwrap();
        assert_eq!(actual["identity"], a["identity"]);
        assert_eq!(actual["user_rating"], 8.0);
        assert_eq!(actual["total_episode_count"], 24);
        assert_eq!(actual["aired_episode_count"], 20);
        assert_eq!(actual["provenance"].as_array().unwrap().len(), 1);
        assert!(actual["times_watched"].is_null());
        assert_eq!(actual["watched"], position == 99000);
        assert!(actual["last_watched_millis"].as_u64().unwrap() > 1000);
        for other in statistics
            .iter()
            .filter(|s| s["metadata_origin"] != a["metadata_origin"])
        {
            assert!(other["user_rating"].is_null(), "{other}");
            assert!(other["total_episode_count"].is_null());
            assert!(other["provenance"].as_array().unwrap().is_empty());
        }
    }
    task.abort();
}

#[tokio::test]
async fn canonical_unsave_removes_shared_backing_but_preserves_private_legacy_collision() {
    let (app, server, _) = stack().await;
    {
        let db = app.db.lock().unwrap();
        db.execute("INSERT INTO favorites(profile_id,id,type,name,context)VALUES(1,'tt123','movie','Canonical','{}'),(1,'shared-unsave','movie','Ambiguous legacy','{}')",[]).unwrap();
    }
    let card = detail(&app, 101, "shared-unsave").await;
    save(&app, &card).await;
    let library = call(&app, "GET", "/api/profiles/1/library", Value::Null).await;
    assert_eq!(library["cards"].as_array().unwrap().len(), 2);
    let removed = call(&app, "DELETE", &removal(&card), Value::Null).await;
    assert!(removed["library_membership"]["identities"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(removed["library_membership"]
        .get("scoped_identities")
        .is_none_or(|v| v.as_array().unwrap().is_empty()));
    assert_eq!(
        removed["library_membership"]["legacy_identities"][0]["id"],
        "shared-unsave"
    );
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*)FROM favorites WHERE profile_id=1 AND id='tt123'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*)FROM favorites WHERE profile_id=1 AND id='shared-unsave'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    app.db.lock().unwrap().execute("INSERT INTO favorites(profile_id,id,type,name,context)VALUES(1,'tt456','movie','Canonical only','{}')",[]).unwrap();
    let public = detail(&app, 101, "tt456").await;
    call(&app, "DELETE", &removal(&public), Value::Null).await;
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*)FROM favorites WHERE profile_id=1 AND id='tt456'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    server.abort();
}

fn kids_profiles_reads(db: &Connection, f: impl FnOnce()) -> usize {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let counted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = counted.clone();
    db.authorizer(Some(move |ctx: AuthContext<'_>| {
        if matches!(
            ctx.action,
            AuthAction::Read {
                table_name: "kids_profiles",
                ..
            }
        ) {
            seen.fetch_add(1, Ordering::Relaxed);
        }
        Authorization::Allow
    }));
    f();
    db.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    counted.load(Ordering::Relaxed)
}

fn old_membership(
    db: &Connection,
    p: &auth::Principal,
    profile: i64,
) -> LibraryMembershipSnapshot {
    use viptv_engine::{identity::MediaIdentity, origin::MetadataOrigin};
    let mut result = LibraryMembershipSnapshot {
        identities: vec![],
        revision: 0,
        complete: false,
        scoped_identities: vec![],
        legacy_identities: vec![],
    };
    let mut q = db
        .prepare(
            "SELECT identity_scope,kind,identity_id,lookup_origin,lookup_identity,json_extract(value,'$.parent_identity') FROM engine_scoped_records WHERE profile_id=?1 AND record_kind='favorite' ORDER BY identity_scope,kind,identity_id LIMIT 10001",
        )
        .unwrap();
    for row in q
        .query_map([profile], |r| {
            Ok((
                Key(r.get(0)?, r.get(1)?, r.get(2)?),
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })
        .unwrap()
    {
        let (key, raw_origin, raw_identity, raw_parent) = row.unwrap();
        let origin: MetadataOrigin = decode(&raw_origin).unwrap();
        let identity: MediaIdentity = decode(&raw_identity).unwrap();
        let parent: Option<MediaIdentity> = raw_parent.as_deref().map(decode).transpose().unwrap();
        if kids::scoped::require_identity(
            db,
            p,
            kids::restricted(db, p).unwrap(),
            &origin,
            &identity,
            parent.as_ref(),
        )
        .is_err()
        {
            continue;
        }
        if key.0 == "shared" {
            let mut shared = identity.clone();
            shared.id = key.2.clone();
            shared.kind = key.1.clone();
            shared.ids.clear();
            result.identities.push(shared);
        }
        result.scoped_identities.push(ScopedMediaIdentity {
            metadata_origin: origin,
            identity,
        });
    }
    result
        .identities
        .sort_by(|a, b| (&a.kind, &a.id).cmp(&(&b.kind, &b.id)));
    result
        .identities
        .dedup_by(|a, b| a.kind == b.kind && a.id == b.id);
    result
}

#[tokio::test]
async fn non_restricted_reads_children_restriction_once_across_large_rows() {
    let app = fixture();
    let owned = profile_app(&app);
    let origin_raw = json!({"provider_id":"simkl","connection_id":null,"incarnation":null}).to_string();
    {
        let db = app.db.lock().unwrap();
        for i in 0..500 {
            let id = format!("fav-{i}");
            let identity = json!({"id": id.clone(), "kind": "movie", "ids": [], "episode": null, "edition": null});
            let card = json!({
                "identity": identity,
                "provider_id": "simkl",
                "title": format!("Fixture {i}"),
                "subtitle": null,
                "description": null,
                "poster": null,
                "landscape": null,
                "logo": null,
                "progress": null,
                "badges": [],
                "parent_identity": null,
                "metadata_origin": json!({"provider_id":"simkl","connection_id":null,"incarnation":null}),
            });
            db.execute(
                "INSERT INTO engine_scoped_records(profile_id,record_kind,identity_scope,kind,identity_id,lookup_origin,lookup_id,lookup_identity,value,added_at_millis,updated_at_millis,verified) VALUES(?1,'favorite','shared','movie',?2,?3,?2,?4,?5,?6,1,0)",
                params![1, id, origin_raw.as_str(), identity.to_string(), card.to_string(), i as u64],
            )
            .unwrap();
        }
        for i in 0..5000 {
            let id = format!("pb-{i}");
            let identity = json!({"id": id.clone(), "kind": "movie", "ids": [], "episode": null, "edition": null});
            let value = json!({
                "identity": identity,
                "provider_id": "simkl",
                "title": format!("Progress {i}"),
                "poster": null,
                "landscape": null,
                "position_millis": 1000,
                "duration_millis": 5000,
                "updated_at_millis": 1,
                "completed_at_millis": null,
            });
            db.execute(
                "INSERT INTO engine_scoped_records(profile_id,record_kind,identity_scope,kind,identity_id,lookup_origin,lookup_id,lookup_identity,value,added_at_millis,updated_at_millis,verified) VALUES(?1,'playback','shared','movie',?2,?3,?2,?4,?5,NULL,1,0)",
                params![1, id, origin_raw.as_str(), identity.to_string(), value.to_string()],
            )
            .unwrap();
        }
    }

    let db = app.db.lock().unwrap();
    let principal = owned.identity();

    let fav_reads = kids_profiles_reads(&db, || {
        favorites_db(&db, &owned, 1).unwrap();
    });
    assert!(fav_reads <= 2, "favorites re-query kids_profiles {fav_reads} times");

    let playback_reads = kids_profiles_reads(&db, || {
        playback_db(&db, &owned, 1).unwrap();
    });
    assert!(
        playback_reads <= 2,
        "playback re-queries kids_profiles {playback_reads} times"
    );

    let membership_reads = kids_profiles_reads(&db, || {
        let mut snapshot = LibraryMembershipSnapshot {
            identities: vec![],
            revision: 0,
            complete: false,
            scoped_identities: vec![],
            legacy_identities: vec![],
        };
        membership_db(&db, &owned, 1, &mut snapshot, std::collections::BTreeSet::new()).unwrap();
        assert_eq!(snapshot.scoped_identities.len(), 500);
        assert_eq!(snapshot.identities.len(), 500);
    });
    assert!(
        membership_reads <= 2,
        "membership re-queries kids_profiles {membership_reads} times"
    );

    let fav_new: Vec<Key> = favorites_db(&db, &owned, 1)
        .unwrap()
        .into_iter()
        .map(|s| s.key)
        .collect();
    let fav_old_reads = kids_profiles_reads(&db, || {
        rows::<Card>(&db, 1, "favorite", 10000)
            .unwrap()
            .into_iter()
            .filter(|s| kids::scoped::require_card_db(&db, &principal, &s.value).is_ok())
            .count();
    });
    let fav_old: Vec<Key> = rows::<Card>(&db, 1, "favorite", 10000)
        .unwrap()
        .into_iter()
        .filter(|s| kids::scoped::require_card_db(&db, &principal, &s.value).is_ok())
        .map(|s| s.key)
        .collect();
    assert_eq!(fav_new, fav_old);
    assert_eq!(fav_new.len(), 500);
    assert!(fav_old_reads >= 500, "old favorites path reads per row: {fav_old_reads}");

    let playback_new: Vec<Key> = playback_db(&db, &owned, 1)
        .unwrap()
        .into_iter()
        .map(|s| s.key)
        .collect();
    let playback_old_reads = kids_profiles_reads(&db, || {
        rows::<PlaybackRecord>(&db, 1, "playback", 50000)
            .unwrap()
            .into_iter()
            .filter(|s| kids::scoped::require_card_db(&db, &principal, &s.value.card()).is_ok())
            .count();
    });
    let playback_old: Vec<Key> = rows::<PlaybackRecord>(&db, 1, "playback", 50000)
        .unwrap()
        .into_iter()
        .filter(|s| kids::scoped::require_card_db(&db, &principal, &s.value.card()).is_ok())
        .map(|s| s.key)
        .collect();
    assert_eq!(playback_new, playback_old);
    assert_eq!(playback_new.len(), 5000);
    assert!(
        playback_old_reads >= 5000,
        "old playback path reads per row: {playback_old_reads}"
    );

    let mut membership_new = LibraryMembershipSnapshot {
        identities: vec![],
        revision: 0,
        complete: false,
        scoped_identities: vec![],
        legacy_identities: vec![],
    };
    membership_db(&db, &owned, 1, &mut membership_new, std::collections::BTreeSet::new()).unwrap();
    assert_eq!(
        encode(&membership_new).unwrap(),
        encode(&old_membership(&db, &principal, 1)).unwrap()
    );
}

fn table_reads(db: &Connection, table: &str, f: impl FnOnce()) -> usize {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let counted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = counted.clone();
    let wanted = table.to_owned();
    db.authorizer(Some(move |ctx: AuthContext<'_>| {
        if let AuthAction::Read { table_name, .. } = ctx.action {
            if table_name == wanted.as_str() {
                seen.fetch_add(1, Ordering::Relaxed);
            }
        }
        Authorization::Allow
    }));
    f();
    db.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    counted.load(Ordering::Relaxed)
}

fn statement_steps(db: &Connection, sql_contains: &str, f: impl FnOnce()) -> i32 {
    use std::ffi::CStr;
    use std::os::raw::{c_char, c_int, c_uint, c_void};

    struct Measured {
        needle: String,
        steps: i32,
    }

    unsafe extern "C" fn profile(
        mask: c_uint,
        ctx: *mut c_void,
        stmt: *mut c_void,
        sql: *mut c_void,
    ) -> c_int {
        if mask != rusqlite::ffi::SQLITE_TRACE_PROFILE as c_uint {
            return 0;
        }
        let measured = &mut *(ctx as *mut Measured);
        let text = CStr::from_ptr(sql as *const c_char).to_string_lossy();
        if text.contains(measured.needle.as_str()) {
            measured.steps = rusqlite::ffi::sqlite3_stmt_status(
                stmt as *mut rusqlite::ffi::sqlite3_stmt,
                rusqlite::ffi::SQLITE_STMTSTATUS_RUN as c_int,
                0,
            );
        }
        0
    }

    let mut measured = Measured {
        needle: sql_contains.to_owned(),
        steps: 0,
    };
    let handle = unsafe { db.handle() };
    unsafe {
        rusqlite::ffi::sqlite3_trace_v2(
            handle,
            rusqlite::ffi::SQLITE_TRACE_PROFILE as c_uint,
            Some(profile),
            &mut measured as *mut Measured as *mut c_void,
        );
    }
    f();
    unsafe {
        rusqlite::ffi::sqlite3_trace_v2(handle, 0, None, std::ptr::null_mut());
    }
    measured.steps
}

fn old_continuation_keys(db: &Connection, profile: i64) -> Vec<Key> {
    let mut groups: std::collections::BTreeMap<Key, Saved<PlaybackRecord>> = Default::default();
    for saved in rows::<PlaybackRecord>(db, profile, "playback", 50000).unwrap() {
        let key = parent_key(&saved)
            .unwrap()
            .unwrap_or_else(|| saved.key.clone());
        if groups.get(&key).is_none_or(|old| old.updated < saved.updated) {
            groups.insert(key, saved);
        }
    }
    let mut keys = Vec::new();
    for (key, _saved) in groups {
        let hidden: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM engine_scoped_queue_hidden WHERE profile_id=?1 AND identity_scope=?2 AND kind=?3 AND identity_id=?4)",
                params![profile, key.0, key.1, key.2],
                |r| r.get(0),
            )
            .unwrap();
        if hidden {
            continue;
        }
        keys.push(key);
    }
    keys
}

#[tokio::test]
async fn continue_watching_reads_hidden_queue_once_across_large_rows() {
    let app = fixture();
    let owned = profile_app(&app);
    let origin_raw = json!({"provider_id":"simkl","connection_id":null,"incarnation":null}).to_string();
    {
        let db = app.db.lock().unwrap();
        for i in 0..500 {
            let id = format!("pb-{i}");
            let identity = json!({"id": id.clone(), "kind": "movie", "ids": [], "episode": null, "edition": null});
            let value = json!({
                "identity": identity,
                "provider_id": "simkl",
                "title": format!("Progress {i}"),
                "poster": null,
                "landscape": null,
                "position_millis": 1000,
                "duration_millis": 5000,
                "updated_at_millis": 1,
                "completed_at_millis": null,
            });
            db.execute(
                "INSERT INTO engine_scoped_records(profile_id,record_kind,identity_scope,kind,identity_id,lookup_origin,lookup_id,lookup_identity,value,added_at_millis,updated_at_millis,verified) VALUES(?1,'playback','shared','movie',?2,?3,?2,?4,?5,NULL,1,0)",
                params![1, id, origin_raw.as_str(), identity.to_string(), value.to_string()],
            )
            .unwrap();
        }
        for i in 0..10 {
            db.execute(
                "INSERT INTO engine_scoped_queue_hidden(profile_id,identity_scope,kind,identity_id) VALUES(1,'shared','movie',?1)",
                [format!("pb-{i}")],
            )
            .unwrap();
        }
    }

    let db = app.db.lock().unwrap();
    let new_keys: Vec<Key> = continuation_db(&db, &owned, 1)
        .unwrap()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let new_reads = table_reads(&db, "engine_scoped_queue_hidden", || {
        continuation_db(&db, &owned, 1).unwrap();
    });
    let old_reads = table_reads(&db, "engine_scoped_queue_hidden", || {
        old_continuation_keys(&db, 1);
    });
    let old_keys = old_continuation_keys(&db, 1);
    assert_eq!(new_keys, old_keys);
    assert_eq!(new_keys.len(), 490);
    assert!(
        new_reads <= 4,
        "continue watching re-queries the hidden queue {new_reads} times"
    );
    assert!(
        old_reads >= 500,
        "old continue path reads the hidden queue per entry: {old_reads}"
    );
}

fn old_home_favorites(db: &Connection, app: &App, profile: i64) -> Vec<Card> {
    app.require_profile(db, profile).unwrap();
    app.request_lease().validate(db).unwrap();
    let allowed = kids::sql_allowed("favorites.type", "favorites.id", "?1");
    let mut q = db
        .prepare(&format!(
            "SELECT id,type,name,poster,context,added_at_millis FROM favorites WHERE profile_id=?1 AND {allowed} ORDER BY added_at_millis DESC,name COLLATE NOCASE,type,id LIMIT 12"
        ))
        .unwrap();
    let items = q
        .query_map([profile], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "type": r.get::<_, String>(1)?,
                "name": r.get::<_, String>(2)?,
                "poster": r.get::<_, Option<String>>(3)?,
                "engine_favorite": serde_json::from_str::<Value>(&r.get::<_, String>(4)?)
                    .unwrap_or(Value::Null),
                "added_at_millis": r.get::<_, Option<u64>>(5)?,
            }))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut cards: std::collections::BTreeMap<Key, Card> =
        super::super::library::cards(db, &app.identity(), items)
            .unwrap()
            .into_iter()
            .map(|c| Ok((raw_key(&c).unwrap(), c)))
            .collect::<Result<_, ApiError>>()
            .unwrap();
    let mut accepted = 0;
    let mut offset = 0;
    while accepted < 12 && offset <= 10000 {
        let mut q = db
            .prepare(
                "SELECT identity_scope,kind,identity_id,value,added_at_millis FROM engine_scoped_records WHERE profile_id=?1 AND record_kind='favorite' ORDER BY added_at_millis DESC,json_extract(value,'$.title') COLLATE NOCASE,kind,identity_id LIMIT 24 OFFSET ?2",
            )
            .unwrap();
        let batch = q
            .query_map(params![profile, offset], |r| {
                Ok((
                    Key(r.get(0)?, r.get(1)?, r.get(2)?),
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<u64>>(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let count = batch.len();
        for (key, raw, added) in batch {
            let mut card: Card = decode(&raw).unwrap();
            if kids::scoped::require_card_db(db, &app.identity(), &card).is_err() {
                continue;
            }
            card.added_at_millis = added;
            card.progress = None;
            cards.insert(key, card);
            accepted += 1;
            if accepted == 12 {
                break;
            }
        }
        if count < 24 {
            break;
        }
        offset += 24;
    }
    let mut cards = cards.into_iter().collect::<Vec<_>>();
    cards.sort_by(|(ka, a), (kb, b)| {
        b.added_at_millis
            .cmp(&a.added_at_millis)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
            .then_with(|| ka.cmp(kb))
    });
    cards.into_iter().take(12).map(|(_, c)| c).collect()
}

#[tokio::test]
async fn home_favorites_reads_engine_records_once_across_large_rows() {
    let app = fixture();
    let owned = profile_app(&app);
    let origin_raw = json!({"provider_id":"simkl","connection_id":null,"incarnation":null}).to_string();
    {
        let db = app.db.lock().unwrap();
        for i in 0..500 {
            let id = format!("fav-{i}");
            let identity = json!({"id": id.clone(), "kind": "movie", "ids": [], "episode": null, "edition": null});
            let card = json!({
                "identity": identity,
                "provider_id": "simkl",
                "title": format!("Fixture {i}"),
                "subtitle": null,
                "description": null,
                "poster": null,
                "landscape": null,
                "logo": null,
                "progress": null,
                "badges": [],
                "parent_identity": null,
                "metadata_origin": json!({"provider_id":"simkl","connection_id":null,"incarnation":null}),
            });
            db.execute(
                "INSERT INTO engine_scoped_records(profile_id,record_kind,identity_scope,kind,identity_id,lookup_origin,lookup_id,lookup_identity,value,added_at_millis,updated_at_millis,verified) VALUES(?1,'favorite','shared','movie',?2,?3,?2,?4,?5,?6,1,0)",
                params![1, id, origin_raw.as_str(), identity.to_string(), card.to_string(), i as u64],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision) VALUES(1,1,7,0) ON CONFLICT(profile_id) DO UPDATE SET enabled=1,max_age=7,revision=kids_profiles.revision+1",
            [],
        )
        .unwrap();
    }

    let db = app.db.lock().unwrap();
    let new_cards = home_favorites_db(&db, &owned, 1).unwrap();
    let old_cards = old_home_favorites(&db, &owned, 1);
    assert_eq!(encode(&new_cards).unwrap(), encode(&old_cards).unwrap());

    let new_reads = table_reads(&db, "engine_scoped_records", || {
        home_favorites_db(&db, &owned, 1).unwrap();
    });
    let old_reads = table_reads(&db, "engine_scoped_records", || {
        old_home_favorites(&db, &owned, 1);
    });
    assert!(
        new_reads <= 12,
        "home favourites re-query engine records {new_reads} times"
    );
    assert!(
        old_reads >= 200,
        "old home favourites path scans engine records per batch: {old_reads}"
    );
    let new_steps = statement_steps(&db, "engine_scoped_records", || {
        home_favorites_db(&db, &owned, 1).unwrap();
    });
    assert!(
        new_steps <= 600,
        "restricted home favourites scan the list but stay bounded: {new_steps} steps"
    );
}

#[tokio::test]
async fn home_favorites_reads_about_twelve_rows_when_unrestricted() {
    let app = fixture();
    let owned = profile_app(&app);
    let origin_raw = json!({"provider_id":"simkl","connection_id":null,"incarnation":null}).to_string();
    {
        let db = app.db.lock().unwrap();
        for i in 0..500 {
            let id = format!("fav-{i}");
            let identity = json!({"id": id.clone(), "kind": "movie", "ids": [], "episode": null, "edition": null});
            let card = json!({
                "identity": identity,
                "provider_id": "simkl",
                "title": format!("Fixture {i}"),
                "subtitle": null,
                "description": null,
                "poster": null,
                "landscape": null,
                "logo": null,
                "progress": null,
                "badges": [],
                "parent_identity": null,
                "metadata_origin": json!({"provider_id":"simkl","connection_id":null,"incarnation":null}),
            });
            db.execute(
                "INSERT INTO engine_scoped_records(profile_id,record_kind,identity_scope,kind,identity_id,lookup_origin,lookup_id,lookup_identity,value,added_at_millis,updated_at_millis,verified) VALUES(?1,'favorite','shared','movie',?2,?3,?2,?4,?5,?6,1,0)",
                params![1, id, origin_raw.as_str(), identity.to_string(), card.to_string(), i as u64],
            )
            .unwrap();
        }
    }

    let db = app.db.lock().unwrap();
    let cards = home_favorites_db(&db, &owned, 1).unwrap();
    assert_eq!(cards.len(), 12);
    let steps = statement_steps(&db, "engine_scoped_records", || {
        home_favorites_db(&db, &owned, 1).unwrap();
    });
    assert!(
        steps <= 16,
        "home favourites read {steps} rows just to produce 12 cards"
    );
}
