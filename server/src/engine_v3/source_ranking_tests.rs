use crate::{auth_integration_tests::fixture,test_support::request,*};
#[tokio::test]
async fn remembered_supplier_candidates_preserve_language_hard_filters_and_owner_scope(){
 let(app,cards)=rank_fixture(vec![
  json!({"url":"https://media.example/first.mp4","name":"Other","quality_height":1080,"audio_languages":["en"],"presentation":{"provider_name":"Other"}}),
  json!({"url":"https://media.example/manual.mp4","name":"Manual","quality_height":1080,"audio_languages":["eng"],"presentation":{"provider_name":"CineStream"}}),
  json!({"url":"https://media.example/japanese.mp4","name":"Japanese","quality_height":1080,"audio_languages":["ja"],"presentation":{"provider_name":"CineStream"}}),
  json!({"url":"https://media.example/3d.mp4","name":"3D","three_d":true,"quality_height":1080,"audio_languages":["en"],"presentation":{"provider_name":"CineStream"}}),
 ]);
 let ids=cards.iter().map(|row|row["id"].clone()).collect::<Vec<_>>();
 let memory=json!({"provider_id":cards[0]["source_addon_id"],"provider_name":"CineStream","audio_languages":["en"]});
 let body=json!({"ids":ids,"remembered_source":memory});
 let(status,ranked)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",body.clone()).await;
 assert_eq!(status,StatusCode::OK,"{ranked}");assert_eq!(ranked["source_reuse_supported"],true);
 assert_eq!(ranked["source_reuse_ids"],json!([cards[1]["id"],cards[0]["id"]]));
 assert_eq!(ranked["source_reuse_preferred"],true);assert_eq!(ranked["source_reuse_fully_matches"],true);
 assert!(!ranked["automatic_source_ids"].as_array().unwrap().contains(&cards[3]["id"]));
 assert!(!ranked.to_string().contains("https://"));
 assert_eq!(request(&app,"member-token-2","POST","/api/profiles/1/source-ranking",body).await.0,StatusCode::FORBIDDEN);
 let(_,none)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids,"remembered_source":{"provider_id":memory["provider_id"],"audio_languages":["fr"]},"cross_title_source":memory})).await;
 assert_eq!(none["source_reuse_ids"],json!([]));
}
#[tokio::test]
async fn source_reuse_rejects_expiring_urls_and_honors_profile_opt_out(){
 let(app,cards)=rank_fixture(vec![json!({"url":"https://media.example/stream.mp4","name":"Source","audio_languages":["en"]})]);
 let ids=vec![cards[0]["id"].clone()];
 let bad=json!({"provider_id":"https://media.example/private-capability","audio_languages":["en"]});
 assert_eq!(request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids,"remembered_source":bad})).await.0,StatusCode::BAD_REQUEST);
 let(_,prefs)=request(&app,"member-token-1","GET","/api/profiles/1/preferences",Value::Null).await;
 let mut updated=prefs["preferences"].clone();updated["remembered_source_reuse"]=json!(false);
 assert_eq!(request(&app,"member-token-1","PUT","/api/profiles/1/preferences",json!({"revision":prefs["revision"],"preferences":updated})).await.0,StatusCode::OK);
 let(_,ranked)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids,"remembered_source":{"provider_id":cards[0]["source_addon_id"],"audio_languages":["en"]}})).await;
 assert_eq!(ranked["source_reuse_ids"],json!([]));
}
#[tokio::test]
async fn ranking_preserves_private_grants_checks_owner_and_uses_structured_quality(){
 let app=fixture();let owned=app.clone().with_lease(ResourceLease{policy_revision:0,principal:auth::Principal::Account{account_id:1,role:"owner".into(),profile_id:Some(1),session_id:Some("s1".into())},session_id:Some("s1".into())});
 {let db=app.db.lock().unwrap();crate::addon::credentials_v2::store_checked(&db,app.secret_vault.as_ref().unwrap(),1,"https://source.example/manifest.json",&json!({"id":"sources","name":"Sources","version":"1","resources":["stream"],"types":["movie"]}),&None).unwrap();}
 let connection=owned.addons.engine_connections(1).unwrap().into_iter().find(|c|c.capabilities.contains(&viptv_engine::capabilities::Capability::Sources)).unwrap();
 let(cards,error)=owned.register(&connection.id,vec![json!({"url":"https://media.example/high.mp4","name":"High","quality_height":2160}),json!({"url":"https://media.example/compatible.mp4","name":"Compatible","quality_height":1080}),json!({"url":"https://media.example/unknown.mp4","name":"Unknown"})],"movie");assert!(error.is_none());
 let job=Arc::new(Job{metadata_proof:None,source_slots:std::sync::atomic::AtomicUsize::new(0),cancelled:std::sync::atomic::AtomicBool::new(false),configuration_revision:None,media_id:None,exact_vod:None,kind:"movie".into(),created:Instant::now(),state:Mutex::new(JobState{events:vec![json!({"seq":1,"source":connection.id,"streams":cards})],pending:0}),notify:Notify::new()});app.jobs.lock().unwrap().insert("rank-job".into(),job);
 let(_,defaults)=request(&app,"member-token-1","GET","/api/profiles/1/preferences",Value::Null).await;let mut prefs=defaults["preferences"].clone();prefs["max_quality_height"]=json!(1080);assert_eq!(request(&app,"member-token-1","PUT","/api/profiles/1/preferences",json!({"revision":0,"preferences":prefs})).await.0,StatusCode::OK);
 let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();let(status,ranked)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids})).await;assert_eq!(status,StatusCode::OK,"{ranked}");assert_eq!(ranked["streams"][0]["name"],"Compatible");assert_eq!(ranked["streams"][1]["name"],"Unknown");assert!(!ranked.to_string().contains("https://"));assert_eq!(request(&app,"member-token-2","POST","/api/profiles/1/source-ranking",json!({"ids":ids})).await.0,StatusCode::FORBIDDEN);
}
fn rank_fixture(streams:Vec<Value>)->(App,Vec<Value>){
 let app=fixture();let owned=app.clone().with_lease(ResourceLease{policy_revision:0,principal:auth::Principal::Account{account_id:1,role:"owner".into(),profile_id:Some(1),session_id:Some("s1".into())},session_id:Some("s1".into())});
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
 assert_eq!(rank_names(&app,&ids,Some(1080)).await,["1080","720","1440","2160"]);
 let(_,defaults)=request(&app,"member-token-1","GET","/api/profiles/1/preferences",Value::Null).await;let mut prefs=defaults["preferences"].clone();prefs["max_quality_height"]=json!(1440);assert_eq!(request(&app,"member-token-1","PUT","/api/profiles/1/preferences",json!({"revision":0,"preferences":prefs})).await.0,StatusCode::OK);
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
 let ranked=rank_names(&app,&ids,None).await;
 assert_eq!(ranked,["Debrid\n4k","Torrentio\n4k DV | HDR10+ | HDR","Debrid\n4k","Torrentio\n1080p"]);
 let(_,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids})).await;
 assert_eq!(body["streams"][0]["release_type"],"web_dl");assert_eq!(body["streams"][2]["release_type"],"remux");
}

