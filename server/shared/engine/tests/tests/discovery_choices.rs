use serde_json::{Value, json};
use viptv_engine::{
    query::{self, QueryValue},
    stremio::*,
};
fn catalog() -> Value {
    json!({"id":"top","type":"movie","extra":[
        {"name":"genre","isRequired":true,"optionsLimit":2,"options":["Action & Adventure","Drama, Crime","Comedy"]},
        {"name":"sort","isRequired":true,"options":["year","rating"],"default":"year"},
        {"name":"disabled","optionsLimit":0,"options":["x"]},{"name":"skip"}
    ]})
}
fn request() -> CatalogQuery {
    serde_json::from_value(json!({"kind":"movie","catalog":"top","addon":1,"skip":24,"search":null,"genre":null,"extras":{},"values":[{"id":"genre","values":["Action & Adventure","Drama, Crime"]}]})).unwrap()
}
fn plan(q: &CatalogQuery) -> Result<Vec<String>, viptv_engine::Error> {
    discovery_urls(
        "https://addon.example/manifest.json",
        &json!({"catalogs":[catalog()]}),
        q,
    )
}
#[test]
fn raw_and_normalized_controls_preserve_required_limits_and_defaults() {
    let raw = catalog();
    let mut normalized = raw.clone();
    normalized["extra"] = json!(
        viptv_provider::extras::catalog_extras(&raw)
            .iter()
            .map(|e| e.wire())
            .collect::<Vec<_>>()
    );
    let controls = catalog_controls(&raw);
    assert_eq!(
        serde_json::to_value(&controls).unwrap(),
        serde_json::to_value(catalog_controls(&normalized)).unwrap()
    );
    assert_eq!(controls.len(), 2);
    assert!(controls[0].required);
    assert_eq!(controls[0].maximum_selections, Some(2));
    assert_eq!(
        serde_json::to_value(&controls[0].kind).unwrap(),
        "MultiChoice"
    );
    assert!(!controls[1].required);
    assert_eq!(controls[1].maximum_selections, Some(1));
}
#[test]
fn one_request_encodes_repeated_keys_once_and_keeps_selections_while_paging() {
    let mut q = request();
    let urls = plan(&q).unwrap();
    assert_eq!(urls.len(), 1);
    let url = url::Url::parse(&urls[0]).unwrap();
    let encoded = url
        .path()
        .rsplit('/')
        .next()
        .unwrap()
        .strip_suffix(".json")
        .unwrap();
    let pairs = url::form_urlencoded::parse(encoded.as_bytes())
        .into_owned()
        .collect::<Vec<_>>();
    assert_eq!(
        pairs,
        vec![
            ("skip".into(), "24".into()),
            ("genre".into(), "Action & Adventure".into()),
            ("genre".into(), "Drama, Crime".into()),
            ("sort".into(), "year".into())
        ]
    );
    assert!(!encoded.contains("%2526"));
    q.skip = 48;
    let next = plan(&q).unwrap();
    assert!(next[0].contains("skip=48"));
    assert_eq!(next[0].matches("genre=").count(), 2);
    q.values.clear();
    q.genre = Some("Comedy".into());
    q.extras.insert("sort".into(), "rating".into());
    assert!(plan(&q).unwrap()[0].contains("genre=Comedy&sort=rating"));
}
#[test]
fn invalid_conflicting_unadvertised_required_or_foreign_selections_never_plan_a_fetch() {
    let q = request();
    let mut variants = Vec::new();
    let mut bad = q.clone();
    bad.values.push(bad.values[0].clone());
    variants.push(bad);
    let mut bad = q.clone();
    bad.values[0].values.push("Comedy".into());
    variants.push(bad);
    let mut bad = q.clone();
    bad.values[0].values[1] = bad.values[0].values[0].clone();
    variants.push(bad);
    let mut bad = q.clone();
    bad.genre = Some("Comedy".into());
    variants.push(bad);
    let mut bad = q.clone();
    bad.values[0].values.clear();
    variants.push(bad);
    let mut bad = q.clone();
    bad.values[0].id = "foreign".into();
    variants.push(bad);
    let mut bad = q.clone();
    bad.values[0].values[0] = "not advertised".into();
    variants.push(bad);
    let mut bad = q.clone();
    bad.values[0].id = "disabled".into();
    bad.values[0].values = vec!["x".into()];
    variants.push(bad);
    let mut bad = q.clone();
    bad.extras.insert("genre".into(), "Comedy".into());
    variants.push(bad);
    let mut bad = q.clone();
    bad.catalog = Some("other".into());
    variants.push(bad);
    let mut bad = q.clone();
    bad.addon = Some(2);
    variants.push(bad);
    let mut bad = q.clone();
    bad.addon = None;
    variants.push(bad);
    let mut bad = q.clone();
    bad.values[0].values = vec!["Comedy".into(); 65];
    variants.push(bad);
    let mut bad = q.clone();
    bad.values[0].id = "skip".into();
    variants.push(bad);
    for (i, bad) in variants.iter().enumerate() {
        assert!(plan(bad).is_err(), "case {i}");
    }
}
#[test]
fn canonical_selection_limits_are_optional_but_duplicates_and_global_bounds_are_not() {
    let mut control = catalog_controls(&catalog()).remove(0);
    let values = |v: Vec<&str>| {
        vec![QueryValue {
            id: "genre".into(),
            values: v.into_iter().map(str::to_owned).collect(),
        }]
    };
    assert!(
        query::validate(
            &[control.clone()],
            &values(vec!["Comedy", "Drama, Crime", "Action & Adventure"])
        )
        .is_err()
    );
    control.maximum_selections = None;
    assert!(
        query::validate(
            &[control.clone()],
            &values(vec!["Comedy", "Drama, Crime", "Action & Adventure"])
        )
        .is_ok()
    );
    assert!(query::validate(&[control.clone()], &values(vec!["Comedy", "Comedy"])).is_err());
    assert!(query::validate(&[control.clone()], &[]).is_err());
    let old = json!({"id":"genre","label":"Genre","kind":"MultiChoice","required":false,"choices":[],"minimum":null,"maximum":null});
    assert!(
        serde_json::from_value::<query::QueryControl>(old)
            .unwrap()
            .maximum_selections
            .is_none()
    );
}

