use serde_json::{Value, json};
use viptv_core::dto::{HomeLayout, SearchPlan};

fn call(kind: &str, input: Value) -> Value {
    serde_json::from_str(
        &viptv_core::normalize(kind.into(), input.to_string(), "https://example.com".into())
            .unwrap(),
    )
    .unwrap()
}

fn catalogs() -> Value {
    json!([
        {"id":"top","type":"movie","name":"Popular","addonName":"Cinemeta","supportsSearch":true,"extras":[]},
        {"id":"chan","type":"live","name":"Channels","addonName":"IPTV","supportsSearch":true,"extras":[]},
        {"id":"genre","type":"series","name":"By genre","addonName":"Cinemeta","extras":[{"name":"genre","required":true,"options":["Drama"]}]},
        {"id":"pick","type":"series","name":"Pick one","addonName":"Other","extras":[{"name":"genre","required":true,"options":[]}]},
        {"id":"only-search","type":"movie","name":"Search","addonName":" ","supportsSearch":true,"extras":[{"name":"search","required":true}]},
        {"id":"anime","type":"anime","name":"Season","addonName":"Kitsu","supportsSearch":true,"extras":[]}
    ])
}

#[test]
fn home_layout_keeps_the_shelf_order_and_only_loadable_catalogs() {
    let layout = call("homeLayout", json!({"catalogs":catalogs()}));
    serde_json::from_value::<HomeLayout>(layout.clone()).unwrap();
    let shelves = layout["shelves"].as_array().unwrap();
    let roles: Vec<_> = shelves
        .iter()
        .map(|s| s["role"].as_str().unwrap())
        .collect();
    assert_eq!(
        roles,
        [
            "continueWatching",
            "recentLive",
            "catalog",
            "catalog",
            "catalog",
            "myList",
            "liveNow"
        ]
    );
    let titles: Vec<_> = shelves
        .iter()
        .map(|s| s["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        [
            "Continue watching",
            "Recently watched live TV",
            "Cinemeta · Popular",
            "Cinemeta · By genre",
            "Kitsu · Season",
            "My List",
            "Live now"
        ]
    );
    // Live namespaces, a required filter without choices and a search-only
    // catalog cannot load without viewer input.
    let indices: Vec<_> = shelves
        .iter()
        .filter_map(|s| s["catalogIndex"].as_u64())
        .collect();
    assert_eq!(indices, [0, 2, 5]);
    assert_eq!(shelves[0]["limit"], 40);
    assert_eq!(shelves[1]["limit"], 24);
    assert!(shelves[2]["limit"].is_null());
    // Without catalogs the fixed shelves remain.
    let empty = call("homeLayout", json!({}));
    assert_eq!(empty["shelves"].as_array().unwrap().len(), 4);
    // A shell that hides live Home shelves keeps every other shelf in order.
    let no_live = call(
        "homeLayout",
        json!({"catalogs":catalogs(),"liveShelves":false}),
    );
    let roles: Vec<_> = no_live["shelves"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["role"].as_str().unwrap())
        .collect();
    assert_eq!(
        roles,
        [
            "continueWatching",
            "catalog",
            "catalog",
            "catalog",
            "myList"
        ]
    );
}

#[test]
fn search_plan_covers_live_catalogs_and_follows_the_scope() {
    let plan = call(
        "searchPlan",
        json!({"query":"  bebop ","catalogs":catalogs()}),
    );
    serde_json::from_value::<SearchPlan>(plan.clone()).unwrap();
    assert_eq!(plan["query"], "bebop");
    assert_eq!(
        plan["sections"],
        json!([
            {"catalogIndex":0,"title":"Cinemeta · Popular"},
            {"catalogIndex":1,"title":"IPTV · Channels"},
            {"catalogIndex":4,"title":"Search"},
            {"catalogIndex":5,"title":"Kitsu · Season"}
        ])
    );
    assert_eq!(plan["live"], true);
    assert_eq!(plan["liveTitle"], "Live TV");
    assert_eq!(plan["liveRequestLimit"], 80);
    assert_eq!(plan["sectionLimit"], 24);

    let movies = call(
        "searchPlan",
        json!({"query":"x","scope":"movie","catalogs":catalogs()}),
    );
    let indices: Vec<_> = movies["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["catalogIndex"].as_u64().unwrap())
        .collect();
    assert_eq!(indices, [0, 4]);
    assert_eq!(movies["live"], false);
    let live = call(
        "searchPlan",
        json!({"query":"x","scope":"live","catalogs":catalogs()}),
    );
    assert_eq!(live["live"], true);
    assert_eq!(live["sections"].as_array().unwrap().len(), 1);
}

#[test]
fn search_plan_bounds_queries_and_catalogs() {
    for query in ["", "   ", "a\u{0007}b"] {
        let plan = call("searchPlan", json!({"query":query,"catalogs":catalogs()}));
        assert_eq!(plan["query"], "");
        assert_eq!(plan["sections"], json!([]));
        assert_eq!(plan["live"], false);
    }
    let long = "x".repeat(300);
    let plan = call("searchPlan", json!({"query":long,"catalogs":catalogs()}));
    assert_eq!(plan["query"].as_str().unwrap().chars().count(), 128);
    let many: Vec<_> = (0..200)
        .map(|n| json!({"id":n.to_string(),"type":"movie","name":"C","supportsSearch":true}))
        .collect();
    let plan = call("searchPlan", json!({"query":"x","catalogs":many}));
    assert_eq!(plan["sections"].as_array().unwrap().len(), 128);
}
