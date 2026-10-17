use crate::{auth_integration_tests::fixture, test_support::request, *};
async fn fact(app: &App, token: &str) -> Value {
    let (status, body) = request(app, token, "GET", "/api/bootstrap", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["subtitle_availability"].clone()
}
#[tokio::test]
async fn subtitle_bootstrap_fact_requires_enabled_scoped_assignment_and_performs_no_provider_io() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let app = fixture();
    assert_eq!(
        fact(&app, "member-token-1").await,
        json!({"available":false,"provider_count":0})
    );
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    StatusCode::BAD_GATEWAY
                }
            }),
        )
        .await
        .unwrap()
    });
    {
        let db = app.db.lock().unwrap();
        for (id, resources) in [
            (1, json!(["meta"])),
            (2, json!([{"name":"subtitles","types":["movie"]}])),
        ] {
            db.execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(?1,'Cached fixture',?2,?3,1)",params![id,format!("http://{address}/{id}/manifest.json"),json!({"id":format!("fixture-{id}"),"name":"Cached fixture","version":"1","resources":resources,"types":["movie"]}).to_string()]).unwrap();
        }
        db.execute("INSERT INTO profiles(id,name,avatar_seed,presentation_complete) VALUES(2,'Foreign','foreign',1)",[]).unwrap();
        db.execute(
            "INSERT INTO profile_owners(profile_id,account_id,created_at) VALUES(2,2,0)",
            [],
        )
        .unwrap();
        db.execute("UPDATE auth_sessions SET profile_id=2 WHERE id='s2'", [])
            .unwrap();
    }
    crate::test_support::encrypt_fixture_sources(&app);
    assert_eq!(
        fact(&app, "member-token-1").await,
        json!({"available":true,"provider_count":1})
    );
    assert_eq!(
        fact(&app, "member-token-2").await,
        json!({"available":false,"provider_count":0})
    );
    for (revision, enabled) in [(0, false), (1, true)] {
        let(status,body)=request(&app,"member-token-1","PUT","/api/profiles/1/integrations",json!({"revision":revision,"browsing_plugin":"stremio","assignments":[{"connection_id":"addon:1","enabled":["Metadata"]},{"connection_id":"addon:2","enabled":if enabled{json!(["Subtitles"])}else{json!([])}}]})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            fact(&app, "member-token-1").await,
            json!({"available":enabled,"provider_count":usize::from(enabled)})
        );
    }
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE addons SET enabled=0 WHERE id=2", [])
        .unwrap();
    assert_eq!(
        fact(&app, "member-token-1").await,
        json!({"available":false,"provider_count":0})
    );
    app.db
        .lock()
        .unwrap()
        .execute("UPDATE auth_sessions SET profile_id=NULL WHERE id='s1'", [])
        .unwrap();
    assert!(fact(&app, "member-token-1").await.is_null());
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn bootstrap_negotiates_required_audio_api_without_an_extra_request_or_selected_profile() {
    let app = fixture();
    for profile in [Some(1_i64), None] {
        app.db.lock().unwrap().execute("UPDATE auth_sessions SET profile_id=?1 WHERE id='s1'", [profile]).unwrap();
        let(status,body)=request(&app,"member-token-1","GET","/api/bootstrap",Value::Null).await;
        assert_eq!(status,StatusCode::OK);
        assert_eq!(body["playback_capabilities"],json!({"required_audio_index":true}));
    }
    let(status,body)=request(&app,"invalid-token","GET","/api/bootstrap",Value::Null).await;
    assert_eq!(status,StatusCode::UNAUTHORIZED);
    assert!(body.get("playback_capabilities").is_none());
}
