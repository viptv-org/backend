use super::*;

pub(super) fn media_identity(id: &str, kind: &str, episode: Option<Value>) -> Value {
    json!({"id":id,"kind":kind,"ids":[],"episode":episode,"edition":null})
}
pub(super) fn history_body(id: &str, position: u64, duration: u64) -> Value {
    json!({"record":{"identity":media_identity(id,"movie",None),"provider_id":"simkl","title":"Movie","poster":null,"landscape":null,"position_millis":position,"duration_millis":duration,"updated_at_millis":0,"completed_at_millis":999},"ended":false})
}
pub(super) async fn receipt(app: &App, id: &str, state: &str) {
    tokio::time::timeout(Duration::from_secs(4),async {
        loop {
            let found=app.db.lock().unwrap().query_row("SELECT state FROM simkl_tracking_receipts WHERE profile_id=1 AND media_id=?1 ORDER BY link_revision DESC LIMIT 1",[id],|r|r.get::<_,String>(0)).optional().unwrap();
            if found.as_deref()==Some(state){return;}
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
}

pub(super) async fn tracking_server() -> (App, tokio::task::JoinHandle<()>, Arc<Mutex<Vec<Value>>>)
{
    tracking_server_with_failed_stop(false).await
}

pub(super) async fn tracking_server_with_failed_stop(
    fail_stop: bool,
) -> (App, tokio::task::JoinHandle<()>, Arc<Mutex<Vec<Value>>>) {
    let (mut app, token_server, _) = server().await;
    link(&app).await;
    token_server.abort();
    let writes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let history = writes.clone();
    let scrobble = writes.clone();
    let mock=axum::Router::new()
        .route("/movies/10",get(||async{axum::Json(json!({"title":"Movie","ids":{"simkl":10}}))}))
        .route("/tv/20",get(||async{axum::Json(json!({"title":"Show","ids":{"simkl":20}}))}))
        .route("/tv/episodes/20",get(||async{axum::Json(json!([{"season":1,"episode":2,"ids":{"simkl":30}}]))}))
        .route("/anime/40",get(||async{axum::Json(json!({"title":"Anime","anime_type":"tv","ids":{"simkl":40}}))}))
        .route("/anime/41",get(||async{axum::Json(json!({"title":"Mapped Anime","anime_type":"tv","ids":{"simkl":41,"tvdb":267440,"tmdb":1429}}))}))
        .route("/anime/50",get(||async{axum::Json(json!({"title":"Anime Film","anime_type":"movie","ids":{"simkl":50}}))}))
        .route("/anime/episodes/40",get(||async{axum::Json(json!([
            {"episode":36,"type":"episode","aired":true,"tvdb":{"season":2,"episode":1},"ids":{"simkl_id":60}},
            {"type":"episode","aired":true,"tvdb":{"season":2,"episode":2},"ids":{"simkl_id":61}}
        ]))}))
        .route("/anime/episodes/41",get(||async{axum::Json(json!([
            {"episode":36,"type":"episode","aired":true,"tvdb":{"season":2,"episode":4},"ids":{"simkl_id":62}}
        ]))}))
        .route("/sync/history",post(move|headers:axum::http::HeaderMap,axum::Json(body):axum::Json<Value>|{let history=history.clone();async move{
            assert_eq!(headers[header::AUTHORIZATION],"Bearer fixture-private-token");
            let movie=body["movies"].is_array();let anime_film=body["anime"].is_array()&&body["anime"][0]["seasons"].is_null();
            history.lock().unwrap().push(json!({"path":"history","body":body}));
            axum::Json(if movie{json!({"added":{"movies":1}})}else if anime_film{json!({"added":{"shows":1}})}else{json!({"added":{"episodes":1}})})
        }}))
        .route("/scrobble/:action",post(move|Path(action):Path<String>,headers:axum::http::HeaderMap,axum::Json(body):axum::Json<Value>|{let scrobble=scrobble.clone();async move{
            assert_eq!(headers[header::AUTHORIZATION],"Bearer fixture-private-token");
            scrobble.lock().unwrap().push(json!({"path":action,"body":body}));
            if fail_stop && action=="stop" {return StatusCode::BAD_GATEWAY.into_response();}
            let mut response=json!({"action":if action=="stop"{"scrobble"}else{&action},"movie":body["movie"],"show":body["show"],"anime":body["anime"],"episode":body["episode"]});
            if body["movie"].is_null(){response.as_object_mut().unwrap().remove("movie");}
            axum::Json(response).into_response()
        }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    app.simkl = Arc::new(Service::fixture(&format!("http://{address}")));
    (app, task, writes)
}
