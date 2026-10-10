use serde_json::{Value, json};
use viptv_core::dto::{
    DiscoverPolicyProjection, EpisodeWatching, HomeActions, PhonePresentation, SourceRanks,
    SourcesPollStep,
};

fn call(kind: &str, input: Value) -> Value {
    serde_json::from_str(
        &viptv_core::normalize(kind.into(), input.to_string(), String::new()).unwrap(),
    )
    .unwrap()
}

fn home(item: Value, queue: bool) -> HomeActions {
    serde_json::from_value(call("homeActions", json!({"item":item,"queueShelf":queue}))).unwrap()
}

#[test]
fn active_rewatch_overrides_cached_next_for_card_and_hero() {
    let item = json!({"type":"series","season":2,"episode":3,"position":25,"duration":100,
        "watched":true,"resumeActive":true,"queueStatus":"next",
        "previousEpisode":{"type":"episode","position":100,"duration":100,"completionOnly":true}});
    let actions = home(item.clone(), true);
    assert_eq!(actions.card_primary_action, "resume");
    assert_eq!(actions.hero_primary_action, "resume");
    assert_eq!(actions.hero_primary_action_label, "Resume");
    assert!(!actions.has_resolved_next);
    assert!(!actions.can_resume);
    assert!(actions.manage_previous);
    assert!(actions.show_hero_progress);
    assert_eq!(
        call("presentation", item.clone())["primaryAction"],
        "resume"
    );
    let episode: EpisodeWatching = serde_json::from_value(call("episodeWatching", item)).unwrap();
    assert!(episode.watching);
    assert_eq!(episode.progress, 0.25);
}

#[test]
fn completion_only_progress_is_neither_resume_nor_watching() {
    let item = json!({"type":"episode","season":1,"episode":1,"position":100,"duration":100,
        "completionOnly":true,"resumeActive":true,"watched":false});
    let actions = home(item.clone(), true);
    assert_eq!(actions.card_primary_action, "details");
    assert_eq!(actions.hero_primary_action, "details");
    assert!(!actions.can_resume);
    assert!(!actions.show_hero_progress);
    assert_eq!(call("presentation", item.clone())["resumeEligible"], false);
    let episode: EpisodeWatching = serde_json::from_value(call("episodeWatching", item)).unwrap();
    assert!(!episode.watching);
    assert_eq!(episode.progress, 1.0);
}

#[test]
fn home_inventory_keeps_manual_sources_and_queue_management_distinct() {
    let movie = home(json!({"type":"movie","position":20}), false);
    assert_eq!(movie.card_primary_action, "details");
    assert_eq!(movie.hero_primary_action, "sources");
    assert_eq!(movie.hero_primary_action_label, "Play");
    assert!(movie.opens_sources_from_hero);
    assert!(!movie.opens_queue_manage);
    let root = home(json!({"type":"series"}), false);
    assert_eq!(root.hero_primary_action, "details");
    assert_eq!(root.hero_primary_action_label, "Episodes");
    assert!(!root.opens_sources_from_hero);
    let episode = home(json!({"type":"series","season":0,"episode":1}), false);
    assert!(episode.opens_sources_from_hero);
    let next = home(
        json!({"type":"series","season":2,"episode":4,"queueStatus":"next",
        "previousEpisode":{"type":"episode","position":20}}),
        true,
    );
    assert!(next.has_resolved_next);
    assert!(next.can_resume);
    assert!(next.opens_queue_manage);
    assert!(!next.opens_sources_from_hero);
    assert_eq!(next.card_primary_action, "next");
    assert_eq!(next.hero_primary_action_label, "Play next episode");
    let unresolved = home(json!({"type":"series","queueStatus":"next"}), true);
    assert!(!unresolved.has_resolved_next);
    assert_eq!(unresolved.card_primary_action, "details");
    let live = home(json!({"type":"live","position":20,"duration":100}), true);
    assert!(!live.can_manage);
    assert!(!live.can_resume);
    assert!(!live.opens_queue_manage);
    assert_eq!(live.hero_primary_action, "play");
    assert_eq!(live.hero_primary_action_label, "Watch live");
}

