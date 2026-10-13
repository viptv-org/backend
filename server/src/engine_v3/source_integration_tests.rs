use crate::{auth_integration_tests::fixture,test_support::request,*};
#[tokio::test]
async fn sdk_configuration_is_profile_private_vault_backed_and_revision_fenced(){
    let app=fixture();
    let (status,schemas)=request(&app,"member-token-1","GET","/api/v3/profiles/1/source-integrations",json!({})).await;assert_eq!(status,StatusCode::OK);assert_eq!(schemas["schemas"][0]["runtime_enabled"],cfg!(feature="vsources-runtime"));
    let config=json!({"revision":0,"plugin_id":"vsources","configuration":{"tmdb_api_key":"fixture-write-only-key"}});
    let (status,public)=request(&app,"member-token-1","PUT","/api/v3/profiles/1/source-integrations",config.clone()).await;
    if !cfg!(feature="vsources-runtime"){assert_eq!(status,StatusCode::SERVICE_UNAVAILABLE);return;}
    assert_eq!(status,StatusCode::OK,"{public}");assert!(!public.to_string().contains("fixture-write-only-key"));
    let (status,schemas)=request(&app,"member-token-1","GET","/api/v3/profiles/1/source-integrations",json!({})).await;assert_eq!(status,StatusCode::OK);assert_eq!(schemas["schemas"][0]["state"],"Ready");let id=schemas["schemas"][0]["connection_id"].as_str().unwrap();
    let secret:String=app.db.lock().unwrap().query_row("SELECT secret FROM engine_source_integrations LIMIT 1",[],|r|r.get(0)).unwrap();assert!(!secret.contains("fixture-write-only-key"));
    assert_eq!(request(&app,"member-token-1","PUT","/api/v3/profiles/1/source-integrations",config).await.0,StatusCode::CONFLICT);
    assert_eq!(request(&app,"member-token-2","GET","/api/v3/profiles/1/source-integrations",json!({})).await.0,StatusCode::FORBIDDEN);
    let before=crate::sources::source_configuration(&app.db.lock().unwrap(),id).unwrap();let (status,_)=request(&app,"member-token-1","DELETE",&format!("/api/v3/profiles/1/source-integrations/{id}?revision=1"),json!({})).await;assert_eq!(status,StatusCode::OK);assert!(before.is_some());assert!(crate::sources::source_configuration(&app.db.lock().unwrap(),id).unwrap().is_none());
}
#[cfg(feature="vsources-runtime")]
#[tokio::test]
#[ignore="requires explicitly injected owned TMDB credential and real public metadata/source providers"]
async fn real_sdk_api_uses_selected_metadata_and_opaque_incremental_grants(){
 let app=fixture();let(status,addon)=request(&app,"member-token-1","POST","/api/v2/addons",json!({"manifest_url":"https://v3-cinemeta.strem.io/manifest.json"})).await;assert_eq!(status,StatusCode::OK,"Public metadata registration failed");let addon_id=addon["id"].as_i64().unwrap();
 let (status,_)=request(&app,"member-token-1","PUT","/api/v3/profiles/1/source-integrations",json!({"revision":0,"plugin_id":"vsources","configuration":{"tmdb_api_key":std::env::var("VIPTV_QA_TMDB_KEY").unwrap()}})).await;assert_eq!(status,StatusCode::OK);
 let(_,schemas)=request(&app,"member-token-1","GET","/api/v3/profiles/1/source-integrations",Value::Null).await;let sdk_id=schemas["schemas"][0]["connection_id"].as_str().unwrap();
 let(status,_)=request(&app,"member-token-1","PUT","/api/v3/profiles/1/integrations",json!({"revision":1,"browsing_plugin":"stremio","assignments":[{"connection_id":format!("addon:{addon_id}"),"enabled":["Metadata"]},{"connection_id":sdk_id,"enabled":["Sources"]}]})).await;assert_eq!(status,StatusCode::OK);
 let(status,detail)=request(&app,"member-token-1","GET","/api/v3/meta/movie/tt1375666",Value::Null).await;assert_eq!(status,StatusCode::OK,"Selected public metadata lookup failed");let mut hint=detail["item"].clone();hint["identity"]["ids"]=json!([{"namespace":"tmdb","value":"1"}]);
 let(status,job)=request(&app,"member-token-1","POST","/api/v2/streams",json!({"type":"movie","id":"tt1375666","card":hint,"only_addons":true})).await;assert_eq!(status,StatusCode::OK);let id=job["id"].as_str().unwrap();let start=Instant::now();
 let grant=loop{let(status,poll)=request(&app,"member-token-1","GET",&format!("/api/v2/streams/{id}?wait_ms=10000"),Value::Null).await;assert_eq!(status,StatusCode::OK);if let Some(grant)=poll["events"].as_array().and_then(|events|events.iter().find_map(|event|event["streams"][0]["id"].as_str())){assert!(!poll.to_string().contains("https://"));break grant.to_string();}assert!(start.elapsed().as_secs()<55 && poll["done"]!=true,"No SDK grant qualified");};
 let(status,_)=request(&app,"member-token-1","DELETE",&format!("/api/v2/streams/{id}"),Value::Null).await;assert_eq!(status,StatusCode::OK);assert!(app.streams.lock().unwrap().contains_key(&grant));println!("Real SDK API opaque source grant qualified; selected metadata verified; cancel preserves grant; elapsed_seconds={}",start.elapsed().as_secs());
}
