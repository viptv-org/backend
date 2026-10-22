use crate::{auth_integration_tests::fixture,test_support::request,*};
#[tokio::test]
async fn ranking_preserves_private_grants_checks_owner_and_uses_structured_quality(){
 let app=fixture();let owned=app.clone().with_lease(ResourceLease{policy_revision:0,principal:auth::Principal::Account{account_id:1,role:"member".into(),profile_id:Some(1),session_id:Some("s1".into())},session_id:Some("s1".into())});
 {let db=app.db.lock().unwrap();crate::addon::credentials_v2::store_checked(&db,app.secret_vault.as_ref().unwrap(),1,"https://source.example/manifest.json",&json!({"id":"sources","name":"Sources","version":"1","resources":["stream"],"types":["movie"]}),&None).unwrap();}
 let connection=owned.addons.engine_connections(1).unwrap().into_iter().find(|c|c.capabilities.contains(&viptv_engine::capabilities::Capability::Sources)).unwrap();
 let(cards,error)=owned.register(&connection.id,vec![json!({"url":"https://media.example/high.mp4","name":"High","quality_height":2160}),json!({"url":"https://media.example/compatible.mp4","name":"Compatible","quality_height":1080}),json!({"url":"https://media.example/unknown.mp4","name":"Unknown"})],"movie");assert!(error.is_none());
 let job=Arc::new(Job{metadata_proof:None,source_slots:std::sync::atomic::AtomicUsize::new(0),cancelled:std::sync::atomic::AtomicBool::new(false),configuration_revision:None,media_id:None,exact_vod:None,kind:"movie".into(),created:Instant::now(),state:Mutex::new(JobState{events:vec![json!({"seq":1,"source":connection.id,"streams":cards})],pending:0}),notify:Notify::new()});app.jobs.lock().unwrap().insert("rank-job".into(),job);
 let(_,defaults)=request(&app,"member-token-1","GET","/api/profiles/1/preferences",Value::Null).await;let mut prefs=defaults["preferences"].clone();prefs["max_quality_height"]=json!(1080);assert_eq!(request(&app,"member-token-1","PUT","/api/profiles/1/preferences",json!({"revision":0,"preferences":prefs})).await.0,StatusCode::OK);
 let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();let(status,ranked)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids})).await;assert_eq!(status,StatusCode::OK,"{ranked}");assert_eq!(ranked["streams"][0]["name"],"Compatible");assert_eq!(ranked["streams"][1]["name"],"Unknown");assert!(!ranked.to_string().contains("https://"));assert_eq!(request(&app,"member-token-2","POST","/api/profiles/1/source-ranking",json!({"ids":ids})).await.0,StatusCode::FORBIDDEN);
}
fn rank_fixture(streams:Vec<Value>)->(App,Vec<Value>){
 let app=fixture();let owned=app.clone().with_lease(ResourceLease{policy_revision:0,principal:auth::Principal::Account{account_id:1,role:"member".into(),profile_id:Some(1),session_id:Some("s1".into())},session_id:Some("s1".into())});
 {let db=app.db.lock().unwrap();crate::addon::credentials_v2::store_checked(&db,app.secret_vault.as_ref().unwrap(),1,"https://source.example/manifest.json",&json!({"id":"sources","name":"Sources","version":"1","resources":["stream"],"types":["movie"]}),&None).unwrap();}
 let connection=owned.addons.engine_connections(1).unwrap().into_iter().find(|c|c.capabilities.contains(&viptv_engine::capabilities::Capability::Sources)).unwrap();
 let(cards,error)=owned.register(&connection.id,streams,"movie");assert!(error.is_none(),"{error:?}");
 let job=Arc::new(Job{metadata_proof:None,source_slots:std::sync::atomic::AtomicUsize::new(0),cancelled:std::sync::atomic::AtomicBool::new(false),configuration_revision:None,media_id:None,exact_vod:None,kind:"movie".into(),created:Instant::now(),state:Mutex::new(JobState{events:vec![json!({"seq":1,"source":connection.id,"streams":cards})],pending:0}),notify:Notify::new()});app.jobs.lock().unwrap().insert("rank-job".into(),job);
 (app,cards)
}
async fn rank_names(app:&App,ids:&[Value],device:Option<u32>)->Vec<String>{
 let mut body=json!({"ids":ids});if let Some(device)=device{body["device_max_height"]=json!(device);}
 let(status,ranked)=request(app,"member-token-1","POST","/api/profiles/1/source-ranking",body).await;assert_eq!(status,StatusCode::OK,"{ranked}");
 ranked["streams"].as_array().unwrap().iter().map(|s|s["name"].as_str().unwrap().to_string()).collect()
}
#[tokio::test]
async fn ranking_caps_quality_at_the_device_height_and_the_profile_cap(){
 let streams=[2160,1440,1080,720].into_iter().map(|h|json!({"url":format!("https://media.example/{h}.mp4"),"name":format!("{h}"),"quality_height":h})).collect();
 let(app,cards)=rank_fixture(streams);let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();
 assert_eq!(rank_names(&app,&ids,None).await,["2160","1440","1080","720"]);
 // Device only: over the cap, the height closest to the device comes first.
 assert_eq!(rank_names(&app,&ids,Some(1080)).await,["1080","720","1440","2160"]);
 let(_,defaults)=request(&app,"member-token-1","GET","/api/profiles/1/preferences",Value::Null).await;let mut prefs=defaults["preferences"].clone();prefs["max_quality_height"]=json!(1440);assert_eq!(request(&app,"member-token-1","PUT","/api/profiles/1/preferences",json!({"revision":0,"preferences":prefs})).await.0,StatusCode::OK);
 // Profile only, then both: the lower cap wins.
 assert_eq!(rank_names(&app,&ids,None).await,["1440","1080","720","2160"]);
 assert_eq!(rank_names(&app,&ids,Some(720)).await,["720","1080","1440","2160"]);
 assert_eq!(rank_names(&app,&ids,Some(2160)).await,["1440","1080","720","2160"]);
 for invalid in [json!(143),json!(4321),json!(-1),json!("1080")]{
  let(status,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids,"device_max_height":invalid})).await;assert_ne!(status,StatusCode::OK,"{invalid} {body}");
 }
 let(status,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids,"device_max_height":100})).await;assert_eq!(status,StatusCode::BAD_REQUEST);assert!(body.to_string().contains("Invalid source selection"),"{body}");
}
#[tokio::test]
async fn cards_carry_release_facts_and_ranking_reads_them_back(){
 let hash="0123456789abcdef0123456789abcdef01234567";
 let streams=vec![
  json!({"infoHash":hash,"name":"Torrentio\n1080p","title":"Movie.2024.1080p.CAM.x264-GRP\n👤 900 💾 1.4 GB ⚙️ 1337x"}),
  json!({"infoHash":"1123456789abcdef0123456789abcdef01234567","name":"Torrentio\n4k DV | HDR10+ | HDR","title":"Movie.2024.2160p.WEB-DL.DV.HDR10Plus.H.265-GRP\n👤 42 💾 7 GB ⚙️ TPB\nDual Audio / 🇬🇧"}),
  json!({"url":"https://media.example/remux.mkv","name":"Debrid\n4k","title":"Movie.2024.2160p.BluRay.REMUX.HEVC-GRP"}),
  json!({"url":"https://media.example/web.mkv","name":"Debrid\n4k","title":"Movie.2024.2160p.WEB-DL.HEVC-GRP","release_type":"web_dl","seeders":3}),
 ];
 let(app,cards)=rank_fixture(streams);
 assert_eq!(cards[0]["release_type"],"cam");assert_eq!(cards[0]["delivery"],"torrent");assert_eq!(cards[0]["seeders"],900);
 let torrent=&cards[1];
 assert_eq!(torrent["release_type"],"web_dl");assert_eq!(torrent["dynamic_range"],json!(["dv","hdr10_plus","hdr"]));assert_eq!(torrent["audio_flags"],json!(["dual"]));assert_eq!(torrent["seeders"],42);assert_eq!(torrent["delivery"],"torrent");assert_eq!(torrent["audio_languages"],json!(["en"]));assert!(torrent.get("three_d").is_none());
 assert_eq!(cards[2]["release_type"],"remux");assert_eq!(cards[2]["delivery"],"direct");assert!(cards[2].get("seeders").is_none()&&cards[2].get("audio_flags").is_none()&&cards[2].get("dynamic_range").is_none());
 assert_eq!(cards[3]["seeders"],3);
 assert!(!cards.iter().any(|card|card.to_string().contains(hash)));
 let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();
 // Direct WEB-DL before torrent WEB-DL (3 declared seeders do not sink a direct
 // stream), REMUX after both at equal height, camera last.
 let ranked=rank_names(&app,&ids,None).await;
 assert_eq!(ranked,["Debrid\n4k","Torrentio\n4k DV | HDR10+ | HDR","Debrid\n4k","Torrentio\n1080p"]);
 let(_,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids})).await;
 assert_eq!(body["streams"][0]["release_type"],"web_dl");assert_eq!(body["streams"][2]["release_type"],"remux");
}