#[tokio::test]
async fn remote_quality_filters_preserve_server_order_and_report_all_facets() {
    let (app,cards)=rank_fixture(vec![json!({"url":"https://media.example/4k.mp4","name":"4K","quality_height":2160}),json!({"url":"https://media.example/hd.mp4","name":"HD","quality_height":1080})]);
    let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();
    let (status,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids,"quality_height":1080})).await;
    assert_eq!(status,StatusCode::OK,"{body}");assert_eq!(body["streams"].as_array().unwrap().len(),1);assert_eq!(body["streams"][0]["quality_height"],1080);assert_eq!(body["total"],2);assert_eq!(body["quality_filters"].as_array().unwrap().len(),2);assert!(ids.contains(&body["best_match_id"]));
    assert_eq!(body["streams"][0]["view"]["quality_label"], "1080p");
    assert_eq!(body["streams"][0]["view"]["title"], "HD");
    assert_eq!(body["streams"][0]["view"]["addon_label"], "Sources");
}

#[tokio::test]
async fn best_matches_are_the_first_five_rows_passing_the_hard_filters() {
    let mut streams=(1..=7).map(|step|json!({"url":format!("https://media.example/{step}.mp4"),"name":format!("row {step}"),"quality_height":2400-step*240})).collect::<Vec<_>>();
    streams.push(json!({"infoHash":"0123456789abcdef0123456789abcdef01234567","name":"camera","title":"Thing.2024.1080p.CAM.x264-GRP\n👤 900"}));
    streams.push(json!({"url":"https://media.example/stereo.mp4","name":"stereo","three_d":true,"quality_height":2160}));
    streams.push(json!({"infoHash":"1123456789abcdef0123456789abcdef01234567","name":"dead","title":"Thing.2024.2160p.WEB-DL.x264-GRP","seeders":4}));
    let(app,cards)=rank_fixture(streams);
    let cam=cards[7]["id"].clone();let stereo=cards[8]["id"].clone();let dead=cards[9]["id"].clone();
    let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();
    let(status,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids})).await;
    assert_eq!(status,StatusCode::OK,"{body}");
    let ranked=body["streams"].as_array().unwrap().iter().map(|card|card["id"].clone()).collect::<Vec<_>>();
    assert_eq!(ranked.len(),10);
    assert_eq!(body["best_match_ids"],json!(ranked[..5].to_vec()));
    assert_eq!(body["best_match_id"],ranked[0]);
    for excluded in [cam,stereo,dead] {
        assert!(ranked[7..].contains(&excluded));
        assert!(!body["best_match_ids"].as_array().unwrap().contains(&excluded));
    }
}

