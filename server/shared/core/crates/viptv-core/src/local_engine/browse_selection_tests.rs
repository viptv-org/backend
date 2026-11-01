use super::*;
#[derive(Default)]
struct Secrets(Mutex<std::collections::HashMap<String, String>>);
impl SecretStore for Secrets {
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
fn native_multichoice_json_preserves_owner_selection_and_one_page_request() {
    let dir = std::env::temp_dir().join(format!("viptv-browse-{}", uuid::Uuid::new_v4()));
    let engine = LocalEngine::new(
        dir.join("engine.sqlite").to_string_lossy().into(),
        Arc::new(Secrets::default()),
    )
    .unwrap();
    let manifest = json!({"id":"fixture","name":"Friendly provider","version":"1","resources":["catalog"],"catalogs":[{"id":"top","type":"movie","extra":[{"name":"genre","isRequired":true,"optionsLimit":2,"options":["A&B","C,D"]},{"name":"skip"}]}]});
    let connection: serde_json::Value = serde_json::from_str(
        &engine
            .install_addon(
                "default".into(),
                "https://addon.example/manifest.json".into(),
                manifest.to_string(),
            )
            .unwrap(),
    )
    .unwrap();
    let controls: serde_json::Value = serde_json::from_str(
        &engine
            .discovery_controls("default".into(), "movie".into())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        controls["catalogs"][0]["controls"][0]["kind"],
        "MultiChoice"
    );
    assert_eq!(
        controls["catalogs"][0]["controls"][0]["maximum_selections"],
        2
    );
    assert_eq!(
        controls["catalogs"][0]["connection_name"],
        "Friendly provider"
    );
    let query = json!({"type":"movie","catalog":"top","connection_id":connection["id"],"skip":96,"values":[{"id":"genre","values":["A&B","C,D"]}]});
    let plan: serde_json::Value = serde_json::from_str(
        &engine
            .plan_discover("default".into(), query.to_string())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(plan["requests"].as_array().unwrap().len(), 1);
    let url = plan["requests"][0]["url"].as_str().unwrap();
    assert!(url.contains("skip=96&genre=A%26B&genre=C%2CD"));
    let response = json!([{"id":plan["requests"][0]["id"],"body":json!({"metas":[{"id":"tt123","type":"movie","name":"Film"}]}).to_string()}]);
    let page: serde_json::Value = serde_json::from_str(
        &engine
            .project_discover(plan["id"].as_str().unwrap().into(), response.to_string())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(page["next_cursor"], "97");
    for (key, value) in [
        ("genre", json!("A&B")),
        ("connection_id", json!("foreign")),
        ("catalog", json!("foreign")),
        ("values", json!([{"id":"genre","values":["A&B","A&B"]}])),
    ] {
        let mut bad = query.clone();
        bad[key] = value;
        assert!(
            engine
                .plan_discover("default".into(), bad.to_string())
                .is_err()
        );
    }
    assert!(
        engine
            .plan_discover("foreign-profile".into(), query.to_string())
            .is_err()
    );
    drop(engine);
    std::fs::remove_dir_all(dir).unwrap();
}
