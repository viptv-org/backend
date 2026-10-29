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
            role: "member".into(),
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
        role: "member".into(),
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
