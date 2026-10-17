use super::*;
use crate::{auth_integration_tests::fixture, test_support::request};

fn legacy() -> App {
    let app = fixture();
    app.db.lock().unwrap().execute_batch("INSERT INTO providers(id,name,url,username,password,enabled,enable_live,enable_movies,enable_series)VALUES(10,'Owned live','https://provider.example','private-user','private-password',1,1,0,0),(20,'Foreign live','https://foreign.example','foreign-user','foreign-password',1,1,0,0);INSERT INTO provider_ownership VALUES(10,1),(20,2);INSERT INTO provider_live(id,provider_id,stream_id,name,category_id,category,ordinal)VALUES('opaque-live',10,'7','News','news','News',1),('foreign-live',20,'8','Foreign','news','News',1);").unwrap();
    crate::test_support::encrypt_fixture_sources(&app);
    app
}
const COLLECTION: &str = "/api/profiles/1/live/favorites";
const OWN: &str = "/api/profiles/1/live/opaque-live/favorite";

#[tokio::test]
async fn live_favorites_are_idempotent_profile_owned_and_separate_from_vod_library() {
    let app = legacy();
    let before: i64 = app
        .db
        .lock()
        .unwrap()
        .query_row("SELECT count(*) FROM favorites", [], |row| row.get(0))
        .unwrap();
    for _ in 0..2 {
        let (status, value) = request(&app, "member-token-1", "PUT", OWN, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value, json!({"channel_ids":["opaque-live"]}));
    }
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM engine_live_favorites", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM favorites", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert_eq!(
        request(&app, "member-token-2", "GET", COLLECTION, json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    for channel in ["foreign-live", "unknown"] {
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "PUT",
                &format!("/api/profiles/1/live/{channel}/favorite"),
                json!({})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    app.db.lock().unwrap().execute_batch("INSERT INTO profiles(id,name,avatar_seed,presentation_complete)VALUES(3,'Other account','fixture-other',1);INSERT INTO profile_owners(profile_id,account_id,created_at)VALUES(3,2,0);UPDATE auth_sessions SET profile_id=3 WHERE id='s2';").unwrap();
    let (status, other) = request(
        &app,
        "member-token-2",
        "GET",
        "/api/profiles/3/live/favorites",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(other, json!({"channel_ids":[]}));
    assert_eq!(
        request(
            &app,
            "member-token-2",
            "PUT",
            "/api/profiles/3/live/opaque-live/favorite",
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, value) = request(&app, "member-token-1", "DELETE", OWN, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, json!({"channel_ids":[]}));
    assert_eq!(
        request(&app, "member-token-1", "DELETE", OWN, json!({}))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn disabled_unassigned_or_removed_channels_are_hidden_but_saved_rows_can_be_removed() {
    let app = legacy();
    assert_eq!(
        request(&app, "member-token-1", "PUT", OWN, json!({}))
            .await
            .0,
        StatusCode::OK
    );
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE providers SET enabled=0 WHERE id=10", [])
        .unwrap();
    assert_eq!(
        request(&app, "member-token-1", "GET", COLLECTION, json!({}))
            .await
            .1,
        json!({"channel_ids":[]})
    );
    assert_eq!(
        request(&app, "member-token-1", "PUT", OWN, json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "member-token-1", "DELETE", OWN, json!({}))
            .await
            .0,
        StatusCode::OK
    );
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE providers SET enabled=1 WHERE id=10", [])
        .unwrap();
    assert_eq!(
        request(&app, "member-token-1", "PUT", OWN, json!({}))
            .await
            .0,
        StatusCode::OK
    );
    app.db
        .lock()
        .unwrap()
        .execute("DELETE FROM provider_live WHERE id='opaque-live'", [])
        .unwrap();
    assert_eq!(
        request(&app, "member-token-1", "GET", COLLECTION, json!({}))
            .await
            .1,
        json!({"channel_ids":[]})
    );
    assert_eq!(
        request(&app, "member-token-1", "DELETE", OWN, json!({}))
            .await
            .0,
        StatusCode::OK
    );
    app.db.lock().unwrap().execute("INSERT INTO provider_live(id,provider_id,stream_id,name,ordinal)VALUES('opaque-live',10,'7','News',1)",[]).unwrap();
    assert_eq!(
        request(&app, "member-token-1", "PUT", OWN, json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/integrations",
            json!({"revision":0,"browsing_plugin":"stremio","assignments":[]})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "member-token-1", "GET", COLLECTION, json!({}))
            .await
            .1,
        json!({"channel_ids":[]})
    );
    assert_eq!(
        request(&app, "member-token-1", "PUT", OWN, json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "member-token-1", "DELETE", OWN, json!({}))
            .await
            .0,
        StatusCode::OK
    );
}

#[cfg(feature = "live-runtime")]
#[tokio::test]
async fn native_live_favorites_require_current_host_registry_and_connection_revision() {
    let app = fixture();
    let credentials = json!({"playlist_url":"https://provider.example/list.m3u?private"});
    let (status, created) = request(
        &app,
        "member-token-1",
        "PUT",
        "/api/profiles/1/source-integrations",
        json!({"revision":0,"plugin_id":"m3u","configuration":credentials}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let connection = created["configuration"]["assignments"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|assignment| {
            assignment["connection_id"]
                .as_str()
                .filter(|id| id.starts_with("m3u:"))
        })
        .unwrap()
        .to_owned();
    let channels = viptv_engine::live::parse_m3u(
        "#EXTM3U\n#EXTINF:-1,Private news\nhttps://media.example/private-channel.m3u8\n",
        &url::Url::parse("https://provider.example/list.m3u").unwrap(),
        &connection,
    )
    .unwrap();
    let id = channels[0].channel.id.clone();
    app.live_cache.lock().await.insert(
        connection.clone(),
        Cache {
            revision: 1,
            expires: Instant::now() - Duration::from_secs(1),
            channels,
            groups: vec![],
            guides: Default::default(),
            xmltv: None,
        },
    );
    let path = format!("/api/profiles/1/live/{id}/favorite");
    let (status, saved) = request(&app, "member-token-1", "PUT", &path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["channel_ids"], json!([id]));
    assert!(!saved.to_string().contains("media.example"));
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/profiles/1/source-integrations",
            json!({"revision":1,"plugin_id":"m3u","configuration":credentials})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "member-token-1", "GET", COLLECTION, json!({}))
            .await
            .1,
        json!({"channel_ids":[]})
    );
    assert_eq!(
        request(&app, "member-token-1", "PUT", &path, json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    app.live_cache
        .lock()
        .await
        .get_mut(&connection)
        .unwrap()
        .revision = 2;
    assert_eq!(
        request(&app, "member-token-1", "GET", COLLECTION, json!({}))
            .await
            .1,
        json!({"channel_ids":[]})
    );
    assert_eq!(
        request(&app, "member-token-1", "PUT", &path, json!({}))
            .await
            .0,
        StatusCode::OK
    );
    app.live_cache
        .lock()
        .await
        .get_mut(&connection)
        .unwrap()
        .channels
        .clear();
    assert_eq!(
        request(&app, "member-token-1", "GET", COLLECTION, json!({}))
            .await
            .1,
        json!({"channel_ids":[]})
    );
    assert_eq!(
        request(&app, "member-token-1", "DELETE", &path, json!({}))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn favorites_cookie_mutations_require_csrf_and_restricted_profiles_require_parent() {
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;
    let app = legacy();
    let response = crate::router(app.clone(), None)
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(OWN)
                .header(header::HOST, "tv.example")
                .header(header::ORIGIN, "https://tv.example")
                .header(header::COOKIE, "viptv_session=member-token-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = crate::router(app.clone(), None)
        .oneshot(
            Request::builder()
                .uri("/api/auth/status")
                .header(header::HOST, "tv.example")
                .header(header::COOKIE, "viptv_session=member-token-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    let csrf = status["csrf_token"].as_str().unwrap();
    let response = crate::router(app.clone(), None)
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(OWN)
                .header(header::HOST, "tv.example")
                .header(header::ORIGIN, "https://tv.example")
                .header(header::COOKIE, "viptv_session=member-token-1")
                .header("x-csrf-token", csrf)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    app.db
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO kids_profiles(profile_id,enabled,max_age,revision)VALUES(1,1,12,1)",
            [],
        )
        .unwrap();
    assert_eq!(
        request(&app, "member-token-1", "GET", COLLECTION, json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "member-token-1", "DELETE", OWN, json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn favorites_are_bounded_without_deleting_existing_preferences() {
    let app = legacy();
    {
        let mut db = app.db.lock().unwrap();
        let tx = db.transaction().unwrap();
        for index in 0..MAX_FAVORITES {
            tx.execute("INSERT INTO engine_live_favorites(profile_id,channel_id,connection_id,connection_revision)VALUES(1,?1,'iptv:10',0)",[format!("retained-{index}")]).unwrap();
        }
        tx.commit().unwrap();
    }
    assert_eq!(
        request(&app, "member-token-1", "PUT", OWN, json!({}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        app.db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM engine_live_favorites", [], |row| {
                row.get::<_, usize>(0)
            })
            .unwrap(),
        MAX_FAVORITES
    );
}