#[test]
fn episode_projection_retains_watched_and_unknown_duration_semantics() {
    for (item, watching, progress) in [
        (
            json!({"type":"episode","position":0,"duration":100}),
            false,
            0.0,
        ),
        (
            json!({"type":"episode","position":20,"duration":100,"watched":true}),
            false,
            0.2,
        ),
        (json!({"type":"episode","position":20}), true, 0.0),
        (
            json!({"type":"episode","position":150,"duration":100,"resumeActive":true,"watched":true}),
            true,
            1.0,
        ),
        (
            json!({"type":"live","position":20,"duration":100}),
            false,
            0.0,
        ),
    ] {
        let projection: EpisodeWatching =
            serde_json::from_value(call("episodeWatching", item)).unwrap();
        assert_eq!(projection.watching, watching);
        assert_eq!(projection.progress, progress);
    }
}

#[test]
fn batch_ranking_matches_single_policy_with_stable_ties_and_unknown_caps() {
    let sources = json!([
        {"name":"1080p h264 English audio"},
        {"name":"1080p h264 English audio"},
        {"name":"720p h264 English audio"},
        {"name":"2160p h264 English audio"},
        {"name":"2160p hevc English audio"},
        {"name":"1080p h264 English subtitles"},
        {"name":"1080p av1 English audio"}
    ]);
    for caps in [
        json!({}),
        json!({"maxHeight":null}),
        json!({"maxHeight":0}),
        json!({"maxHeight":1080}),
        json!({"maxHeight":2160,"hevcSdr":true}),
    ] {
        let preferences = json!({"audioLanguage":"en","quality":"480p"});
        let batch = call(
            "sourceRanks",
            json!({"sources":sources,"capabilities":caps,"preferences":preferences}),
        );
        let typed: SourceRanks = serde_json::from_value(batch.clone()).unwrap();
        for (index, source) in sources.as_array().unwrap().iter().enumerate() {
            assert_eq!(
                batch["ranks"][index],
                call(
                    "sourceMatch",
                    json!({"source":source,"candidates":sources,"capabilities":caps,"preferences":preferences})
                )
            );
        }
        let first_equal = typed.ordered_indices.iter().position(|i| *i == 0).unwrap();
        let second_equal = typed.ordered_indices.iter().position(|i| *i == 1).unwrap();
        assert!(first_equal < second_equal);
        if caps["maxHeight"] == 1080 {
            assert_eq!(typed.ordered_indices[..4], [0, 1, 2, 5]);
            assert!(!typed.ranks[3].likely);
        } else if caps["hevcSdr"] != true {
            assert_eq!(typed.ordered_indices[0], 3);
            assert!(typed.ranks[3].best);
            assert!(!typed.ranks[4].likely);
        }
    }
    assert_eq!(
        call("sourceRanks", json!({"sources":[]}))["orderedIndices"],
        json!([])
    );
}

#[test]
fn producer_outcomes_preserve_partial_failures_successful_rows_and_observed_order() {
    let first = call(
        "sourcesPollStep",
        json!({"poll":{"done":false,"events":[
            {"seq":1,"source":"addon:3","streams":[],"error_code":"source_format_unsupported","error":"https://private.invalid/secret"},
            {"seq":2,"source":"iptv:4","streams":[{"id":"one","name":"Good"}]},
            {"seq":3,"source":"catalog:9","streams":[]}
        ]}}),
    );
    let first_typed: SourcesPollStep = serde_json::from_value(first.clone()).unwrap();
    assert_eq!(first_typed.producers.len(), 2);
    assert_eq!(first_typed.producers[0].source_id, "addon:3");
    assert_eq!(first_typed.producers[1].source_id, "iptv:4");
    assert!(!first.to_string().contains("secret"));
    let second = call(
        "sourcesPollStep",
        json!({"state":first["state"],"poll":{"done":true,"events":[
            {"seq":4,"source":"addon:3","streams":[{"id":"two","name":"Recovered"}]},
            {"seq":5,"source":"addon:5","streams":[],"error_code":"addon_timeout"}
        ]}}),
    );
    let typed: SourcesPollStep = serde_json::from_value(second.clone()).unwrap();
    assert_eq!(
        typed
            .producers
            .iter()
            .map(|p| p.source_id.as_str())
            .collect::<Vec<_>>(),
        ["addon:3", "iptv:4", "addon:5"]
    );
    assert_eq!(
        typed.producers[0].error_code.as_deref(),
        Some("source_format_unsupported")
    );
    assert_eq!(typed.sources.len(), 2);
    assert!(typed.done);
    assert_eq!(second["producers"], second["state"]["producers"]);
}

