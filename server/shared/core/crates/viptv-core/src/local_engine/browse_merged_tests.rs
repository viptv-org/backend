use super::*;
use serde_json::Value;
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
struct Fixture {
    dir: std::path::PathBuf,
    engine: LocalEngine,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("viptv-merged-browse-{}", uuid::Uuid::new_v4()));
        let engine = LocalEngine::new(
            dir.join("engine.sqlite").to_string_lossy().into(),
            Arc::new(Secrets::default()),
        )
        .unwrap();
        Self { dir, engine }
    }
    fn install(&self, url: &str, catalog: Value) -> String {
        let manifest = json!({"id":"fixture","name":"Provider","version":"1","resources":["catalog"],"catalogs":[catalog]});
        let connection: Value = serde_json::from_str(
            &self
                .engine
                .install_addon("default".into(), url.into(), manifest.to_string())
                .unwrap(),
        )
        .unwrap();
        connection["id"].as_str().unwrap().into()
    }
    fn plan(&self, query: Value) -> Value {
        serde_json::from_str(
            &self
                .engine
                .plan_discover("default".into(), query.to_string())
                .unwrap(),
        )
        .unwrap()
    }
    fn cursor(&self, cursor: &str) -> Value {
        self.plan(json!({"type":"movie","cursor":cursor}))
    }
}
fn pageable_catalog() -> Value {
    json!({"id":"twenty","type":"movie","name":"Twenty","extra":[{"name":"skip"}]})
}
fn single_page_catalog() -> Value {
    json!({"id":"ten","type":"movie","name":"Ten"})
}
fn request<'a>(plan: &'a Value, provider: &str) -> &'a Value {
    plan["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|request| request["provider_id"] == provider)
        .unwrap()
}
fn skip(request: &Value) -> Option<&str> {
    let url = request["url"].as_str().unwrap();
    let rest = url.split("skip=").nth(1)?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    Some(&rest[..end])
}
fn page(fixture: &Fixture, plan: &Value, metas: impl Fn(&str) -> Vec<Value>) -> Value {
    let results: Vec<Value> = plan["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|request| {
            json!({"id":request["id"],"body":json!({"metas":metas(request["provider_id"].as_str().unwrap())}).to_string()})
        })
        .collect();
    serde_json::from_str(
        &fixture
            .engine
            .project_discover(
                plan["id"].as_str().unwrap().into(),
                Value::Array(results).to_string(),
            )
            .unwrap(),
    )
    .unwrap()
}
fn metas(prefix: &str, range: std::ops::Range<usize>) -> Vec<Value> {
    range
        .map(|n| json!({"id":format!("{prefix}{n}"),"type":"movie","name":format!("{prefix}{n}")}))
        .collect()
}
fn ids(page: &Value) -> Vec<String> {
    page["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|card| card["identity"]["id"].as_str().unwrap().to_string())
        .collect()
}
#[test]
fn merged_browse_carries_every_catalog_cursor() {
    let fixture = Fixture::new();
    let pageable = fixture.install("https://pageable.example/manifest.json", pageable_catalog());
    let single = fixture.install("https://single.example/manifest.json", single_page_catalog());
    let first = fixture.plan(json!({"type":"movie"}));
    assert_eq!(first["requests"].as_array().unwrap().len(), 2);
    assert_eq!(skip(request(&first, &pageable)), Some("0"));
    assert_eq!(skip(request(&first, &single)), None);
    let first_page = page(&fixture, &first, |provider| {
        if provider == pageable {
            metas("a", 0..20)
        } else {
            metas("b", 0..10)
        }
    });
    assert_eq!(first_page["cards"].as_array().unwrap().len(), 30);
    assert_eq!(first_page["complete"], false);
    let cursor = first_page["next_cursor"].as_str().unwrap().to_string();
    assert!(
        cursor.parse::<u32>().is_err(),
        "a merged page must not publish one catalog's offset: {cursor}"
    );
    let second = fixture.cursor(&cursor);
    assert_eq!(second["requests"].as_array().unwrap().len(), 1);
    assert_eq!(second["requests"][0]["provider_id"], pageable.as_str());
    assert_eq!(skip(&second["requests"][0]), Some("20"));
    let second_page = page(&fixture, &second, |_| metas("a", 20..40));
    assert_eq!(second_page["complete"], false);
    assert_eq!(
        ids(&second_page),
        (20..40).map(|n| format!("a{n}")).collect::<Vec<_>>()
    );
    let third = fixture.cursor(second_page["next_cursor"].as_str().unwrap());
    assert_eq!(third["requests"].as_array().unwrap().len(), 1);
    assert_eq!(skip(&third["requests"][0]), Some("40"));
    let third_page = page(&fixture, &third, |_| Vec::new());
    assert_eq!(third_page["cards"].as_array().unwrap().len(), 0);
    assert_eq!(third_page["complete"], true);
    assert_eq!(third_page["next_cursor"], Value::Null);
    let mut all = ids(&first_page);
    all.extend(ids(&second_page));
    let mut expected: Vec<String> = (0..40).map(|n| format!("a{n}")).collect();
    expected.extend((0..10).map(|n| format!("b{n}")));
    all.sort();
    expected.sort();
    assert_eq!(all, expected, "no catalog repeats or skips an entry");
}
#[test]
fn merged_browse_pages_the_same_way_whichever_catalog_finishes_last() {
    let fixture = Fixture::new();
    let single = fixture.install("https://single.example/manifest.json", single_page_catalog());
    let pageable = fixture.install("https://pageable.example/manifest.json", pageable_catalog());
    let first = fixture.plan(json!({"type":"movie"}));
    assert_eq!(first["requests"].as_array().unwrap().len(), 2);
    assert_eq!(skip(request(&first, &single)), None);
    let first_page = page(&fixture, &first, |provider| {
        if provider == pageable {
            metas("a", 0..20)
        } else {
            metas("b", 0..10)
        }
    });
    assert_eq!(first_page["complete"], false);
    let second = fixture.cursor(first_page["next_cursor"].as_str().unwrap());
    assert_eq!(second["requests"].as_array().unwrap().len(), 1);
    assert_eq!(second["requests"][0]["provider_id"], pageable.as_str());
    assert_eq!(skip(&second["requests"][0]), Some("20"));
}
#[test]
fn merged_browse_retries_a_catalog_that_did_not_answer() {
    let fixture = Fixture::new();
    let pageable = fixture.install("https://pageable.example/manifest.json", pageable_catalog());
    let single = fixture.install("https://single.example/manifest.json", single_page_catalog());
    let first = fixture.plan(json!({"type":"movie"}));
    let results = json!([
        {"id":request(&first, &pageable)["id"],"body":json!({"metas":metas("a", 0..20)}).to_string()},
        {"id":request(&first, &single)["id"]}
    ]);
    let first_page: Value = serde_json::from_str(
        &fixture
            .engine
            .project_discover(first["id"].as_str().unwrap().into(), results.to_string())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first_page["cards"].as_array().unwrap().len(), 20);
    assert_eq!(first_page["complete"], false);
    let second = fixture.cursor(first_page["next_cursor"].as_str().unwrap());
    assert_eq!(
        request(&second, &pageable)["url"]
            .as_str()
            .unwrap()
            .contains("skip=20"),
        true
    );
    assert_eq!(skip(request(&second, &single)), None);
}
#[test]
fn merged_browse_rejects_a_cursor_it_did_not_issue() {
    let fixture = Fixture::new();
    fixture.install("https://pageable.example/manifest.json", pageable_catalog());
    for cursor in ["20", "d1;", "d2;discover:addon:a:0=20", "d1;discover:addon:a:0=20000"] {
        assert!(
            fixture
                .engine
                .plan_discover("default".into(), json!({"type":"movie","cursor":cursor}).to_string())
                .is_err(),
            "{cursor}"
        );
    }
}#[test]
fn aggregated_search_stays_one_page() {
    let fixture = Fixture::new();
    fixture.install(
        "https://search.example/manifest.json",
        json!({"id":"search","type":"movie","name":"Search","extra":[{"name":"search"},{"name":"skip"}]}),
    );
    let plan = fixture.plan(json!({"type":"movie","search":"dune"}));
    assert_eq!(plan["requests"].as_array().unwrap().len(), 1);
    let page = page(&fixture, &plan, |_| metas("s", 0..20));
    assert_eq!(page["complete"], true);
    assert_eq!(page["next_cursor"], Value::Null);
}