#[tokio::test]
async fn hard_filtered_rows_supply_no_best_match() {
    let(app,cards)=rank_fixture(vec![
        json!({"infoHash":"0123456789abcdef0123456789abcdef01234567","name":"camera","title":"Thing.2024.1080p.CAM.x264-GRP\n👤 900"}),
        json!({"url":"https://media.example/stereo.mp4","name":"stereo","three_d":true}),
        json!({"infoHash":"1123456789abcdef0123456789abcdef01234567","name":"dead","title":"Thing.2024.1080p.WEB-DL.x264-GRP","seeders":4}),
    ]);
    let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();
    let(status,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids})).await;
    assert_eq!(status,StatusCode::OK,"{body}");
    assert_eq!(body["streams"].as_array().unwrap().len(),3);
    assert_eq!(body["best_match_ids"],json!([]));
    assert!(body["best_match_id"].is_null(),"{body}");
    assert_eq!(body["best_match_fully_matches"],json!(false));
}

#[tokio::test]
async fn the_quality_filter_hides_rows_without_changing_group_membership() {
    let(app,cards)=rank_fixture(vec![json!({"url":"https://media.example/4k.mp4","name":"4K","quality_height":2160}),json!({"url":"https://media.example/hd.mp4","name":"HD","quality_height":1080})]);
    let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();
    let(status,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids,"quality_height":1080})).await;
    assert_eq!(status,StatusCode::OK,"{body}");
    assert_eq!(body["streams"].as_array().unwrap().len(),1);
    assert_eq!(body["streams"][0]["id"],ids[1]);
    assert_eq!(body["best_match_ids"],json!([ids[0].clone(),ids[1].clone()]));
    assert_eq!(body["best_match_id"],ids[0]);
}

#[tokio::test]
async fn best_match_fully_matches_reports_whether_every_preference_is_met() {
    let (app,cards)=rank_fixture(vec![
        json!({"url":"https://media.example/direct.mp4","name":"direct","title":"Thing.2024.1080p.WEB-DL.x264-GRP","quality_height":1080}),
        json!({"infoHash":"0123456789abcdef0123456789abcdef01234567","name":"torrent","title":"Thing.2024.1080p.WEB-DL.x264-GRP","seeders":7}),
    ]);
    let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();
    let(status,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids})).await;
    assert_eq!(status,StatusCode::OK,"{body}");
    assert_eq!(body["streams"][0]["name"],"direct");
    assert_eq!(body["best_match_fully_matches"],json!(true));
    let (app,cards)=rank_fixture(vec![
        json!({"infoHash":"0123456789abcdef0123456789abcdef01234567","name":"torrent","title":"Thing.2024.1080p.WEB-DL.x264-GRP","seeders":7}),
    ]);
    let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();
    let(status,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids})).await;
    assert_eq!(status,StatusCode::OK,"{body}");
    assert_eq!(body["best_match_id"],ids[0]);
    assert_eq!(body["best_match_fully_matches"],json!(false));
}

