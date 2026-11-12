use super::*;
use crate::{auth_integration_tests::fixture, test_support::request};

struct Duplicate {
    app: App,
    task: tokio::task::JoinHandle<()>,
    card: Card,
}
impl Drop for Duplicate {
    fn drop(&mut self) { self.task.abort(); }
}

async fn duplicate(scoped_newer: bool) -> Duplicate {
    let title = json!({"title":"Owned continuation","runtime":2,"ids":{"simkl":20}});
    let episodes = json!([
        {"title":"Owned previous","season":1,"episode":1,"aired":true,"ids":{"simkl":29}},
        {"title":"Owned current","season":1,"episode":2,"aired":true,"ids":{"simkl":30}},
        {"title":"Owned next","season":1,"episode":3,"aired":true,"ids":{"simkl":31}}
    ]);
    let parent = viptv_engine::simkl::card(&title, "tv").unwrap();
    let mut card = viptv_engine::simkl::episodes(&episodes, &parent).unwrap().remove(1);
    card.metadata_origin = Some(viptv_engine::origin::MetadataOrigin {
        provider_id: "simkl".into(), connection_id: None, incarnation: None,
    });
    let routes = Router::new()
        .route("/tv/20", axum::routing::get(move || { let title=title.clone(); async move { axum::Json(title) } }))
        .route("/tv/episodes/20", axum::routing::get(move || { let episodes=episodes.clone(); async move { axum::Json(episodes) } }));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address=listener.local_addr().unwrap();
    let task=tokio::spawn(async move { axum::serve(listener,routes).await.unwrap(); });
    let mut app=fixture();
    app.simkl=Arc::new(crate::simkl::Service::fixture(&format!("http://{address}")));
    let origin=serde_json::to_string(card.metadata_origin.as_ref().unwrap()).unwrap();
    let legacy_at=if scoped_newer { 100 } else { 300 };
    let record=json!({"identity":card.identity,"parent_identity":card.parent_identity,"metadata_origin":card.metadata_origin,
        "provider_id":"simkl","title":"Owned continuation","poster":null,"landscape":null,
        "position_millis":30000,"duration_millis":100000,"updated_at_millis":200000,"completed_at_millis":null,"presentation":card});
    let context=json!({"engine_identity":card.identity,"engine_parent_identity":card.parent_identity,
        "engine_provider_id":"simkl","series_id":parent.identity.id,"season":1,"episode":2});
    {
        let db=app.db.lock().unwrap();
        db.execute("INSERT INTO progress(profile_id,id,type,name,position,duration,updated_at,context,title_id) VALUES(1,?1,'series','Owned continuation',10,100,?2,?3,?4)",params![card.identity.id,legacy_at,context.to_string(),parent.identity.id]).unwrap();
        db.execute("INSERT INTO engine_scoped_records(profile_id,record_kind,identity_scope,kind,identity_id,lookup_origin,lookup_id,lookup_identity,value,added_at_millis,updated_at_millis,verified) VALUES(1,'playback','shared','series',?1,?2,?1,?3,?4,NULL,200000,1)",params![card.identity.id,origin,serde_json::to_string(&card.identity).unwrap(),record.to_string()]).unwrap();
    }
    Duplicate { app, task, card }
}

async fn call(app: &App, method: &str, path: &str, value: Value) -> Value {
    let (status,value)=request(app,"member-token-1",method,path,value).await;
    assert_eq!(status,StatusCode::OK,"{path}: {value}"); value
}

#[tokio::test]
async fn legacy_and_scoped_resume_coalesce_with_one_authoritative_clock_across_public_views() {
    for scoped_newer in [true,false] {
        let fixture=duplicate(scoped_newer).await;
        let expected=if scoped_newer { 30000 } else { 10000 };
        let page=call(&fixture.app,"GET","/api/profiles/1/continue",Value::Null).await;
        let entries=page["entries"].as_array().unwrap();
        assert_eq!(entries.len(),1,"same served Simkl episode must have one continuation: {page}");
        let entry=&entries[0];
        assert_eq!(entry["status"],"Resume");
        assert_eq!(entry["record"]["position_millis"],expected);
        assert_eq!(entry["card"]["identity"],serde_json::to_value(&fixture.card.identity).unwrap());
        assert_eq!(entry["card"]["metadata_origin"],serde_json::to_value(&fixture.card.metadata_origin).unwrap());
        let home=call(&fixture.app,"GET","/api/profiles/1/home",Value::Null).await;
        let cards=home["rows"].as_array().unwrap().iter().find(|r|r["id"]=="continue").unwrap()["cards"].as_array().unwrap();
        assert_eq!(cards.len(),1,"{home}");
        assert_eq!(cards[0]["identity"],entry["card"]["identity"]);
        assert_eq!(cards[0]["view"]["preview"]["progress"],expected as f64/100000.0);
        let action=call(&fixture.app,"POST","/api/profiles/1/viewing-action",json!({"card":cards[0]})).await;
        assert_eq!(action["resume"],true);
        assert_eq!(action["card"]["identity"],entry["card"]["identity"]);
        assert_eq!(action["progress"],cards[0]["view"]["preview"]["progress"]);
        assert_eq!(action["remaining_label"],"2m left");
        let db=fixture.app.db.lock().unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM progress",[],|r|r.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(db.query_row("SELECT count(*) FROM engine_scoped_records WHERE record_kind='playback'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    }
}