#[test]
fn producer_labels_join_configured_then_row_labels_without_inventing_choices() {
    let out = call(
        "sourceProducerLabels",
        json!({
            "observed":[{"sourceId":"addon:3","label":"raw","errorCode":"addon_timeout"},
                {"sourceId":"addon:4","label":"fallback"},{"sourceId":"iptv:8","label":"unchanged"}],
            "addons":[{"id":"3","name":"Configured"},{"id":"9","name":"Catalog only"}],
            "sources":[{"sourceAddonId":"addon:3","name":"Wrong row name"},{"sourceAddonId":"addon:4","name":"Row label"}]
        }),
    );
    assert_eq!(out.as_array().unwrap().len(), 3);
    assert_eq!(out[0]["label"], "Configured");
    assert_eq!(out[0]["errorCode"], "addon_timeout");
    assert_eq!(out[1]["label"], "Row label");
    assert_eq!(out[2]["label"], "unchanged");
}

#[test]
fn phone_copy_keeps_fixed_headings_namespace_labels_and_exact_year_text() {
    for (shelf, expected) in [
        (json!({"title":"Live now"}), "Live now"),
        (
            json!({"title":"Recently watched live TV"}),
            "Recently watched live TV",
        ),
        (json!({"title":"My List"}), "My List"),
        (
            json!({"isQueueShelf":true,"contentType":"movie"}),
            "Continue watching",
        ),
        (
            json!({"contentType":"series","catalogName":"Trending"}),
            "Series · Trending",
        ),
        (
            json!({"contentType":"movie","catalogName":"Popular"}),
            "Movies · Popular",
        ),
        (
            json!({"contentType":"anime.series","catalogName":"Top"}),
            "Anime · Top",
        ),
        (
            json!({"contentType":"tv_shows","catalogName":"Picks"}),
            "Tv Shows · Picks",
        ),
        (json!({"contentType":"series","catalogName":" "}), "Series"),
    ] {
        let projection: PhonePresentation =
            serde_json::from_value(call("phonePresentation", json!({"shelf":shelf}))).unwrap();
        assert_eq!(projection.shelf_heading, expected);
    }
    for (item, expected) in [
        (json!({"season":0,"episode":2,"year":"2024"}), "S0 E2"),
        (json!({"season":1,"year":"2024–2026"}), "2024–2026"),
        (json!({"year":2026}), "2026"),
        (json!({}), ""),
    ] {
        assert_eq!(
            call("phonePresentation", json!({"item":item}))["cardContext"],
            expected
        );
    }
    assert_eq!(
        call("phonePresentation", json!({"contentType":"movie"}))["contentTypeLabel"],
        "Movie"
    );
}

