use serde_json::{Value, json};
fn call(kind: &str, input: Value) -> Value {
    serde_json::from_str(
        &viptv_core::normalize(kind.into(), input.to_string(), "https://example.com".into())
            .unwrap(),
    )
    .unwrap()
}
#[test]
fn hero_never_promotes_portrait_and_home_enrichment_preserves_progress() {
    let item = json!({"id":"episode","type":"series","name":"Title","poster":"portrait.jpg","position":42,"duration":100,"sourceFingerprint":"saved"});
    let view = call("presentation", item.clone());
    assert!(view["heroImage"].is_null());
    assert_eq!(view["posterImage"], "portrait.jpg");
    let enriched = call(
        "enrichHome",
        json!({"original":item,"metadata":{"background":"landscape.jpg","position":0,"sourceFingerprint":"wrong"}}),
    );
    assert_eq!(enriched["position"], 42);
    assert_eq!(enriched["sourceFingerprint"], "saved");
    assert_eq!(call("presentation", enriched)["heroImage"], "landscape.jpg");
}
#[test]
fn malformed_rows_do_not_blank_valid_items() {
    let out = call(
        "discover",
        json!({"items":[{"id":"x","type":"movie","name":"Movie"},{"type":"bad"}]}),
    );
    assert_eq!(out["items"].as_array().unwrap().len(), 1);
}
#[test]
fn playback_request_never_introduces_profile_identity() {
    let out = call(
        "request",
        json!({"operation":"playback","profileId":"private","playback":{"streamId":"opaque","capabilities":{"directPlay":true,"maxWidth":1920}}}),
    );
    assert_eq!(out["path"], "/api/playback");
    assert!(out["body"].get("profile_id").is_none());
    assert_eq!(out["body"]["capabilities"]["direct_play"], true);
}

#[test]
fn episode_metadata_request_targets_its_parent_series() {
    let out = call(
        "request",
        json!({
            "operation":"metadata",
            "item":{
                "id":"bleach:1:3",
                "type":"episode",
                "seriesId":"bleach"
            }
        }),
    );
    assert_eq!(out["method"], "GET");
    assert_eq!(out["path"], "/api/meta/series/bleach");
}
#[test]
fn exact_resume_does_not_cross_provider() {
    let sources = json!([{"id":"bad","sourceAddonId":"iptv:2","sourceFingerprint":"same"},{"id":"good","sourceAddonId":"iptv:1","sourceFingerprint":"same"}]);
    let out = call(
        "exactResume",
        json!({"sources":sources,"item":{"sourceAddonId":"iptv:1","sourceFingerprint":"same"}}),
    );
    assert_eq!(out["id"], "good");
}
#[test]
fn subtitles_are_not_audio_evidence() {
    let caps = json!({"maxHeight":1080,"hevcSdr":false});
    let prefs = json!({"audioLanguage":"en"});
    let sub = call(
        "sourceMatch",
        json!({"source":{"name":"1080p h264 English subtitles","raw":{}},"capabilities":caps,"preferences":prefs}),
    );
    let dub = call(
        "sourceMatch",
        json!({"source":{"name":"1080p h264 English audio","raw":{}},"capabilities":caps,"preferences":prefs}),
    );
    assert!(sub["rank"].as_f64().unwrap() > dub["rank"].as_f64().unwrap());
}

#[test]
fn home_hero_primary_matches_each_contract_state() {
    for (item, action, label) in [
        (
            json!({"type":"live","position":50,"queueStatus":"next"}),
            "play",
            "Watch live",
        ),
        (
            json!({"type":"series","episode":2,"queueStatus":"next","position":50}),
            "next",
            "Play next episode",
        ),
        (json!({"type":"movie","position":50}), "resume", "Resume"),
        (
            json!({"type":"series","episode":2,"position":50}),
            "resume",
            "Resume",
        ),
        (json!({"type":"series"}), "episodes", "Episodes"),
        (json!({"type":"series","episode":2}), "sources", "Play"),
        (json!({"type":"episode","episode":2}), "sources", "Play"),
        (json!({"type":"movie"}), "sources", "Play"),
    ] {
        let view = call("presentation", item);
        assert_eq!(view["primaryAction"], action);
        assert_eq!(view["primaryActionLabel"], label);
    }
}

#[test]
fn source_projection_preserves_identity_and_rich_native_labels() {
    let source = call(
        "source",
        json!({"id":"stream-a","provider":"iptv:4","source_name":"Evening News","title":"HD broadcast","filename":"evening-news.mkv","source_addon_id":"addon:one","source_fingerprint":"fp-a"}),
    );
    assert_eq!(source["name"], "HD broadcast");
    let display = call("sourceDisplay", source.clone());
    assert_eq!(display["title"], "Evening News");
    assert_eq!(display["body"], "HD broadcast\nevening-news.mkv");
    assert_eq!(source["sourceFingerprint"], "fp-a");
    assert_eq!(
        call(
            "sourceDisplay",
            json!({"name":"Label","description":"Same","title":"Same","filename":"Same"})
        )["body"],
        "Same"
    );
}

