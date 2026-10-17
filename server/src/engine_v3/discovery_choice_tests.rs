use crate::{auth_integration_tests::fixture, test_support::request, *};
use std::sync::atomic::{AtomicUsize, Ordering};
fn uri(values: Value, extras: Option<Value>, genre: Option<&str>, addon: i64, catalog: &str, skip: usize) -> String {
    let mut pairs=url::form_urlencoded::Serializer::new(String::new());
    pairs.append_pair("type","movie").append_pair("addon_id",&addon.to_string()).append_pair("catalog",catalog).append_pair("skip",&skip.to_string()).append_pair("values",&values.to_string());
    if let Some(extras)=extras {pairs.append_pair("extras",&extras.to_string());}
    if let Some(genre)=genre {pairs.append_pair("genre",genre);}
    format!("/api/discover?{}",pairs.finish())
}
#[tokio::test]
async fn discovery_multi_choices_preserve_controls_owner_and_validate_before_provider_io() {
    let reads=Arc::new(AtomicUsize::new(0));
    let seen=Arc::new(Mutex::new(Vec::new()));
    let (count,paths)=(reads.clone(),seen.clone());
    let mock=Router::new().fallback(move |url:axum::http::Uri| {
        let (count,paths)=(count.clone(),paths.clone());
        async move {count.fetch_add(1,Ordering::SeqCst);paths.lock().unwrap().push(url.path().to_owned());axum::Json(json!({"metas":[{"id":"tt1234567","type":"movie","name":"Film"}]}))}
    });
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address=listener.local_addr().unwrap();
    let server=tokio::spawn(async move {axum::serve(listener,mock).await.unwrap()});
    let mut app=fixture();app.addons.allow_test_loopback=true;
    let manifest=json!({"id":"fixture","name":"Friendly provider","resources":["catalog","meta"],"types":["movie"],"catalogs":[{"id":"top","name":"Popular","type":"movie","extra":[{"name":"genre","isRequired":true,"optionsLimit":2,"options":["A&B","C,D","Comedy"]},{"name":"sort","isRequired":true,"options":["rating","year"]},{"name":"skip"}]}]});
    {
        let db=app.db.lock().unwrap();
        for (id,account) in [(1,1),(2,2)] {
            db.execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(?1,'Friendly provider',?2,?3,?4)",params![id,format!("http://{address}/manifest.json"),manifest.to_string(),account]).unwrap();
        }
    }
    crate::test_support::encrypt_fixture_sources(&app);
    let(status,body)=request(&app,"member-token-1","PUT","/api/profiles/1/integrations",json!({"revision":0,"browsing_plugin":"stremio","assignments":[{"connection_id":"addon:1","enabled":["Metadata","Discovery"]}]})).await;
    assert_eq!(status,StatusCode::OK,"{body}");
    let(status,controls)=request(&app,"member-token-1","GET","/api/discovery-controls?type=movie",Value::Null).await;
    assert_eq!(status,StatusCode::OK,"{controls}");
    let catalog=&controls["catalogs"][0];
    assert_eq!(catalog["connection_name"],"Friendly provider");
    assert_eq!(catalog["controls"][0]["kind"],"MultiChoice");
    assert_eq!(catalog["controls"][0]["required"],true);
    assert_eq!(catalog["controls"][0]["maximum_selections"],2);
    assert_eq!(catalog["controls"][1]["required"],true);
    assert_eq!(reads.load(Ordering::SeqCst),0);
    let values=json!([{"id":"genre","values":["A&B","C,D"]}]);
    let sort=Some(json!({"sort":"year"}));
    let(status,page)=request(&app,"member-token-1","GET",&uri(values.clone(),sort.clone(),None,1,"top",24),Value::Null).await;
    assert_eq!(status,StatusCode::OK,"{page}");assert_eq!(page["next_cursor"],"25");
    assert_eq!(page["cards"][0]["metadata_origin"]["connection_id"],"addon:1");
    assert_eq!(reads.load(Ordering::SeqCst),1);
    assert!(seen.lock().unwrap()[0].contains("skip=24&genre=A%26B&genre=C%2CD&sort=year"));
    let(status,page)=request(&app,"member-token-1","GET",&uri(values.clone(),sort.clone(),None,1,"top",25),Value::Null).await;
    assert_eq!(status,StatusCode::OK,"{page}");assert_eq!(page["next_cursor"],"26");
    assert_eq!(reads.load(Ordering::SeqCst),2);
    let mut variants=Vec::new();
    variants.push(uri(values.clone(),sort.clone(),Some("Comedy"),1,"top",0));
    variants.push(uri(values.clone(),sort.clone(),None,2,"top",0));
    variants.push(uri(values.clone(),sort.clone(),None,1,"foreign",0));
    variants.push(uri(values.clone(),None,None,1,"top",0));
    for values in [json!([]),json!([{"id":"genre","values":["A&B","C,D","Comedy"]}]),json!([{"id":"genre","values":["A&B","A&B"]}]),json!([{"id":"genre","values":["A&B"]},{"id":"genre","values":["C,D"]}]),json!([{"id":"foreign","values":["x"]}])] {
        variants.push(uri(values,sort.clone(),None,1,"top",0));
    }
    for uri in variants {let(status,_)=request(&app,"member-token-1","GET",&uri,Value::Null).await;assert_eq!(status,StatusCode::BAD_REQUEST,"{uri}");}
    assert_eq!(reads.load(Ordering::SeqCst),2,"invalid queries must never fetch");
    let(status,_)=request(&app,"member-token-1","GET",&uri(json!([]),sort,Some("Comedy"),1,"top",0),Value::Null).await;
    assert_eq!(status,StatusCode::OK);assert_eq!(reads.load(Ordering::SeqCst),3,"old scalar request remains one fetch");
    server.abort();
}
#[tokio::test]
async fn simkl_never_silently_ignores_array_filters_or_malformed_values() {
    let app=fixture();
    for values in ["{}","[{}]","[{\"id\":\"genre\",\"values\":[\"drama\"]}]"] {
        let query=url::form_urlencoded::Serializer::new(String::new()).append_pair("type","movie").append_pair("values",values).finish();
        let(status,_)=request(&app,"member-token-1","GET",&format!("/api/discover?{query}"),Value::Null).await;
        assert_eq!(status,StatusCode::BAD_REQUEST);
    }
}
