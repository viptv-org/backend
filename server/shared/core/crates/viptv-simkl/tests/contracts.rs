use serde_json::json;
use viptv_simkl::{Category, episode, normalize, stream_id, write_item};

#[test]
fn public_feed_dates_runtime_and_identity_are_normalized() {
    let item=normalize(&json!({"title":"Test","ids":{"simkl_id":42,"imdb":"tt42"},"release_date":"08/14/2020","runtime":"1h 30m","ratings":{"simkl":{"rating":8.5}}}),Category::Movie).unwrap();
    assert_eq!(item["id"], "simkl:movies:42");
    assert_eq!(item["year"], 2020);
    assert_eq!(item["duration"], 5400.0);
    assert_eq!(stream_id(&item).unwrap(), "tt42");
    assert_eq!(write_item(&item).unwrap()["movie"]["ids"]["simkl"], 42);
}
#[test]
fn anime_requires_exact_episode_mapping() {
    let parent = normalize(
        &json!({"title":"Anime","ids":{"simkl":42,"imdb":"tt42"}}),
        Category::Anime,
    )
    .unwrap();
    let episode = episode(
        &parent,
        &json!({"episode":13,"tvdb":{"season":3,"episode":1},"runtime":25}),
    )
    .unwrap();
    assert_eq!(stream_id(&episode).unwrap(), "tt42:3:1");
    assert_eq!(episode["duration"], 1500.0);
    let missing = viptv_simkl::episode(&parent, &json!({"episode":13})).unwrap();
    assert!(stream_id(&missing).is_err());
    assert_eq!(write_item(&missing).unwrap()["episode"]["number"], 13);
}
#[test]
fn unavailable_mapping_never_falls_back_to_title_search() {
    let item = normalize(
        &json!({"title":"No mapping","ids":{"simkl":12}}),
        Category::Tv,
    )
    .unwrap();
    assert!(stream_id(&item).is_err());
    assert!(write_item(&json!({"name":"Unknown","id":"unknown"})).is_err());
}