#[test]
fn title_logo_aliases_preserve_text_and_never_promote_other_artwork() {
    for alias in [
        "titleLogo",
        "title_logo",
        "clearLogo",
        "clearlogo",
        "clear_logo",
        "logo",
    ] {
        let mut input = json!({"id":"movie","type":"movie","name":"Movie","poster":"poster.jpg","background":"backdrop.jpg"});
        input[alias] = json!("transparent.png");
        let item = call("media", input);
        assert_eq!(item["titleLogo"], "transparent.png", "{alias}");
        let view = call("presentation", item);
        assert_eq!(view["titleLogo"], "transparent.png");
        assert_eq!(view["title"], "Movie");
        assert_eq!(view["heroImage"], "backdrop.jpg");
    }
    for input in [
        json!({"id":"movie","type":"movie","name":"Movie","poster":"poster.jpg","background":"backdrop.jpg","titleLogo":"  ","logo":42}),
        json!({"id":"channel","type":"live","name":"Channel","logo":"station.png"}),
    ] {
        let item = call("media", input.clone());
        assert!(call("presentation", item)["titleLogo"].is_null());
        assert_eq!(call("presentation", input.clone())["title"], input["name"]);
    }
    assert_eq!(
        call(
            "media",
            json!({"id":"m","type":"movie","titleLogo":false,"clearlogo":"clear.png","logo":"other.png"})
        )["titleLogo"],
        "clear.png"
    );
    assert!(
        call(
            "presentation",
            json!({"name":"Movie","raw":{"logo":"raw.png"}})
        )["titleLogo"]
            .is_null()
    );
}

#[test]
fn episodes_inherit_parent_logo_without_losing_playback_identity() {
    let series = call(
        "media",
        json!({"id":"series","type":"series","name":"Series","logo":"series.png","videos":[{"id":"series:1:2","name":"Episode two","season":1,"episode":2,"position":42,"duration":100}]}),
    );
    let episode = &series["episodes"][0];
    assert_eq!(episode["id"], "series:1:2");
    assert_eq!(episode["seriesId"], "series");
    assert_eq!(episode["episodeTitle"], "Episode two");
    let view = call("presentation", episode.clone());
    assert_eq!(view["titleLogo"], "series.png");
    assert_eq!(view["title"], "Series");
    assert_eq!(view["progress"], 0.42);
    let enriched = call(
        "enrichHome",
        json!({"original":episode,"metadata":{"id":"series","titleLogo":"new.png","position":0}}),
    );
    assert_eq!(enriched["id"], "series:1:2");
    assert_eq!(enriched["position"], 42);
    assert_eq!(enriched["titleLogo"], "new.png");
    assert_eq!(
        call(
            "enrichHome",
            json!({"original":enriched,"metadata":{"titleLogo":" "}})
        )["titleLogo"],
        "new.png"
    );
}

#[test]
fn queue_card_is_a_complete_shared_projection_of_the_exact_episode() {
    let original = json!({"id":"series:1:3","type":"series","name":"Series","seriesId":"series","season":1,"episode":3,"poster":"series-poster.jpg","thumbnail":"saved-still.jpg","position":42,"duration":100,"sourceFingerprint":"saved","previousEpisode":{"id":"series:1:2"}});
    let metadata = json!({"id":"series","name":"Series","poster":"new-series-poster.jpg","thumbnail":"wrong-parent-image.jpg","background":"series-landscape.jpg","episodes":[
        {"id":"series:1:2","season":1,"episode":2,"thumbnail":"wrong-episode.jpg"},
        {"id":"series:1:3","season":1,"episode":3,"thumbnail":"episode-three.jpg","name":"The Third Episode"}
    ]});
    let enriched = call(
        "enrichHome",
        json!({"original":original,"metadata":metadata}),
    );
    assert_eq!(enriched["sourceFingerprint"], "saved");
    assert_eq!(enriched["previousEpisode"]["id"], "series:1:2");
    let card = call(
        "cardPresentation",
        json!({"item":enriched,"context":"queue"}),
    );
    assert_eq!(card["image"], "episode-three.jpg");
    assert_eq!(card["imageRole"], "episode");
    assert_eq!(card["title"], "Series");
    assert_eq!(
        card["subtitle"],
        "S1 · E3 · The Third Episode · Resume at 0:42"
    );
    assert_eq!(card["progress"], 0.42);
    assert_eq!(card["primaryAction"], "resume");
    assert_eq!(card["primaryActionLabel"], "Resume");
}

