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
