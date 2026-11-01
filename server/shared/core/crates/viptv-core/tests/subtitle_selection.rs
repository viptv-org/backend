use serde_json::{Value, json};

fn call(kind: &str, input: Value) -> Value {
    serde_json::from_str(
        &viptv_core::normalize(kind.into(), input.to_string(), String::new()).unwrap(),
    )
    .unwrap()
}

fn select(mode: &str, audio: Value, tracks: Value) -> Value {
    call(
        "subtitleSelection",
        json!({"mode":mode,"preferredAudioLanguage":"en","subtitleLanguage":"en",
            "audioLanguage":audio,"subtitleTracks":tracks}),
    )
}

fn tracks() -> Value {
    json!([
        {"id":"forced","language":"eng","forced":true},
        {"id":"spanish","language":"spa"},
        {"id":"english","language":"en-US"}
    ])
}

#[test]
fn always_and_never_ignore_the_audio_language() {
    let always = select("always", json!("en"), tracks());
    assert_eq!(always["show"], true);
    assert_eq!(always["trackId"], "english");
    for audio in [json!("ja"), json!("en"), Value::Null] {
        let never = select("never", audio, tracks());
        assert_eq!(never["show"], false);
        assert!(never["trackId"].is_null());
    }
}

#[test]
fn foreign_shows_subtitles_only_for_other_audio_languages() {
    for audio in ["jpn", "ja", "Japanese", "spa", "fr-CA"] {
        let choice = select("foreign", json!(audio), tracks());
        assert_eq!(choice["show"], true, "{audio}");
        assert_eq!(choice["foreignAudio"], true, "{audio}");
        assert_eq!(choice["trackId"], "english", "{audio}");
    }
    for audio in ["en", "eng", "English", "en-GB"] {
        let choice = select("foreign", json!(audio), tracks());
        assert_eq!(choice["show"], false, "{audio}");
        assert!(choice["trackId"].is_null(), "{audio}");
    }
}

#[test]
fn foreign_treats_unknown_audio_as_not_foreign() {
    for audio in [
        Value::Null,
        json!(""),
        json!("und"),
        json!("UND"),
        json!("mul"),
    ] {
        let choice = select("foreign", audio.clone(), tracks());
        assert_eq!(choice["show"], false, "{audio}");
    }
}

#[test]
fn foreign_compares_against_a_non_english_preferred_audio() {
    let choice = call(
        "subtitleSelection",
        json!({"mode":"foreign","preferredAudioLanguage":"ja","subtitleLanguage":"es",
            "audioLanguage":"eng","subtitleTracks":tracks()}),
    );
    assert_eq!(choice["show"], true);
    assert_eq!(choice["trackId"], "spanish");
    let native = call(
        "subtitleSelection",
        json!({"mode":"foreign","preferredAudioLanguage":"ja","subtitleLanguage":"es",
            "audioLanguage":"jpn","subtitleTracks":tracks()}),
    );
    assert_eq!(native["show"], false);
}

#[test]
fn missing_preferred_subtitle_track_keeps_the_decision_without_a_track() {
    let choice = call(
        "subtitleSelection",
        json!({"mode":"foreign","preferredAudioLanguage":"en","subtitleLanguage":"de",
            "audioLanguage":"jpn","subtitleTracks":tracks()}),
    );
    assert_eq!(choice["show"], true);
    assert!(choice["trackId"].is_null());
    let none = select("always", json!("en"), json!([]));
    assert_eq!(none["show"], true);
    assert!(none["trackId"].is_null());
}

#[test]
fn preferences_project_a_subtitle_mode_for_old_and_new_servers() {
    let base = json!({"audio_language":"en","subtitle_language":"en","subtitle_size":"normal",
        "subtitle_style":"system","quality":"auto","autoplay":true});
    let with = |fields: Value| {
        let mut value = base.clone();
        for (key, field) in fields.as_object().unwrap() {
            value[key] = field.clone();
        }
        call("preferences", value)
    };
    assert_eq!(
        with(json!({"subtitles_enabled":true}))["subtitleMode"],
        "always"
    );
    assert_eq!(
        with(json!({"subtitles_enabled":false}))["subtitleMode"],
        "never"
    );
    let foreign = with(json!({"subtitles_enabled":true,"subtitle_mode":"foreign"}));
    assert_eq!(foreign["subtitleMode"], "foreign");
    assert_eq!(foreign["subtitlesEnabled"], true);
    let android = call(
        "androidPreferences",
        json!({"subtitle_mode":"foreign","subtitles_enabled":true}),
    );
    assert_eq!(android["subtitleMode"], "foreign");
    assert!(
        viptv_core::normalize(
            "preferences".into(),
            {
                let mut value = base.clone();
                value["subtitles_enabled"] = json!(true);
                value["subtitle_mode"] = json!("sometimes");
                value.to_string()
            },
            String::new()
        )
        .is_err()
    );
}