#[test]
fn missing_episode_art_never_promotes_parent_or_other_episode_image() {
    let original = json!({"id":"e3","type":"series","name":"Series","season":1,"episode":3,"poster":"portrait.jpg"});
    let metadata = json!({"thumbnail":"parent.jpg","episodes":[{"id":"e2","season":1,"episode":2,"thumbnail":"other.jpg"}]});
    let enriched = call(
        "enrichHome",
        json!({"original":original,"metadata":metadata}),
    );
    let card = call(
        "cardPresentation",
        json!({"item":enriched,"context":"queue"}),
    );
    assert!(card["image"].is_null());
    assert_eq!(card["imageRole"], "none");
    assert!(card["progress"].is_null());
}

#[test]
fn shared_cards_distinguish_catalog_live_and_continuation_intent() {
    let live = call(
        "cardPresentation",
        json!({"item":{"name":"Channel","type":"live","poster":"channel-logo.png","position":40,"duration":100},"context":"catalog"}),
    );
    assert_eq!(live["imageRole"], "logo");
    assert_eq!(live["primaryAction"], "play");
    assert!(live["progress"].is_null());
    assert_eq!(live["subtitle"], "");
    for (context, status, action) in [
        ("catalog", "next", "details"),
        ("queue", "next", "next"),
        ("queue", "caught_up", "episodes"),
        ("queue", "upcoming", "episodes"),
    ] {
        let card = call(
            "cardPresentation",
            json!({"item":{"name":"Series","type":"series","season":1,"episode":3,"queueStatus":status},"context":context}),
        );
        assert_eq!(card["primaryAction"], action);
    }
}

#[test]
fn large_series_queue_does_not_duplicate_episode_catalog_across_the_bridge() {
    let videos: Vec<Value> = (1..=400).map(|episode| json!({
        "id":format!("long-series:1:{episode}"), "season":1,"episode":episode,
        "title":format!("Episode {episode}"),"overview":"Episode synopsis. ".repeat(70),"thumbnail":format!("episode-{episode}.jpg")
    })).collect();
    let detail = call(
        "detailResponse",
        json!({"response":{"meta":{"id":"long-series","type":"series","name":"Long Series","videos":videos}},"item":{"id":"long-series","type":"series"}}),
    );
    let queue = call(
        "enrichHome",
        json!({"original":{"id":"long-series:1:3","type":"series","name":"Long Series","season":1,"episode":3,"position":42},"metadata":detail["item"]}),
    );
    let combined = json!({"original":queue,"metadata":detail["item"]});
    // The old normalization copied both the raw and normalized full catalog
    // into a queue occurrence; this exceeded normalize()'s 2 MiB input limit.
    assert!(combined.to_string().len() < 2 * 1024 * 1024);
    let _ = call("enrichDetail", combined);
    assert!(queue["episodes"].as_array().is_none_or(Vec::is_empty));
    assert_eq!(queue["thumbnail"], "episode-3.jpg");
}

#[test]
fn failed_queue_still_uses_landscape_then_empty_without_changing_resume() {
    let item = json!({"id":"series:1:3","type":"series","name":"Series","season":1,"episode":3,"thumbnail":"missing-still.jpg","background":"landscape.jpg","poster":"portrait.jpg","position":42,"duration":100});
    let original = call("cardPresentation", json!({"item":item,"context":"queue"}));
    assert_eq!(original["image"], "missing-still.jpg");
    let mut failed = Vec::new();
    for (url, expected, role) in [
        ("missing-still.jpg", json!("landscape.jpg"), "landscape"),
        ("landscape.jpg", Value::Null, "none"),
    ] {
        failed.push(url);
        let card = call(
            "cardPresentation",
            json!({"item":item,"context":"queue","failedImages":failed}),
        );
        assert_eq!(card["image"], expected);
        assert_eq!(card["imageRole"], role);
        for field in [
            "title",
            "subtitle",
            "progress",
            "primaryAction",
            "primaryActionLabel",
        ] {
            assert_eq!(card[field], original[field], "{field}");
        }
    }
}

#[test]
fn source_groups_do_not_collapse_missing_provider_ids() {
    let iptv = call(
        "sourceDisplay",
        json!({"name":"IPTV One","sourceAddonId":"iptv:1"}),
    );
    let addon = call(
        "sourceDisplay",
        json!({"name":"Torrent Addon","sourceAddonId":"4"}),
    );
    let other = call("sourceDisplay", json!({"name":"IPTV Two"}));
    assert_ne!(iptv["providerKey"], addon["providerKey"]);
    assert_ne!(iptv["providerKey"], other["providerKey"]);
    assert_eq!(addon["providerLabel"], "Torrent Addon");
    assert_eq!(other["providerLabel"], "IPTV Two");
}
