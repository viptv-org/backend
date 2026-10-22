//! SRC-BEST-MATCH-001 ranking keys and language tiers.
use viptv_engine::preferences::{HdrPreference, ProfilePreferences, language_tier, rank_sources};
use viptv_engine::stremio::SourceCandidate;

fn source(id: &str) -> SourceCandidate {
    SourceCandidate {
        id: id.into(),
        provider_id: "addon:1".into(),
        label: id.into(),
        quality_height: Some(1080),
        delivery: Some("direct".into()),
        ..Default::default()
    }
}
fn with(id: &str, change: impl FnOnce(&mut SourceCandidate)) -> SourceCandidate {
    let mut candidate = source(id);
    change(&mut candidate);
    candidate
}
fn langs(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_owned()).collect()
}
fn ranked(
    mut candidates: Vec<SourceCandidate>,
    preferences: &ProfilePreferences,
    original: Option<&str>,
    device: Option<u32>,
) -> Vec<String> {
    rank_sources(&mut candidates, preferences, original, device);
    candidates.into_iter().map(|c| c.id).collect()
}
fn english() -> ProfilePreferences {
    ProfilePreferences {
        preferred_audio_language: Some("en".into()),
        ..Default::default()
    }
}

#[test]
fn language_tier_table() {
    let unmarked = source("unmarked");
    let english = with("english", |c| c.audio_languages = langs(&["en-US"]));
    let dual_english = with("dual-en", |c| {
        c.audio_languages = langs(&["en"]);
        c.audio_flags = langs(&["dual"]);
    });
    let multi = with("multi", |c| c.audio_flags = langs(&["multi"]));
    let dual_french = with("dual-fr", |c| {
        c.audio_languages = langs(&["fr"]);
        c.audio_flags = langs(&["dual", "dubbed"]);
    });
    let dubbed = with("dubbed", |c| c.audio_flags = langs(&["dubbed"]));
    let french = with("french", |c| c.audio_languages = langs(&["fr"]));
    let french_dubbed = with("fr-dub", |c| {
        c.audio_languages = langs(&["fr"]);
        c.audio_flags = langs(&["dubbed"]);
    });
    let rows = [
        &unmarked,
        &english,
        &dual_english,
        &multi,
        &dual_french,
        &dubbed,
        &french,
        &french_dubbed,
    ];
    let tiers = |requested, original| {
        rows.iter()
            .map(|c| language_tier(c, requested, original))
            .collect::<Vec<_>>()
    };
    // English requested, original unknown: dual/multi ties with unmarked.
    assert_eq!(tiers(Some("en"), None), [3, 1, 1, 3, 3, 3, 4, 4]);
    // English requested for a Japanese title: dual/multi and bare dub are tier 2.
    assert_eq!(tiers(Some("en"), Some("ja")), [3, 1, 1, 2, 2, 2, 4, 4]);
    // English requested and English is original: unmarked ties with tier 1,
    // a bare dub is tier 4, dual/multi still beats dubbed.
    assert_eq!(tiers(Some("en"), Some("en")), [1, 1, 1, 2, 2, 4, 4, 4]);
    // "Original language" resolves to the known original.
    assert_eq!(tiers(None, Some("en")), tiers(Some("en"), Some("en")));
    // "Original language" with no metadata: no language ranking at all.
    assert_eq!(tiers(None, None), [1; 8]);
    // French requested for an English title.
    assert_eq!(tiers(Some("fr"), Some("en")), [3, 4, 2, 2, 1, 2, 1, 1]);
}

#[test]
fn torrentio_rows_for_an_english_profile() {
    let rows = vec![
        with("french-only", |c| c.audio_languages = langs(&["fr"])),
        with("multi-audio", |c| c.audio_flags = langs(&["multi"])),
        source("unmarked"),
        with("dual-audio-gb", |c| {
            c.audio_languages = langs(&["en"]);
            c.audio_flags = langs(&["dual"]);
        }),
    ];
    // Original unknown: unmarked and Multi Audio tie and keep input order.
    assert_eq!(
        ranked(rows.clone(), &english(), None, None),
        ["dual-audio-gb", "multi-audio", "unmarked", "french-only"]
    );
    // Japanese original: Multi Audio (likely contains the dub) beats unmarked.
    assert_eq!(
        ranked(rows.clone(), &english(), Some("ja"), None),
        ["dual-audio-gb", "multi-audio", "unmarked", "french-only"]
    );
    // English original: unmarked ties with explicit English.
    assert_eq!(
        ranked(rows, &english(), Some("en"), None),
        ["unmarked", "dual-audio-gb", "multi-audio", "french-only"]
    );
}

#[test]
fn excluded_rows_sink_below_everything() {
    let rows = vec![
        with("cam", |c| {
            c.release_type = Some("cam".into());
            c.audio_languages = langs(&["en"]);
            c.quality_height = Some(2160);
        }),
        with("3d", |c| c.three_d = true),
        with("dead-torrent", |c| {
            c.delivery = Some("torrent".into());
            c.seeders = Some(4);
        }),
        with("unknown-seeders", |c| c.delivery = Some("torrent".into())),
        with("french-480", |c| {
            c.audio_languages = langs(&["fr"]);
            c.quality_height = Some(480);
        }),
    ];
    assert_eq!(
        ranked(rows, &english(), None, None),
        ["unknown-seeders", "french-480", "cam", "3d", "dead-torrent"]
    );
}

