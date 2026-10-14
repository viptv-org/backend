use super::*;
#[tokio::test]
async fn imported_next_episode_uses_verified_provider_target_preserves_canonical_identity_and_never_writes_fake_progress(){
 let(mut app,original,_)=server().await;link(&app).await;original.abort();
 app.db.lock().unwrap().execute("INSERT INTO favorites(profile_id,id,type,name,context) VALUES(1,'tt7654321','series','Local name','{}')",[]).unwrap();
 let routes=Router::new()
 .route("/sync/activities",get(||async{axum::Json(json!({"all":"2026-01-02T00:00:00Z"}))}))
 .route("/sync/all-items",get(||async{axum::Json(json!({"shows":[{"status":"watching","last_watched_at":"2026-01-01T00:00:00Z","next_to_watch_info":{"season":1,"episode":2,"date":"2020-01-01T00:00:00Z"},"show":{"title":"Show","genres":["Drama"],"runtime":24,"ids":{"simkl":20,"imdb":"tt7654321"}},"seasons":[{"number":1,"episodes":[{"number":1,"runtime":24,"watched_at":"2026-01-01T00:00:00Z"}]}]}],"anime":[{"status":"watching","next_to_watch_info":{"episode":2,"date":"2020-01-01T00:00:00Z"},"show":{"title":"Unmapped anime","genres":["Drama"],"ids":{"simkl":30}},"seasons":[{"number":1,"episodes":[{"number":1,"runtime":24,"watched_at":"2026-01-01T00:00:00Z"}]}]}]}))}))
 .route("/sync/playback",get(||async{axum::Json(json!([]))}))
 .route("/tv/20",get(||async{axum::Json(json!({"title":"Verified parent","overview":"Authoritative description","runtime":24,"ids":{"simkl":20,"imdb":"tt7654321"}}))}))
 .route("/tv/episodes/20",get(||async{axum::Json(json!([{"title":"First","season":1,"episode":1,"aired":true,"ids":{"simkl":201}},{"title":"Second","season":1,"episode":2,"aired":true,"ids":{"simkl":202}}]))}))
 .route("/anime/30",get(||async{axum::Json(json!({"title":"Unmapped anime","ids":{"simkl":30}}))}))
 .route("/anime/episodes/30",get(||async{axum::Json(json!([{"title":"Second","type":"episode","episode":2,"aired":true,"ids":{"simkl":302}}]))}));
 let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();let task=tokio::spawn(async move{axum::serve(listener,routes).await.unwrap()});app.simkl=Arc::new(Service::fixture(&format!("http://{address}")));
 assert_eq!(request(&app,"member-token-1","GET","/api/v3/profiles/1/library",Value::Null).await.0,StatusCode::OK);
 let mut done=false;for _ in 0..150 {let(_,state)=request(&app,"member-token-1","GET","/api/v3/profiles/1/imports/simkl",Value::Null).await;if state["running"]==false{assert_eq!(state["availability"],"Available");done=true;break;}tokio::time::sleep(Duration::from_millis(20)).await;}assert!(done);
 {let db=app.db.lock().unwrap();let queue=crate::continuation::queue(&db,1,0,12).unwrap();assert_eq!(queue["items"].as_array().unwrap().len(),1);let next=&queue["items"][0];assert_eq!(next["queue_status"],"next");assert_eq!(next["id"],"tt7654321:1:2");assert_eq!(next["engine_identity"]["episode"]["episode"],2);assert_eq!(next["engine_parent_identity"]["id"],"tt7654321");assert_eq!(next["position"],0);assert_eq!(db.query_row("SELECT COUNT(*) FROM progress WHERE id='tt7654321:1:2'",[],|r|r.get::<_,u64>(0)).unwrap(),0);assert_eq!(db.query_row("SELECT COUNT(*) FROM simkl_tracking_receipts",[],|r|r.get::<_,u64>(0)).unwrap(),0);}
 let(status,detail)=request(&app,"member-token-1","GET","/api/v3/meta/series/tt7654321",Value::Null).await;assert_eq!(status,StatusCode::OK,"{detail}");assert_eq!(detail["item"]["identity"]["id"],"tt7654321");assert_eq!(detail["item"]["description"],"Authoritative description");assert_eq!(detail["episodes"][1]["identity"]["id"],"tt7654321:1:2");assert_eq!(detail["episodes"][1]["parent_identity"]["id"],"tt7654321");task.abort();
}
