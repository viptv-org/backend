use super::*;
use std::collections::HashMap;

#[derive(Default)]
struct MemorySecrets(Mutex<HashMap<String, String>>);
impl SecretStore for MemorySecrets {
    fn get(&self, key: String) -> Result<Option<String>, CoreError> {
        Ok(self.0.lock().unwrap().get(&key).cloned())
    }
    fn put(&self, key: String, value: String) -> Result<(), CoreError> {
        self.0.lock().unwrap().insert(key, value);
        Ok(())
    }
    fn remove(&self, key: String) -> Result<(), CoreError> {
        self.0.lock().unwrap().remove(&key);
        Ok(())
    }
}

#[test]
fn warm_home_retries_only_failed_rows_and_search_never_populates_home_cache() {
    let dir = std::env::temp_dir().join(format!("viptv-cache-{}", uuid::Uuid::new_v4()));
    let engine = LocalEngine::new(
        dir.join("local.sqlite").to_string_lossy().into(),
        Arc::new(MemorySecrets::default()),
    )
    .unwrap();
    engine.install_addon("default".into(), "https://example.com/manifest.json".into(),
        r#"{"id":"test","name":"Movies","version":"1","resources":["catalog"],"catalogs":[{"id":"first","type":"movie","extra":[{"name":"search"}]},{"id":"second","type":"movie"}]}"#.into()).unwrap();
    let plan: Value = serde_json::from_str(&engine.plan_home("default".into()).unwrap()).unwrap();
    let first_id = plan["requests"][0]["id"].clone();
    let second_id = plan["requests"][1]["id"].clone();
    let results = json!([{"id":first_id,"body":r#"{"metas":[{"id":"tt1","type":"movie","name":"Home film"}]}"#}]);
    engine
        .project_home(plan["id"].as_str().unwrap().into(), results.to_string())
        .unwrap();
    let search: Value = serde_json::from_str(
        &engine
            .plan_search("default".into(), "Other".into())
            .unwrap(),
    )
    .unwrap();
    let results = json!([{"id":search["requests"][0]["id"],"body":r#"{"metas":[{"id":"tt2","type":"movie","name":"Search film"}]}"#}]);
    engine
        .project_home(search["id"].as_str().unwrap().into(), results.to_string())
        .unwrap();
    let warm: Value = serde_json::from_str(&engine.plan_home("default".into()).unwrap()).unwrap();
    assert_eq!(warm["requests"].as_array().unwrap().len(), 1);
    assert_eq!(warm["requests"][0]["id"], second_id);
    let initial: Value = serde_json::from_str(
        &engine
            .prepared_home(warm["id"].as_str().unwrap().into())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(initial["rows"][0]["cards"][0]["title"], "Home film");
    assert_eq!(initial["rows"][1]["state"], "Pending");
    let final_home: Value = serde_json::from_str(
        &engine
            .project_home(warm["id"].as_str().unwrap().into(), "[]".into())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(final_home["rows"][0]["id"], first_id);
    assert_eq!(final_home["rows"][0]["state"], "Ready");
    assert_eq!(final_home["rows"][1]["state"], "Failed");
    assert!(final_home["rows"][1]["message"].is_string());
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn subtitle_capabilities_are_profile_scoped_and_selections_expire_on_configuration_change() {
    let dir = std::env::temp_dir().join(format!("viptv-subtitles-{}", uuid::Uuid::new_v4()));
    let engine = LocalEngine::new(
        dir.join("local.sqlite").to_string_lossy().into(),
        Arc::new(MemorySecrets::default()),
    )
    .unwrap();
    engine
        .create_profile("second".into(), "Second".into())
        .unwrap();
    let installed: Value = serde_json::from_str(&engine.install_addon("default".into(), "https://addon.example/secret/manifest.json".into(),
        r#"{"id":"subs","name":"Subtitles","version":"1","resources":["subtitles"],"types":["movie"]}"#.into()).unwrap()).unwrap();
    let other: Value = serde_json::from_str(
        &engine
            .plan_subtitles("second".into(), "movie".into(), "tt1".into(), "{}".into())
            .unwrap(),
    )
    .unwrap();
    assert!(other["requests"].as_array().unwrap().is_empty());
    let plan: Value = serde_json::from_str(
        &engine
            .plan_subtitles("default".into(), "movie".into(), "tt1".into(), "{}".into())
            .unwrap(),
    )
    .unwrap();
    let responses = json!([{"provider":plan["requests"][0]["provider"],"body":r#"{"subtitles":[{"lang":"eng","url":"https://cdn.example/private-subtitle.srt"}]}"#}]);
    let snapshot: Value = serde_json::from_str(
        &engine
            .project_subtitles(
                plan["operation"].as_str().unwrap().into(),
                responses.to_string(),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(snapshot["providers"][0]["state"], "Ready");
    assert_eq!(snapshot["candidates"][0]["language"], "eng");
    assert!(!snapshot.to_string().contains("private-subtitle"));
    let id = snapshot["candidates"][0]["id"].as_str().unwrap();
    let opened: Value = serde_json::from_str(&engine.open_subtitle(id.into()).unwrap()).unwrap();
    assert_eq!(opened["url"], "https://cdn.example/private-subtitle.srt");
    engine
        .set_addon_enabled(
            "default".into(),
            installed["id"].as_str().unwrap().into(),
            false,
        )
        .unwrap();
    assert!(engine.open_subtitle(id.into()).is_err());
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn addon_secret_stays_out_of_sqlite_and_home_uses_its_catalog() {
    let dir = std::env::temp_dir().join(format!("viptv-local-{}", uuid::Uuid::new_v4()));
    let path = dir.join("local.sqlite");
    let vault = Arc::new(MemorySecrets::default());
    let engine = LocalEngine::new(path.to_string_lossy().into(), vault.clone()).unwrap();
    engine
        .create_profile("second".into(), "Second".into())
        .unwrap();
    let url = "https://example.com/secret-token/manifest.json";
    let manifest = r#"{"id":"test","name":"My movies","version":"1","resources":["catalog","meta","stream"],"catalogs":[{"id":"top","type":"movie","name":"Popular","extra":[{"name":"search"}]}]}"#;
    engine
        .install_addon("default".into(), url.into(), manifest.into())
        .unwrap();
    assert!(!engine.addons().unwrap().contains("secret-token"));
    assert!(
        !std::fs::read(&path)
            .unwrap()
            .windows(12)
            .any(|part| part == b"secret-token")
    );
    let plan: Value = serde_json::from_str(&engine.plan_home("default".into()).unwrap()).unwrap();
    assert_eq!(plan["requests"].as_array().unwrap().len(), 1);
    let prepared: Value = serde_json::from_str(
        &engine
            .prepared_home(plan["id"].as_str().unwrap().into())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(prepared["rows"][0]["state"], "Pending");
    assert!(
        plan["requests"][0]["url"]
            .as_str()
            .unwrap()
            .contains("secret-token")
    );
    let response = json!([{"id":plan["requests"][0]["id"],"body":r#"{"metas":[{"id":"tt123","type":"movie","name":"Film","background":"https://images.example/landscape"}]}"#}]);
    let home: Value = serde_json::from_str(
        &engine
            .project_home(plan["id"].as_str().unwrap().into(), response.to_string())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(home["hero"]["title"], "Film");
    let warm: Value = serde_json::from_str(&engine.plan_home("default".into()).unwrap()).unwrap();
    assert!(warm["requests"].as_array().unwrap().is_empty());
    let cached: Value = serde_json::from_str(
        &engine
            .prepared_home(warm["id"].as_str().unwrap().into())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(cached["rows"][0]["cards"][0]["title"], "Film");
    assert_eq!(cached["hero"]["title"], "Film");
    assert_eq!(cached["complete"], true);
    let cached_final: Value = serde_json::from_str(
        &engine
            .project_home(warm["id"].as_str().unwrap().into(), "[]".into())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(cached_final["rows"][0]["state"], "Ready");
    let search: Value = serde_json::from_str(
        &engine
            .plan_search("default".into(), "Two Words".into())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(search["requests"].as_array().unwrap().len(), 1);
    assert!(
        search["requests"][0]["url"]
            .as_str()
            .unwrap()
            .contains("search=Two+Words")
    );
    let provider = home["rows"][0]["provider_id"].as_str().unwrap();
    let detail: Value = serde_json::from_str(
        &engine
            .plan_detail(
                "default".into(),
                "movie".into(),
                "tt123".into(),
                provider.into(),
            )
            .unwrap(),
    )
    .unwrap();
    assert!(
        detail["url"]
            .as_str()
            .unwrap()
            .ends_with("/meta/movie/tt123.json")
    );
    let item: Value = serde_json::from_str(
        &engine
            .project_detail(
                detail["operation"].as_str().unwrap().into(),
                r#"{"meta":{"id":"tt123","name":"Film"}}"#.into(),
                0,
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(item["item"]["title"], "Film");
    let sources: Value = serde_json::from_str(
        &engine
            .plan_sources("default".into(), "movie".into(), "tt123".into())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(sources["requests"].as_array().unwrap().len(), 1);
    let response = json!([{"provider":sources["requests"][0]["provider"],"body":r#"{"streams":[{"name":"Full HD","url":"https://media.example/film.mp4","behaviorHints":{"proxyHeaders":{"request":{"Referer":"https://addon.example/"}}}}]}"#}]);
    let candidates: Value = serde_json::from_str(
        &engine
            .project_sources(
                sources["operation"].as_str().unwrap().into(),
                response.to_string(),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(candidates.as_array().unwrap().len(), 1);
    assert!(!candidates.to_string().contains("media.example"));
    let prepared: Value = serde_json::from_str(
        &engine
            .open_source(candidates[0]["id"].as_str().unwrap().into())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(prepared["url"], "https://media.example/film.mp4");
    assert_eq!(prepared["headers"][0][0], "Referer");
    engine
        .set_addon_enabled("default".into(), provider.into(), false)
        .unwrap();
    assert!(
        engine
            .open_source(candidates[0]["id"].as_str().unwrap().into())
            .is_err()
    );
    let disabled: Value =
        serde_json::from_str(&engine.plan_home("default".into()).unwrap()).unwrap();
    assert!(disabled["requests"].as_array().unwrap().is_empty());
    engine
        .set_addon_enabled("default".into(), provider.into(), true)
        .unwrap();
    let second: Value = serde_json::from_str(&engine.plan_home("second".into()).unwrap()).unwrap();
    assert!(second["requests"].as_array().unwrap().is_empty());
    engine
        .install_addon("second".into(), url.into(), manifest.into())
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&engine.addons().unwrap())
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let assigned: Value =
        serde_json::from_str(&engine.plan_home("second".into()).unwrap()).unwrap();
    assert_eq!(assigned["requests"].as_array().unwrap().len(), 1);
    // Assignment changes invalidate cached personalized catalogs, even when
    // they eventually restore the same enabled set.
    let invalidated: Value =
        serde_json::from_str(&engine.plan_home("default".into()).unwrap()).unwrap();
    assert_eq!(invalidated["requests"].as_array().unwrap().len(), 1);
    let cache_again = json!([{"id":invalidated["requests"][0]["id"],"body":r#"{"metas":[{"id":"tt123","type":"movie","name":"Film"}]}"#}]);
    engine
        .project_home(
            invalidated["id"].as_str().unwrap().into(),
            cache_again.to_string(),
        )
        .unwrap();
    drop(engine);
    let reopened = LocalEngine::new(path.to_string_lossy().into(), vault).unwrap();
    let restored: Value =
        serde_json::from_str(&reopened.plan_home("default".into()).unwrap()).unwrap();
    assert!(restored["requests"].as_array().unwrap().is_empty());
    assert_eq!(
        serde_json::from_str::<Value>(&reopened.addons().unwrap())
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    reopened.remove_addon(provider.into()).unwrap();
    assert!(
        serde_json::from_str::<Value>(&reopened.addons().unwrap())
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    drop(reopened);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bounded_home_enrichment_deduplicates_and_caches_authoritative_details_without_losing_resume(){
 let dir=std::env::temp_dir().join(format!("viptv-enrich-{}",uuid::Uuid::new_v4()));
 let engine=LocalEngine::new(dir.join("local.sqlite").to_string_lossy().into(),Arc::new(MemorySecrets::default())).unwrap();
 engine.install_addon("default".into(),"https://addon.example/manifest.json".into(),r#"{"id":"test","name":"Trending","version":"1","resources":["catalog","meta"],"types":["movie"],"catalogs":[{"id":"top","type":"movie"}]}"#.into()).unwrap();
 let profile:ProfileConfiguration=serde_json::from_str(&engine.profile("default".into()).unwrap()).unwrap();
 let card=viptv_engine::stremio::cards(r#"{"metas":[{"id":"tt123","type":"movie","name":"Film","background":"https://image.example/bg.jpg"}]}"#,"stremio").unwrap().remove(0);
 engine.record_progress("default".into(),profile.scope.revision,serde_json::to_string(&card).unwrap(),10000,100000).unwrap();
 engine.save_favorite("default".into(),profile.scope.revision,serde_json::to_string(&card).unwrap()).unwrap();
 let plan:Value=serde_json::from_str(&engine.plan_home("default".into()).unwrap()).unwrap();
 let home=engine.project_home(plan["id"].as_str().unwrap().into(),json!([{"id":plan["requests"][0]["id"],"body":r#"{"metas":[{"id":"tt123","type":"movie","name":"Film","background":"https://image.example/bg.jpg"}]}"#}]).to_string()).unwrap();
 let hydration:Value=serde_json::from_str(&engine.plan_home_enrichment("default".into(),home).unwrap()).unwrap();assert_eq!(hydration["requests"].as_array().unwrap().len(),1);
 let final_home=engine.project_home_enrichment(hydration["operation"].as_str().unwrap().into(),json!([{"operation":hydration["requests"][0]["operation"],"body":r#"{"meta":{"id":"tt123","name":"Film","description":"Full description","logo":"https://image.example/logo.png","releaseInfo":"2026","runtime":"120 min","genres":["Drama"]}}"#}]).to_string()).unwrap();
 let final_value:Value=serde_json::from_str(&final_home).unwrap();assert_eq!(final_value["hero"]["description"],"Full description");
 let resume=&final_value["rows"][0]["cards"][0];assert_eq!(resume["progress"],0.1);assert_eq!(resume["subtitle"],"2m left");assert_eq!(resume["logo"],"https://image.example/logo.png");assert_eq!(resume["metadata"]["runtime_minutes"],120);
 let library:Value=serde_json::from_str(&engine.library("default".into(),0,40).unwrap()).unwrap();assert_eq!(library["cards"][0]["logo"],"https://image.example/logo.png");assert_eq!(library["cards"][0]["description"],"Full description");assert!(library["cards"][0]["progress"].is_null());assert!(library["cards"][0]["added_at_millis"].as_u64().is_some());
 assert!(engine.cached_detail("default".into(),"movie".into(),"tt123".into(),0).unwrap().is_some());assert!(engine.cached_detail("default".into(),"movie".into(),"tt123".into(),24).unwrap().is_none());
 let cached_plan:Value=serde_json::from_str(&engine.plan_home("default".into()).unwrap()).unwrap();let prepared:Value=serde_json::from_str(&engine.prepared_home(cached_plan["id"].as_str().unwrap().into()).unwrap()).unwrap();for row in prepared["rows"].as_array().unwrap(){for c in row["cards"].as_array().unwrap(){assert_eq!(c["description"],"Full description");}}
 let warm:Value=serde_json::from_str(&engine.plan_home_enrichment("default".into(),final_home).unwrap()).unwrap();assert!(warm["requests"].as_array().unwrap().is_empty());
 assert_eq!(engine.project_home_enrichment(warm["operation"].as_str().unwrap().into(),"[]".into()).is_ok(),true);
 drop(engine);std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn featured_batch_enriches_five_and_deduplicates_continue_parent_in_same_request_set(){
 let dir=std::env::temp_dir().join(format!("viptv-featured-{}",uuid::Uuid::new_v4()));let engine=LocalEngine::new(dir.join("local.sqlite").to_string_lossy().into(),Arc::new(MemorySecrets::default())).unwrap();engine.install_addon("default".into(),"https://addon.example/manifest.json".into(),r#"{"id":"test","name":"Trending","version":"1","resources":["catalog","meta"],"types":["movie"],"catalogs":[{"id":"top","type":"movie"}]}"#.into()).unwrap();
 let profile:ProfileConfiguration=serde_json::from_str(&engine.profile("default".into()).unwrap()).unwrap();let metas=(1..=5).map(|i|json!({"id":format!("tt{i}"),"type":"movie","name":format!("Film {i}"),"background":"https://images.example/bg.jpg"})).collect::<Vec<_>>();let cards=viptv_engine::stremio::cards(&json!({"metas":metas}).to_string(),"stremio").unwrap();engine.record_progress("default".into(),profile.scope.revision,serde_json::to_string(&cards[0]).unwrap(),10000,100000).unwrap();let plan:Value=serde_json::from_str(&engine.plan_home("default".into()).unwrap()).unwrap();let home=engine.project_home(plan["id"].as_str().unwrap().into(),json!([{"id":plan["requests"][0]["id"],"body":json!({"metas":metas}).to_string()}]).to_string()).unwrap();let home:Value=serde_json::from_str(&home).unwrap();assert_eq!(home["featured"].as_array().unwrap().len(),5);assert_eq!(home["hero"],home["featured"][0]);
 let batch:Value=serde_json::from_str(&engine.plan_home_enrichment("default".into(),home.to_string()).unwrap()).unwrap();assert_eq!(batch["requests"].as_array().unwrap().len(),5);let bodies=batch["requests"].as_array().unwrap().iter().enumerate().map(|(i,r)|json!({"operation":r["operation"],"body":json!({"meta":{"id":format!("tt{}",i+1),"name":format!("Film {}",i+1),"description":"Full description","logo":"https://images.example/logo.png"}}).to_string()})).collect::<Vec<_>>();let projected:Value=serde_json::from_str(&engine.project_home_enrichment(batch["operation"].as_str().unwrap().into(),serde_json::to_string(&bodies).unwrap()).unwrap()).unwrap();assert_eq!(projected["hero"],projected["featured"][0]);for card in projected["featured"].as_array().unwrap(){assert_eq!(card["description"],"Full description");assert_eq!(card["logo"],"https://images.example/logo.png");}assert_eq!(projected["rows"][0]["cards"][0]["progress"],0.1);drop(engine);std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn native_cached_detail_pages_new_seasons_from_one_scoped_body_and_validates_authority(){
 let dir=std::env::temp_dir().join(format!("viptv-season-cache-{}",uuid::Uuid::new_v4()));let engine=LocalEngine::new(dir.join("local.sqlite").to_string_lossy().into(),Arc::new(MemorySecrets::default())).unwrap();
 engine.install_addon("default".into(),"https://addon.example/manifest.json".into(),r#"{"id":"test","name":"Provider","version":"1","resources":["meta"],"types":["series"]}"#.into()).unwrap();
 let provider=serde_json::from_str::<Value>(&engine.addons().unwrap()).unwrap()[0]["id"].as_str().unwrap().to_owned();let plan:Value=serde_json::from_str(&engine.plan_detail("default".into(),"series".into(),"tt123".into(),provider.clone()).unwrap()).unwrap();
 let videos=(1..=2).flat_map(|season|(1..=30).map(move |episode|json!({"id":format!("episode-{season}-{episode}"),"name":"Episode","season":season,"episode":episode}))).collect::<Vec<_>>();let body=json!({"meta":{"id":"tt123","name":"Series","type":"series","videos":videos}}).to_string();engine.project_detail(plan["operation"].as_str().unwrap().into(),body,0).unwrap();
 let page:Value=serde_json::from_str(&engine.cached_detail("default".into(),"series".into(),"tt123".into(),30).unwrap().unwrap()).unwrap();assert_eq!(page["episodes"][0]["identity"]["episode"]["season"],2);assert_eq!(page["episodes"][0]["identity"]["episode"]["episode"],1);assert_eq!(page["seasons"][1]["offset"],30);assert_eq!(page["episodes"].as_array().unwrap().len(),24);
 assert!(engine.verified_cards.lock().unwrap().contains_key(&("default".into(),provider.clone(),"series".into(),"episode-2-1".into())));
 engine.remove_addon(provider).unwrap();assert!(engine.cached_detail("default".into(),"series".into(),"tt123".into(),30).unwrap().is_none());drop(engine);std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn native_detail_page_limit_is_part_of_cache_identity_and_old_calls_keep_24(){
 let dir=std::env::temp_dir().join(format!("viptv-limit-cache-{}",uuid::Uuid::new_v4()));let engine=LocalEngine::new(dir.join("local.sqlite").to_string_lossy().into(),Arc::new(MemorySecrets::default())).unwrap();engine.create_profile("other".into(),"Other".into()).unwrap();
 engine.install_addon("default".into(),"https://addon.example/manifest.json".into(),r#"{"id":"test","name":"Provider","version":"1","resources":["meta"],"types":["series"]}"#.into()).unwrap();let provider=serde_json::from_str::<Value>(&engine.addons().unwrap()).unwrap()[0]["id"].as_str().unwrap().to_owned();let plan:Value=serde_json::from_str(&engine.plan_detail("default".into(),"series".into(),"tt9999999".into(),provider.clone()).unwrap()).unwrap();let videos=(1..=1500).map(|episode|json!({"id":format!("opaque-{episode}"),"season":1,"episode":episode,"title":"Episode"})).collect::<Vec<_>>();let body=json!({"meta":{"id":"tt9999999","name":"Series","type":"series","videos":videos}}).to_string();
 let operation=plan["operation"].as_str().unwrap().to_string();assert!(engine.project_detail_page(operation.clone(),body.clone(),0,129).is_err());let wide:Value=serde_json::from_str(&engine.project_detail_page(operation,body,0,96).unwrap()).unwrap();assert_eq!(wide["episodes"].as_array().unwrap().len(),96);
 let old:Value=serde_json::from_str(&engine.cached_detail("default".into(),"series".into(),"tt9999999".into(),0).unwrap().unwrap()).unwrap();assert_eq!(old["episodes"].as_array().unwrap().len(),24);assert_eq!(old["next_cursor"],"24");let page:Value=serde_json::from_str(&engine.detail_page("default".into(),"series".into(),"tt9999999".into(),96,128).unwrap().unwrap()).unwrap();assert_eq!(page["episodes"].as_array().unwrap().len(),128);assert_eq!(page["next_cursor"],"224");assert!(engine.verified_cards.lock().unwrap().contains_key(&("default".into(),provider.clone(),"series".into(),"opaque-224".into())));
 assert!(engine.detail_page("other".into(),"series".into(),"tt9999999".into(),96,128).unwrap().is_none());assert!(engine.detail_page("default".into(),"series".into(),"tt9999999".into(),0,0).is_err());engine.remove_addon(provider).unwrap();assert!(engine.detail_page("default".into(),"series".into(),"tt9999999".into(),96,128).unwrap().is_none());drop(engine);std::fs::remove_dir_all(dir).unwrap();
}
#[test]
#[ignore="reads a private local capture of public One Piece metadata, not user state"]
fn real_one_piece_native_paging_reuses_one_document(){
 let path=std::env::var("VIPTV_QA_METADATA_DOCUMENT").unwrap();let body=std::fs::read_to_string(path).unwrap();let data:Value=serde_json::from_str(&body).unwrap();let meta=&data["meta"];assert_eq!(meta["name"],"One Piece");let id=meta["id"].as_str().unwrap();
 let dir=std::env::temp_dir().join(format!("viptv-one-piece-cache-{}",uuid::Uuid::new_v4()));let engine=LocalEngine::new(dir.join("local.sqlite").to_string_lossy().into(),Arc::new(MemorySecrets::default())).unwrap();engine.install_addon("default".into(),"https://fixture.example/manifest.json".into(),r#"{"id":"fixture","name":"Metadata","version":"1","resources":["meta"],"types":["series"]}"#.into()).unwrap();let plan:Value=serde_json::from_str(&engine.plan_detail("default".into(),"series".into(),id.into(),"".into()).unwrap()).unwrap();let first:Value=serde_json::from_str(&engine.project_detail(plan["operation"].as_str().unwrap().into(),body,0).unwrap()).unwrap();assert_eq!(first["episodes"].as_array().unwrap().len(),24);
 let total=first["seasons"].as_array().unwrap().iter().map(|s|s["episode_count"].as_u64().unwrap() as usize).sum::<usize>();assert!(total>1000);let mut loaded=24;let mut offset=24;let mut pages=0;let mut ids=first["episodes"].as_array().unwrap().iter().map(|e|e["identity"]["id"].as_str().unwrap().to_owned()).collect::<std::collections::HashSet<_>>();loop{let page:Value=serde_json::from_str(&engine.detail_page("default".into(),"series".into(),id.into(),offset,96).unwrap().unwrap()).unwrap();let episodes=page["episodes"].as_array().unwrap();assert!(episodes.len()<=96);assert_eq!(page["seasons"],first["seasons"]);for episode in episodes{assert!(ids.insert(episode["identity"]["id"].as_str().unwrap().to_owned()));}loaded+=episodes.len();pages+=1;let Some(next)=page["next_cursor"].as_str()else{break;};offset=next.parse().unwrap();}assert_eq!(loaded,total);assert_eq!(engine.detail_documents.lock().unwrap().len(),1);println!("Public One Piece native paging: verified_episodes={total},cached_pages={pages},metadata_documents=1");drop(engine);std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn home_row_browse_actions_preserve_opaque_catalog_ids_across_prepared_and_projected_home(){
 let dir=std::env::temp_dir().join(format!("viptv-home-action-{}",uuid::Uuid::new_v4()));let engine=LocalEngine::new(dir.join("local.sqlite").to_string_lossy().into(),Arc::new(MemorySecrets::default())).unwrap();
 let connection:Value=serde_json::from_str(&engine.install_addon("default".into(),"https://catalog.example/manifest.json".into(),json!({"id":"metadata","name":"Metadata","version":"1","resources":["catalog"],"catalogs":[{"id":"staff:curated","type":"movie","name":"Staff"}]}).to_string()).unwrap()).unwrap();
 let plan:Value=serde_json::from_str(&engine.plan_home("default".into()).unwrap()).unwrap();let action=plan["requests"][0]["browse"].clone();assert_eq!(action["target"],"Discover");let values=action["query"].as_array().unwrap();assert!(values.iter().any(|v|v["id"]=="catalog"&&v["values"][0]=="staff:curated"));assert!(values.iter().any(|v|v["id"]=="connection_id"&&v["values"][0]==connection["id"]));
 let prepared:Value=serde_json::from_str(&engine.prepared_home(plan["id"].as_str().unwrap().into()).unwrap()).unwrap();assert_eq!(prepared["rows"][0]["browse"],action);
 let projected:Value=serde_json::from_str(&engine.project_home(plan["id"].as_str().unwrap().into(),json!([{"id":plan["requests"][0]["id"],"body":json!({"metas":[{"id":"opaque-card","type":"movie","name":"Movie"}]}).to_string()}]).to_string()).unwrap()).unwrap();assert_eq!(projected["rows"][0]["browse"],action);
 drop(engine);std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn native_search_retains_each_addon_group_and_profile_enablement() {
    let dir = std::env::temp_dir().join(format!("viptv-search-groups-{}",uuid::Uuid::new_v4()));
    let engine = LocalEngine::new(dir.join("local.sqlite").to_string_lossy().into(),Arc::new(MemorySecrets::default())).unwrap();
    let manifest = r#"{"id":"search-fixture","name":"Same provider name","version":"1","resources":["catalog","meta"],"types":["movie"],"catalogs":[{"id":"search","type":"movie","extra":[{"name":"search","isRequired":true}]}]}"#;
    let mut providers=Vec::new();
    for host in ["first.example","second.example"] {
        let installed:Value=serde_json::from_str(&engine.install_addon("default".into(),format!("https://{host}/manifest.json"),manifest.into()).unwrap()).unwrap();
        providers.push(installed["id"].as_str().unwrap().to_owned());
    }
    let plan:Value=serde_json::from_str(&engine.plan_search("default".into(),"Shared title".into()).unwrap()).unwrap();
    let requests=plan["requests"].as_array().unwrap();assert_eq!(requests.len(),2);
    let results=requests.iter().map(|r|json!({"id":r["id"],"body":r#"{"metas":[{"id":"same-media-id","type":"movie","name":"Shared title"}]}"#})).collect::<Vec<_>>();
    let projected:Value=serde_json::from_str(&engine.project_home(plan["id"].as_str().unwrap().into(),json!(results).to_string()).unwrap()).unwrap();
    let rows=projected["rows"].as_array().unwrap();assert_eq!(rows.len(),2);
    for provider in &providers {
        let row=rows.iter().find(|r|r["provider_id"]==provider.as_str()).unwrap();
        assert_eq!(row["title"],"Same provider name · Movies");assert_eq!(row["state"],"Ready");
        assert_eq!(row["cards"][0]["provider_id"],provider.as_str());
        assert_eq!(row["cards"][0]["identity"]["id"],"same-media-id");
    }
    engine.set_addon_enabled("default".into(),providers[1].clone(),false).unwrap();
    let plan:Value=serde_json::from_str(&engine.plan_search("default".into(),"Shared title".into()).unwrap()).unwrap();
    assert_eq!(plan["requests"].as_array().unwrap().len(),1);
    assert_eq!(plan["requests"][0]["provider_id"],providers[0]);
    engine.create_profile("other".into(),"Other".into()).unwrap();
    let other:Value=serde_json::from_str(&engine.plan_search("other".into(),"Shared title".into()).unwrap()).unwrap();
    assert!(other["requests"].as_array().unwrap().is_empty());
    drop(engine);std::fs::remove_dir_all(dir).unwrap();
}
