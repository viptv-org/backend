use serde_json::json;

#[test]
fn large_anime_catalog_fits_the_native_bridge_and_prefers_english_titles() {
    let mut parent = viptv_simkl::normalize(&json!({"title":"Original title","en_title":"English title","ids":{"simkl":42,"imdb":"tt42"},"overview":"x".repeat(2000),"ratings":{"simkl":{"rating":9}},"trailers":[{"name":"Trailer","url":"https://example.com/trailer"}]}), viptv_simkl::Category::Anime).unwrap();
    assert_eq!(parent["name"], "English title");
    let videos: Vec<_> = (1..=1250).map(|episode| viptv_simkl::episode(&parent, &json!({"episode":episode,"title":format!("Episode {episode}"),"description":"Episode description","date":"2026-10-10T10:00:00Z"})).unwrap()).collect();
    parent["videos"] = json!(videos);
    let input = parent.to_string();
    assert!(input.len() < 2 * 1024 * 1024, "Large catalogs must fit without raising the bridge limit");
    let normalized = viptv_core::normalize("media".into(), input, "".into()).unwrap();
    let output: serde_json::Value = serde_json::from_str(&normalized).unwrap();
    assert_eq!(output["episodes"].as_array().unwrap().len(), 1250);
    assert_eq!(output["name"], "English title");
}