#[test]
fn discover_uses_declared_groups_and_nonblank_required_defaults() {
    let projection: DiscoverPolicyProjection = serde_json::from_value(call("discoverPolicy", json!({
        "type":"anime.movies","catalogs":[{"type":"live"},{"type":"movie"},{"type":"anime.series"}],
        "catalog":{"supportsSearch":true,"extras":[
            {"name":"genre","required":true,"defaultValue":"Drama","options":["Action"]},
            {"name":"sort","required":true,"options":["Popular"]},
            {"name":"empty","required":true,"defaultValue":" ","options":["No fallback"]},
            {"name":"optional","required":false,"defaultValue":"Ignored"},
            {"name":"skip","required":true,"defaultValue":"100"}
        ]}
    }))).unwrap();
    assert_eq!(projection.group, "anime");
    assert_eq!(projection.group_label, "Anime");
    assert_eq!(projection.first_catalog_index, Some(2));
    assert_eq!(projection.defaults.len(), 2);
    assert_eq!(projection.defaults["genre"], "Drama");
    assert_eq!(projection.defaults["sort"], "Popular");
    assert_eq!(
        call(
            "discoverPolicy",
            json!({"type":"unknown","catalogs":[{"type":"live"},{"type":"series"}]})
        )["firstCatalogIndex"],
        1
    );
    assert_eq!(
        call(
            "discoverPolicy",
            json!({"type":"movie","catalogs":[{"type":"live"}]})
        )["firstCatalogIndex"],
        0
    );
    assert!(
        call("discoverPolicy", json!({"type":"movie","catalogs":[]}))["firstCatalogIndex"]
            .is_null()
    );
}

#[test]
fn producer_display_state_is_bounded_sanitized_and_default_compatible() {
    let empty: SourcesPollStep = serde_json::from_value(json!({
        "state":{"after":0,"sources":[],"polls":0},"sources":[],"done":false
    }))
    .unwrap();
    assert!(empty.producers.is_empty());
    assert!(empty.state.producers.is_empty());
    let events: Vec<_> = (0..300)
        .map(|i| {
            json!({"seq":i+1,"source":format!("addon:{i}"),
        "streams":[],"error_code":"addon_timeout","error":"https://private.invalid/secret"})
        })
        .collect();
    let output = call(
        "sourcesPollStep",
        json!({"poll":{"events":events,"done":true}}),
    );
    let producers = output["producers"].as_array().unwrap();
    assert_eq!(producers.len(), 256);
    assert!(output["producers"].to_string().len() <= 256 * 2000);
    assert!(!output.to_string().contains("secret"));
    // The display-state bound must not change completion, cursor or stream handling.
    assert_eq!(output["state"]["after"], 300);
    assert_eq!(output["done"], true);
    let observed = json!([{"sourceId":"addon:3","label":"fallback","url":"https://private.invalid/secret",
        "errorCode":"addon_timeout","errorMessage":"https://private.invalid/secret"}]);
    let named = call(
        "sourceProducerLabels",
        json!({"observed":observed,
        "addons":[{"id":"3","name":"😀".repeat(1000)}]}),
    );
    assert_eq!(named[0]["label"].as_str().unwrap().chars().count(), 180);
    assert!(named.to_string().len() <= 2000);
    assert!(named[0].get("url").is_none());
    assert!(!named.to_string().contains("secret"));
    let retained = call(
        "sourcesPollStep",
        json!({"state":{"producers":observed},"poll":{"events":[],"done":true}}),
    );
    assert!(retained["producers"][0].get("url").is_none());
    assert!(!retained.to_string().contains("secret"));
}

#[test]
fn zero_position_active_flag_does_not_invent_queue_resume() {
    let actions = home(
        json!({"type":"episode","position":0,"resumeActive":true}),
        true,
    );
    assert_eq!(actions.card_primary_action, "details");
    assert!(!actions.can_resume);
}

#[test]
fn playable_rows_precede_unsupported_rows_even_with_stronger_language_evidence() {
    let result = call(
        "sourceRanks",
        serde_json::json!({
            "sources":[{"name":"4k HEVC English audio dubbed"},{"name":"1080p h264"}],
            "capabilities":{"maxHeight":1080,"h264":true,"hevcSdr":false},
            "preferences":{"audioLanguage":"en"}
        }),
    );
    assert_eq!(result["orderedIndices"], serde_json::json!([1, 0]));
    assert_eq!(result["ranks"][0]["best"], false);
    let rejected = call(
        "sourceMatch",
        serde_json::json!({
            "source":{"name":"1080p h264 English audio"},
            "capabilities":{"maxHeight":1080,"h264":false},
            "preferences":{"audioLanguage":"en"}
        }),
    );
    assert_eq!(rejected["likely"], false);
    assert_eq!(rejected["best"], false);
}