#[tokio::test]
async fn verified_import_alias_keeps_one_resume_across_storage_and_lookup_ids() {
    let fixture=duplicate(true).await;
    let mut parent=fixture.card.clone();
    parent.identity=parent.parent_identity.take().unwrap();
    {
        let db=fixture.app.db.lock().unwrap();
        db.execute("INSERT INTO simkl_import_entries(profile_id,remote_id,remote_kind,local_id,local_kind,card,statistics,link_revision) VALUES(1,'simkl:tv:20','series','tt2020','series',?1,'{}',0)",[serde_json::to_string(&parent).unwrap()]).unwrap();
        db.execute("UPDATE engine_scoped_records SET identity_id='tt2020:1:2' WHERE record_kind='playback'",[]).unwrap();
    }
    let page=call(&fixture.app,"GET","/api/profiles/1/continue?limit=1",Value::Null).await;
    assert_eq!(page["entries"].as_array().unwrap().len(),1);
    assert_eq!(page["complete"],true,"coalescing precedes pagination: {page}");
    assert_eq!(page["entries"][0]["record"]["position_millis"],30000);
    assert_eq!(page["entries"][0]["card"]["identity"],serde_json::to_value(&fixture.card.identity).unwrap());
}

#[tokio::test]
async fn real_episode_resume_and_active_rewatch_beat_a_stale_next_anchor_for_that_episode() {
    for rewatch in [false,true] {
        let fixture=duplicate(false).await;
        let mut previous=fixture.card.identity.clone();
        previous.id="simkl:tv:20:1:1".into();
        previous.episode.as_mut().unwrap().episode=1;
        previous.ids[0].value="29".into();
        let context=json!({"engine_identity":previous,"engine_parent_identity":fixture.card.parent_identity,
            "engine_provider_id":"simkl","series_id":"simkl:tv:20","season":1,"episode":1,"watched_override":true});
        {
            let db=fixture.app.db.lock().unwrap();
            db.execute("UPDATE progress SET id=?1,position=100,duration=100,context=?2",params![previous.id,context.to_string()]).unwrap();
            if rewatch {
                db.execute("UPDATE engine_scoped_records SET value=json_set(value,'$.completed_at_millis',1000) WHERE record_kind='playback'",[]).unwrap();
            }
        }
        let next=call(&fixture.app,"POST","/api/profiles/1/continue/next",json!({"identity":previous,"parent_identity":fixture.card.parent_identity})).await;
        assert_eq!(next["status"],"Next","{next}");
        let page=call(&fixture.app,"GET","/api/profiles/1/continue",Value::Null).await;
        assert_eq!(page["entries"].as_array().unwrap().len(),1,"{page}");
        let entry=&page["entries"][0];
        assert_eq!(entry["status"],"Resume","{page}");
        assert_eq!(entry["record"]["position_millis"],30000);
        assert_eq!(entry["record"]["completed_at_millis"],if rewatch {json!(1000)} else {Value::Null});
        assert_eq!(entry["card"]["identity"],serde_json::to_value(&fixture.card.identity).unwrap());
    }
}

#[tokio::test]
async fn episode_order_specials_and_editions_remain_separate_continuations() {
    for variant in ["order","special","edition","parent_edition"] {
        let fixture=duplicate(true).await;
        let mut identity=fixture.card.identity.clone();
        let mut parent=fixture.card.parent_identity.clone().unwrap();
        match variant {
            "order" => identity.episode.as_mut().unwrap().order="absolute".into(),
            "special" => {identity.id="simkl:tv:20:0:2".into();identity.episode.as_mut().unwrap().season=Some(0);},
            "edition" => identity.edition=Some("alternate cut".into()),
            _ => parent.edition=Some("alternate cut".into()),
        }
        let coordinates=identity.episode.as_ref().unwrap();
        let context=json!({"engine_identity":identity,"engine_parent_identity":parent,"engine_provider_id":"simkl",
            "series_id":parent.id,"season":coordinates.season,"episode":coordinates.episode});
        fixture.app.db.lock().unwrap().execute("UPDATE progress SET id=?1,context=?2",params![identity.id,context.to_string()]).unwrap();
        let page=call(&fixture.app,"GET","/api/profiles/1/continue",Value::Null).await;
        assert_eq!(page["entries"].as_array().unwrap().len(),2,"{variant}: {page}");
    }
}

#[tokio::test]
async fn newer_completion_of_a_different_episode_does_not_restore_an_older_resume() {
    let fixture=duplicate(false).await;
    let raw:String=fixture.app.db.lock().unwrap().query_row("SELECT value FROM engine_scoped_records WHERE record_kind='playback'",[],|r|r.get(0)).unwrap();
    let mut record:Value=serde_json::from_str(&raw).unwrap();
    let mut card=fixture.card.clone(); card.identity.id="simkl:tv:20:1:3".into();
    card.identity.episode.as_mut().unwrap().episode=3; card.identity.ids[0].value="31".into();
    record["identity"]=serde_json::to_value(&card.identity).unwrap(); record["presentation"]=serde_json::to_value(&card).unwrap();
    record["position_millis"]=json!(100000);record["updated_at_millis"]=json!(400000);record["completed_at_millis"]=json!(400000);
    fixture.app.db.lock().unwrap().execute("UPDATE engine_scoped_records SET identity_id=?1,lookup_id=?1,lookup_identity=?2,value=?3,updated_at_millis=400000 WHERE record_kind='playback'",params![card.identity.id,serde_json::to_string(&card.identity).unwrap(),record.to_string()]).unwrap();
    let page=call(&fixture.app,"GET","/api/profiles/1/continue",Value::Null).await;
    assert_eq!(page["entries"].as_array().unwrap().len(),1,"{page}");
    assert_eq!(page["entries"][0]["status"],"Pending");
    assert_eq!(page["entries"][0]["card"]["identity"],serde_json::to_value(card.identity).unwrap());
    assert_eq!(page["entries"][0]["record"]["position_millis"],100000);
}