#[test]
fn manifest_default_single_choice_and_bounded_limits_are_explicit() {
    for (limit, kind, cap) in [
        (None, "SingleChoice", 1),
        (Some(1), "SingleChoice", 1),
        (Some(2), "MultiChoice", 2),
        (Some(1000), "MultiChoice", 64),
    ] {
        let mut catalog = json!({"extra":[{"name":"genre","options":["A","B"]}]});
        if let Some(limit) = limit {
            catalog["extra"][0]["optionsLimit"] = json!(limit);
        }
        let controls = catalog_controls(&catalog);
        assert_eq!(serde_json::to_value(&controls[0].kind).unwrap(), kind);
        assert_eq!(controls[0].maximum_selections, Some(cap));
    }
    let catalog = json!({"id":"top","type":"movie","extra":[{"name":"genre","options":["A","B"]}]});
    let mut query = request();
    query.skip = 0;
    query.values[0].values = vec!["A".into(), "B".into()];
    assert!(
        discovery_urls(
            "https://addon.example/manifest.json",
            &json!({"catalogs":[catalog]}),
            &query
        )
        .is_err()
    );
}

#[test]
fn global_selection_cap_accepts_64_unique_values_and_rejects_65() {
    let choices: Vec<_> = (0..65).map(|i| format!("choice-{i}")).collect();
    let catalog = json!({"id":"top","type":"movie","extra":[{"name":"genre","options":choices,"optionsLimit":1000}]});
    let mut q = request();
    q.skip = 0;
    q.values[0].values = choices[..64].to_vec();
    let manifest = json!({"catalogs":[catalog]});
    let urls = discovery_urls("https://addon.example/manifest.json", &manifest, &q).unwrap();
    assert_eq!(urls.len(), 1);
    assert_eq!(urls[0].matches("genre=").count(), 64);
    q.values[0].values = choices;
    assert!(discovery_urls("https://addon.example/manifest.json", &manifest, &q).is_err());
}
