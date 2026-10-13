use super::*;

#[tokio::test]
async fn metadata_and_tracking_require_exact_identity_and_current_profile_link() {
    let (app, task, writes) = server().await;
    link(&app).await;
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/simkl/search?category=movie&query=Movie",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cards"][0]["identity"]["id"], "simkl:movie:10");
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/simkl/meta/tv/20",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["episodes"][0]["identity"]["episode"]["episode"], 2);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/v3/profiles/1/simkl/meta/movie/11",
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_GATEWAY
    );
    for input in [
        json!({"category":"tv","id":20}),
        json!({"category":"tv","id":20,"season":1,"episode":1}),
    ] {
        assert_eq!(
            request(
                &app,
                "member-token-1",
                "POST",
                "/api/v3/profiles/1/simkl/watched",
                input
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    let (status, body) = request(
        &app,
        "member-token-1",
        "POST",
        "/api/v3/profiles/1/simkl/watched",
        json!({"category":"tv","id":20,"season":1,"episode":2}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["recorded"], true);
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/v3/profiles/1/simkl",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v3/profiles/1/simkl/watched",
            json!({"category":"movie","id":10})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    task.abort();
}

#[tokio::test]
async fn unlink_invalidates_pending_oauth_and_disabled_assignment_blocks_tracking() {
    let (app, task, writes) = server().await;
    link(&app).await;
    let state = start(&app).await;
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/v3/profiles/1/simkl",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(callback(
        State(app.clone()),
        Query(crate::simkl::oauth::Callback {
            state,
            code: Some("fixture-code".into()),
            error: None,
            iss: Some("https://simkl.com".into())
        })
    )
    .await
    .is_err());
    link(&app).await;
    let (status, body) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/integrations",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["connections"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "simkl:1"));
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "PUT",
            "/api/v3/profiles/1/integrations",
            json!({"revision":0,"browsing_plugin":"stremio","assignments":[]})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "POST",
            "/api/v3/profiles/1/simkl/watched",
            json!({"category":"movie","id":10})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn upstream_metadata_authorization_failure_does_not_expire_viptv_session() {
    let (mut app, initial, _) = server().await;
    initial.abort();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().fallback(|| async { StatusCode::UNAUTHORIZED }),
        )
        .await
        .unwrap();
    });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    let (status, _) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/simkl/meta/movie/10",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let (status, _) = request(
        &app,
        "member-token-1",
        "GET",
        "/api/v3/profiles/1/simkl",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    task.abort();
}

#[tokio::test]
async fn oauth_required_metadata_uses_profile_link_without_populating_public_cache() {
    let (mut app, initial, _) = server().await;
    link(&app).await;
    initial.abort();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let routes = axum::Router::new()
            .route(
                "/movies/10",
                get(|headers: axum::http::HeaderMap| async move {
                    if headers
                        .get(header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        != Some("Bearer fixture-private-token")
                    {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    axum::Json(json!({"title":"Movie","ids":{"simkl":10}})).into_response()
                }),
            )
            .route(
                "/search/movie",
                get(|headers: axum::http::HeaderMap| async move {
                    if headers
                        .get(header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        != Some("Bearer fixture-private-token")
                    {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    axum::Json(json!([{"title":"Movie","ids":{"simkl":10}}])).into_response()
                }),
            );
        axum::serve(listener, routes).await.unwrap();
    });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    for path in [
        "/api/v3/profiles/1/simkl/search?category=movie&query=Movie",
        "/api/v3/profiles/1/simkl/meta/movie/10",
    ] {
        assert_eq!(
            request(&app, "member-token-1", "GET", path, json!({}))
                .await
                .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "DELETE",
            "/api/v3/profiles/1/simkl",
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "member-token-1",
            "GET",
            "/api/v3/profiles/1/simkl/meta/movie/10",
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_GATEWAY
    );
    task.abort();
}

#[tokio::test]
async fn related_titles_and_full_metadata_arrive_in_one_selected_provider_detail_response(){
 let (app,task,_)=server().await;
 let (status,body)=request(&app,"member-token-1","GET","/api/v3/profiles/1/simkl/meta/movie/10",json!({})).await;
 assert_eq!(status,StatusCode::OK,"{body}");assert_eq!(body["item"]["metadata"]["runtime_minutes"],120);assert_eq!(body["item"]["description"],"Full movie detail");assert_eq!(body["recommendations"].as_array().unwrap().len(),1);assert_eq!(body["recommendations"][0]["identity"]["id"],"simkl:movie:12");
 task.abort();
}

#[tokio::test]
async fn all_search_keeps_anime_subtype_and_ranks_naruto_above_unrelated_movie_ovas(){
 let (mut app,initial,_)=server().await;initial.abort();
 let router=Router::new()
 .route("/search/movie",get(||async{axum::Json(json!([{ "title":"Naruto OVA 12","endpoint_type":"movies","ids":{"simkl_id":1}},{"title":"Naruto","endpoint_type":"movies","ids":{"simkl_id":2},"ratings":{"imdb":{"rating":4,"votes":3}}}]))}))
 .route("/search/tv",get(||async{axum::Json(json!([{ "title":"Naruto Abridged","endpoint_type":"tv","ids":{"simkl_id":3}}]))}))
 .route("/search/anime",get(||async{axum::Json(json!([{ "title":"Naruto","endpoint_type":"anime","type":"tv","ids":{"simkl_id":4},"ratings":{"imdb":{"rating":8.4,"votes":150000}}},{"title":"Naruto movie","endpoint_type":"anime","type":"movie","ids":{"simkl_id":5}}]))}));
 let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();let task=tokio::spawn(async move{axum::serve(listener,router).await.unwrap()});
 app.simkl=Arc::new(Service::fixture(&format!("http://{address}")));
 let (status,body)=request(&app,"member-token-1","GET","/api/v3/discover?type=all&search=Naruto",json!({})).await;
 assert_eq!(status,StatusCode::OK,"{body}");assert_eq!(body["cards"].as_array().unwrap().len(),5);assert_eq!(body["cards"][0]["identity"]["id"],"simkl:anime:4");assert_eq!(body["cards"][0]["category"],"anime");assert_eq!(body["cards"][0]["identity"]["kind"],"series");
 task.abort();
}