#[test]
fn language_precedes_quality_cap_and_resolution() {
    let rows = vec![
        with("fr-2160", |c| {
            c.audio_languages = langs(&["fr"]);
            c.quality_height = Some(2160);
        }),
        with("en-unknown", |c| {
            c.audio_languages = langs(&["en"]);
            c.quality_height = None;
        }),
        with("en-720", |c| {
            c.audio_languages = langs(&["en"]);
            c.quality_height = Some(720);
        }),
        with("en-2160", |c| {
            c.audio_languages = langs(&["en"]);
            c.quality_height = Some(2160);
        }),
    ];
    let mut capped = english();
    capped.max_quality_height = Some(1080);
    assert_eq!(
        ranked(rows.clone(), &capped, None, None),
        ["en-720", "en-unknown", "en-2160", "fr-2160"]
    );
    assert_eq!(
        ranked(rows, &english(), None, None),
        ["en-2160", "en-720", "en-unknown", "fr-2160"]
    );
}

#[test]
fn device_height_caps_quality_with_the_profile_cap() {
    let rows = || {
        [2160, 1440, 1080, 720]
            .into_iter()
            .map(|h| with(&h.to_string(), |c| c.quality_height = Some(h)))
            .collect::<Vec<_>>()
    };
    let any = ProfilePreferences::default();
    assert_eq!(
        ranked(rows(), &any, None, None),
        ["2160", "1440", "1080", "720"]
    );
    // Device only: a 1080p device with "Best available". Over the cap, the
    // closest height to what the device shows comes first.
    assert_eq!(
        ranked(rows(), &any, None, Some(1080)),
        ["1080", "720", "1440", "2160"]
    );
    // Profile only.
    let mut capped = ProfilePreferences::default();
    capped.max_quality_height = Some(1440);
    assert_eq!(
        ranked(rows(), &capped, None, None),
        ["1440", "1080", "720", "2160"]
    );
    // Both: the lower cap wins either way round.
    assert_eq!(
        ranked(rows(), &capped, None, Some(720)),
        ["720", "1080", "1440", "2160"]
    );
    capped.max_quality_height = Some(720);
    assert_eq!(
        ranked(rows(), &capped, None, Some(2160)),
        ["720", "1080", "1440", "2160"]
    );
}

#[test]
fn release_type_order_at_equal_height() {
    let rows = [
        "hdtv",
        "remux",
        "",
        "webrip",
        "bluray",
        "web_dl",
        "other_rip",
        "dvd",
    ]
    .into_iter()
    .map(|kind| {
        with(if kind.is_empty() { "unknown" } else { kind }, |c| {
            c.release_type = (!kind.is_empty()).then(|| kind.into())
        })
    })
    .collect::<Vec<_>>();
    assert_eq!(
        ranked(rows, &english(), None, None),
        [
            "bluray",
            "web_dl",
            "webrip",
            "unknown",
            "remux",
            "hdtv",
            "other_rip",
            "dvd"
        ]
    );
    // Resolution still precedes release type.
    let rows = vec![
        with("web-720", |c| {
            c.release_type = Some("web_dl".into());
            c.quality_height = Some(720);
        }),
        with("remux-1080", |c| c.release_type = Some("remux".into())),
    ];
    assert_eq!(
        ranked(rows, &english(), None, None),
        ["remux-1080", "web-720"]
    );
}

#[test]
fn hdr_preference_any_prefer_avoid() {
    let rows = vec![
        source("sdr"),
        with("dv", |c| c.dynamic_range = langs(&["dv"])),
        with("hlg", |c| c.dynamic_range = langs(&["hlg"])),
    ];
    let mut preferences = english();
    assert_eq!(
        ranked(rows.clone(), &preferences, None, None),
        ["sdr", "dv", "hlg"]
    );
    preferences.hdr_preference = HdrPreference::Prefer;
    assert_eq!(
        ranked(rows.clone(), &preferences, None, None),
        ["dv", "hlg", "sdr"]
    );
    preferences.hdr_preference = HdrPreference::Avoid;
    assert_eq!(ranked(rows, &preferences, None, None), ["sdr", "dv", "hlg"]);
}

#[test]
fn direct_before_torrents_then_seeders() {
    let torrent = |id: &str, seeders: Option<u32>| {
        with(id, |c| {
            c.delivery = Some("torrent".into());
            c.seeders = seeders;
        })
    };
    let rows = vec![
        torrent("t-unknown", None),
        torrent("t-10", Some(10)),
        source("direct"),
        torrent("t-500", Some(500)),
        with("legacy", |c| c.delivery = None),
    ];
    assert_eq!(
        ranked(rows, &english(), None, None),
        ["direct", "legacy", "t-500", "t-10", "t-unknown"]
    );
}

#[test]
fn new_fields_are_omitted_when_unknown_and_old_payloads_parse() {
    let old = serde_json::json!({"id":"a","provider_id":"p","label":"L","detail":null,"quality_height":1080,"audio_languages":["en"],"presentation":null});
    let parsed: SourceCandidate = serde_json::from_value(old.clone()).unwrap();
    assert!(parsed.release_type.is_none() && !parsed.three_d && parsed.delivery.is_none());
    assert_eq!(serde_json::to_value(&parsed).unwrap(), old);
    let full = with("b", |c| {
        c.release_type = Some("web_dl".into());
        c.three_d = true;
        c.dynamic_range = langs(&["dv", "hdr10"]);
        c.audio_flags = langs(&["dual"]);
        c.seeders = Some(7);
        c.delivery = Some("torrent".into());
    });
    let wire = serde_json::to_value(&full).unwrap();
    for (key, value) in [
        ("release_type", serde_json::json!("web_dl")),
        ("three_d", serde_json::json!(true)),
        ("dynamic_range", serde_json::json!(["dv", "hdr10"])),
        ("audio_flags", serde_json::json!(["dual"])),
        ("seeders", serde_json::json!(7)),
        ("delivery", serde_json::json!("torrent")),
    ] {
        assert_eq!(wire[key], value, "{key}");
    }
}