#[tokio::test]
async fn a_device_unsupported_codec_drops_the_row_out_of_best_matches_and_sorts_it_last() {
    let (app,cards)=rank_fixture(vec![
        json!({"url":"https://media.example/hevc.mkv","name":"hevc","codec":"hevc","quality_height":2160}),
        json!({"url":"https://media.example/avc.mkv","name":"avc","codec":"avc","quality_height":2160}),
    ]);
    let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();
    let device=json!({"video_codecs":[{"codec":"hevc","support":"unsupported"}]});
    let(status,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids,"device":device})).await;
    assert_eq!(status,StatusCode::OK,"{body}");
    let names=body["streams"].as_array().unwrap().iter().map(|card|card["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(names,["avc","hevc"]);
    assert_eq!(body["best_match_ids"],json!([ids[1].clone()]));
    assert_eq!(body["best_match_id"],ids[1]);
}

#[tokio::test]
async fn a_dv_only_row_ranks_after_supported_rows_on_a_device_without_a_dv_decoder() {
    let (app,cards)=rank_fixture(vec![
        json!({"url":"https://media.example/sdr.mkv","name":"sdr","title":"Movie.2024.2160p.WEB-DL.x264-GRP"}),
        json!({"url":"https://media.example/dv.mkv","name":"dv","title":"Movie.2024.2160p.WEB-DL.DV-GRP"}),
    ]);
    let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();
    let device=json!({"dolby_vision":"unsupported"});
    let(status,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids,"device":device})).await;
    assert_eq!(status,StatusCode::OK,"{body}");
    let names=body["streams"].as_array().unwrap().iter().map(|card|card["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(names,["sdr","dv"]);
}

#[tokio::test]
async fn a_device_max_height_wins_over_the_legacy_device_max_height() {
    let (app,cards)=rank_fixture(vec![
        json!({"url":"https://media.example/2160.mkv","name":"2160","quality_height":2160}),
        json!({"url":"https://media.example/1080.mkv","name":"1080","quality_height":1080}),
    ]);
    let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();
    let (status,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids,"device_max_height":1080,"device":{"max_height":2160}})).await;
    assert_eq!(status,StatusCode::OK,"{body}");
    let names=body["streams"].as_array().unwrap().iter().map(|card|card["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(names,["2160","1080"]);
}

#[tokio::test]
async fn a_remembered_provider_and_binge_group_ranks_first_within_its_language_tier() {
    let (app,cards)=rank_fixture(vec![
        json!({"url":"https://media.example/a.mkv","name":"remembered","quality_height":1080,"behaviorHints":{"bingeGroup":"group-a"}}),
        json!({"url":"https://media.example/b.mkv","name":"other","quality_height":1080}),
    ]);
    let provider_id=app.addons.clone().for_account(1).engine_connections(1).unwrap().into_iter().find(|c|c.capabilities.contains(&viptv_engine::capabilities::Capability::Sources)).unwrap().id;
    let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();
    let body=json!({"ids":ids,"series_memory":{"provider_id":provider_id,"binge_group":"group-a"}});
    let(status,ranked)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",body).await;
    assert_eq!(status,StatusCode::OK,"{ranked}");
    let names=ranked["streams"].as_array().unwrap().iter().map(|card|card["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(names,["remembered","other"]);
    assert_eq!(ranked["best_match_id"],ids[0]);
}

#[tokio::test]
async fn invalid_capabilities_are_rejected() {
    let (app,cards)=rank_fixture(vec![json!({"url":"https://media.example/a.mkv","name":"a","quality_height":1080})]);
    let ids=cards.iter().map(|card|card["id"].clone()).collect::<Vec<_>>();
    for invalid in [
        json!({"max_height":143}),
        json!({"max_height":4321}),
        json!({"video_codecs":[{"codec":"hevc","support":"supported","max_height":4321}]}),
        json!({"video_codecs":[{"codec":"bad codec!","support":"supported"}]}),
    ] {
        let (status,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids,"device":invalid})).await;
        assert_eq!(status,StatusCode::BAD_REQUEST,"{body}");
        assert!(body.to_string().contains("Invalid source selection"),"{body}");
    }
    for invalid in [
        json!({"provider_id":"","binge_group":"g"}),
        json!({"provider_id":"addon:1","binge_group":""}),
    ] {
        let (status,body)=request(&app,"member-token-1","POST","/api/profiles/1/source-ranking",json!({"ids":ids,"series_memory":invalid})).await;
        assert_eq!(status,StatusCode::BAD_REQUEST,"{body}");
        assert!(body.to_string().contains("Invalid source selection"),"{body}");
    }
}
