use serde_json::{Value, json};
use viptv_core::{CoreBridge, normalize};
fn event(core: &CoreBridge, event: Value) -> Vec<Value> {
    serde_json::from_str(&core.update(event.to_string()).unwrap()).unwrap()
}
fn resolve(core: &CoreBridge, request: &Value, result: Value) -> Vec<Value> {
    serde_json::from_str(
        &core
            .resolve(request["id"].as_u64().unwrap() as u32, result.to_string())
            .unwrap(),
    )
    .unwrap()
}
fn effect(requests: &[Value], name: &str) -> Value {
    requests
        .iter()
        .find(|r| r["effect"].get(name).is_some())
        .unwrap()
        .clone()
}
fn view(core: &CoreBridge) -> Value {
    serde_json::from_str(&core.view().unwrap()).unwrap()
}
fn session(profile: Value) -> Value {
    json!({"sessionId":"session-1","accountId":"account-1","profileId":profile,"accessToken":"fake-access","refreshToken":"fake-refresh","expiresIn":3600})
}
fn me(profile: Value) -> Value {
    json!({"account":{"id":"account-1","username":"viewer","name":"Viewer","role":"member"},"profiles":[{"id":"profile-1","name":"Main","setup_complete":true}],"profile_id":profile,"restricted":false,"profile_setup_required":false})
}
fn http(core: &CoreBridge, request: &Value, status: u16, body: Value) -> Vec<Value> {
    resolve(
        core,
        request,
        json!({"Ok":{"status":status,"headers":[],"body":serde_json::to_vec(&body).unwrap()}}),
    )
}
fn begin(core: &CoreBridge) -> Value {
    effect(
        &event(
            core,
            json!({"Begin":{"origin":"https://example.test","allowInsecurePreview":false}}),
        ),
        "Storage",
    )
}
fn restored(core: &CoreBridge, profile: Value) -> Value {
    let load = begin(core);
    effect(
        &resolve(core, &load, json!({"Ok":session(profile).to_string()})),
        "Http",
    )
}
#[test]
fn absent_storage_enters_pairing_only_after_load() {
    let c = CoreBridge::new();
    assert_eq!(view(&c)["phase"], "Starting");
    let load = begin(&c);
    assert_eq!(view(&c)["phase"], "Restoring");
    resolve(&c, &load, json!({"Ok":null}));
    assert_eq!(view(&c)["phase"], "Pairing");
}
#[test]
fn identity_restores_authorized_profile_without_auth_flash() {
    let c = CoreBridge::new();
    let request = restored(&c, json!("profile-1"));
    assert_eq!(view(&c)["phase"], "Checking");
    http(&c, &request, 200, me(json!("profile-1")));
    assert_eq!(view(&c)["phase"], "Ready");
    assert_eq!(view(&c)["selectedProfileId"], "profile-1");
    assert!(!c.view().unwrap().contains("fake-access"));
}
#[test]
fn confirmed_profile_save_does_not_repeat_identity_request() {
    let core = CoreBridge::new();
    let request = restored(&core, Value::Null);
    let effects = http(&core, &request, 200, me(json!("profile-1")));
    let save = effect(&effects, "Storage");
    let effects = resolve(&core, &save, json!({"Ok":null}));
    assert_eq!(view(&core)["phase"], "Ready");
    assert_eq!(view(&core)["selectedProfileId"], "profile-1");
    assert!(effects.iter().all(|r| r["effect"].get("Http").is_none()));
    effect(&effects, "Render");
}
#[test]
fn confirmed_profile_save_preserves_storage_failure_and_epoch_fences() {
    for stale in [false, true] {
        let core = CoreBridge::new();
        let request = restored(&core, Value::Null);
        let save = effect(
            &http(&core, &request, 200, me(json!("profile-1"))),
            "Storage",
        );
        if stale {
            begin(&core);
        }
        let effects = resolve(&core, &save, json!({"Err":"unavailable"}));
        if stale {
            assert!(effects.is_empty());
            assert_eq!(view(&core)["phase"], "Restoring");
        } else {
            assert_eq!(view(&core)["phase"], "Error");
            effect(&effects, "Render");
            effect(&event(&core, json!("Retry")), "Http");
        }
    }
}
#[test]
fn identity_restores_profile_with_nullable_avatar_choice() {
    let core = CoreBridge::new();
    let request = restored(&core, json!("profile-1"));
    let mut identity = me(json!("profile-1"));
    identity["profiles"][0]["avatar_choice"] = Value::Null;
    let normalized: Value = serde_json::from_str(
        &normalize("identity".into(), identity.to_string(), "".into()).unwrap(),
    )
    .unwrap();
    assert_eq!(normalized["profiles"][0]["avatarChoice"], Value::Null);
    http(&core, &request, 200, identity);
    assert_eq!(view(&core)["phase"], "Ready");
    assert_eq!(view(&core)["selectedProfileId"], "profile-1");
}
#[test]
fn identity_restores_profile_with_empty_optional_account_display_name() {
    let core = CoreBridge::new();
    let request = restored(&core, json!("profile-1"));
    let mut identity = me(json!("profile-1"));
    identity["account"]["name"] = json!("");
    identity["profiles"][0]["avatar_choice"] = Value::Null;
    let normalized: Value = serde_json::from_str(
        &normalize("identity".into(), identity.to_string(), "".into()).unwrap(),
    )
    .unwrap();
    assert_eq!(normalized["account"]["name"], "");
    http(&core, &request, 200, identity);
    assert_eq!(view(&core)["phase"], "Ready");
    assert_eq!(view(&core)["selectedProfileId"], "profile-1");
}
#[test]
fn malformed_successful_identity_is_a_response_error_not_a_network_outage() {
    let core = CoreBridge::new();
    let request = restored(&core, json!("profile-1"));
    let mut malformed = me(json!("profile-1"));
    malformed["account"]["name"] = Value::Null;
    http(&core, &request, 200, malformed);
    assert_eq!(view(&core)["phase"], "Error");
    assert_eq!(view(&core)["error"], "Invalid server response");
    assert_eq!(view(&core)["errorStatus"], 200);
    let retry = effect(&event(&core, json!("Retry")), "Http");
    assert!(
        retry["effect"]["Http"]["url"]
            .as_str()
            .unwrap()
            .ends_with("/api/auth/me")
    );
}
#[test]
fn transient_failure_retains_tokens_for_retry() {
    let c = CoreBridge::new();
    let request = restored(&c, json!("profile-1"));
    resolve(&c, &request, json!({"Err":{"Io":"offline"}}));
    assert_eq!(view(&c)["phase"], "Error");
    let retry = effect(&event(&c, json!("Retry")), "Http");
    assert!(
        retry["effect"]["Http"]["headers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["value"] == "Bearer fake-access")
    );
}
#[test]
fn missing_remembered_profile_returns_chooser() {
    let c = CoreBridge::new();
    let request = restored(&c, json!("deleted"));
    http(&c, &request, 200, me(Value::Null));
    assert_eq!(view(&c)["phase"], "Profiles");
}
#[test]
fn remembered_profile_is_selected_on_server_before_ready() {
    let c = CoreBridge::new();
    let request = restored(&c, json!("profile-1"));
    let select = effect(&http(&c, &request, 200, me(Value::Null)), "Http");
    assert_eq!(view(&c)["phase"], "Selecting");
    assert!(
        select["effect"]["Http"]["url"]
            .as_str()
            .unwrap()
            .ends_with("/api/auth/profile")
    );
    http(&c, &select, 403, json!({}));
    assert_eq!(view(&c)["phase"], "Error");
    assert_eq!(view(&c)["selectedProfileId"], Value::Null);
}
#[test]
fn definitive_refresh_revocation_clears_after_server_response() {
    let c = CoreBridge::new();
    let request = restored(&c, Value::Null);
    let refresh = effect(&http(&c, &request, 401, json!({})), "Http");
    let clear = effect(&http(&c, &refresh, 401, json!({})), "Storage");
    assert_eq!(clear["effect"]["Storage"], "Clear");
    resolve(&c, &clear, json!({"Ok":null}));
    assert_eq!(view(&c)["phase"], "Pairing");
}
#[test]
fn stale_identity_does_not_replace_new_session() {
    let c = CoreBridge::new();
    let old = restored(&c, json!("profile-1"));
    event(
        &c,
        json!({"AdoptSession":{"tokensJson":session(Value::Null).to_string()}}),
    );
    http(&c, &old, 200, me(json!("profile-1")));
    assert_eq!(view(&c)["phase"], "Checking");
    assert_eq!(view(&c)["identity"], Value::Null);
}
#[test]
fn rejected_logout_retains_grant() {
    let c = CoreBridge::new();
    let request = restored(&c, json!("profile-1"));
    http(&c, &request, 200, me(json!("profile-1")));
    let logout = effect(&event(&c, json!("SignOut")), "Http");
    let effects = http(&c, &logout, 403, json!({}));
    assert_eq!(view(&c)["errorStatus"], 403);
    assert!(effects.iter().all(|r| r["effect"].get("Storage").is_none()));
}
#[test]
fn playback_urls_are_capability_paths_or_original_sources() {
    // A root-relative playback URL must stay a same-origin /media/ capability,
    // and no playback URL ever carries embedded credentials.
    assert!(
        normalize(
            "playback".into(),
            json!({"id":"s","url":"/api/private"}).to_string(),
            "https://example.test".into()
        )
        .is_err()
    );
    assert!(
        normalize(
            "playback".into(),
            json!({"id":"s","url":"https://user:pass@evil.test/media/x"}).to_string(),
            "https://example.test".into()
        )
        .is_err()
    );
    let proxy: Value = serde_json::from_str(
        &normalize(
            "playback".into(),
            json!({"id":"s","url":"/media/s/cap/index.m3u8"}).to_string(),
            "https://example.test".into(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(proxy["url"], "https://example.test/media/s/cap/index.m3u8");
    assert!(proxy.as_object().unwrap().get("authorization").is_none());
    // A direct-URL session hands the ORIGINAL absolute source URL to the
    // client; it passes through unchanged with its source authorization.
    let direct: Value = serde_json::from_str(
        &normalize(
            "playback".into(),
            json!({"id":"s","url":"https://provider.test/stream.mkv","authorization":{"cookie":"session=1","user_agent":"VIPTV Desktop","headers":{"Referer":"https://provider.test/watch","X-Stream":"opaque","Host":"wrong.test","Bad":"value\\r\\nInjected: yes"}}}).to_string(),
            "https://example.test".into()
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(direct["url"], "https://provider.test/stream.mkv");
    assert_eq!(direct["authorization"]["cookie"], "session=1");
    assert_eq!(direct["authorization"]["userAgent"], "VIPTV Desktop");
    assert_eq!(
        direct["authorization"]["headers"]["Referer"],
        "https://provider.test/watch"
    );
    assert!(direct["authorization"]["headers"].get("Host").is_none());
    assert_eq!(
        normalize(
            "container".into(),
            json!({"url":"https://example.test/media/movie.M3U8?token=.mp4"}).to_string(),
            "https://example.test".into()
        )
        .unwrap(),
        "\"hls\""
    );
}
#[test]
fn shared_native_wasm_vectors() {
    let vectors: Value =
        serde_json::from_str(include_str!("../../../tests/bridge-vectors.json")).unwrap();
    for scenario in vectors.as_array().unwrap() {
        let core = CoreBridge::new();
        let mut requests = vec![];
        for step in scenario["steps"].as_array().unwrap() {
            requests = if let Some(e) = step.get("event") {
                event(&core, e.clone())
            } else {
                let request = effect(&requests, step["effect"].as_str().unwrap());
                resolve(&core, &request, step["output"].clone())
            };
            let model = view(&core);
            assert_eq!(model["phase"], step["phase"], "{}", scenario["name"]);
            if let Some(profile) = step.get("profileId") {
                assert_eq!(&model["selectedProfileId"], profile);
            }
        }
    }
}
#[test]
fn token_rotation_preserves_remembered_profile_until_server_selection() {
    let core = CoreBridge::new();
    let request = restored(&core, json!("profile-1"));
    let refresh = effect(&http(&core, &request, 401, json!({})), "Http");
    assert!(
        !refresh["effect"]["Http"]["headers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["name"] == "Authorization")
    );
    let rotated = json!({"session_id":"session-1","account_id":"account-1","profile_id":null,"access_token":"rotated-access","refresh_token":"rotated-refresh","expires_in":3600});
    let save = effect(&http(&core, &refresh, 200, rotated), "Storage");
    let saved: Value =
        serde_json::from_str(save["effect"]["Storage"]["Save"].as_str().unwrap()).unwrap();
    assert_eq!(saved["profileId"], "profile-1");
    assert_eq!(saved["accessToken"], "rotated-access");
    let request = effect(&resolve(&core, &save, json!({"Ok":null})), "Http");
    let select = effect(&http(&core, &request, 200, me(Value::Null)), "Http");
    assert!(
        select["effect"]["Http"]["url"]
            .as_str()
            .unwrap()
            .ends_with("/api/auth/profile")
    );
    assert_eq!(view(&core)["phase"], "Selecting");
}

#[test]
fn accepted_profile_selection_with_missing_echo_errors_once() {
    let c = CoreBridge::new();
    let identity = restored(&c, json!("profile-1"));
    let select = effect(&http(&c, &identity, 200, me(Value::Null)), "Http");
    assert!(
        select["effect"]["Http"]["url"]
            .as_str()
            .unwrap()
            .ends_with("/auth/profile")
    );
    let save = effect(&http(&c, &select, 200, json!({})), "Storage");
    let refresh = effect(&resolve(&c, &save, json!({"Ok":null})), "Http");
    let effects = http(&c, &refresh, 200, me(Value::Null));
    assert_eq!(view(&c)["phase"], "Error");
    assert!(view(&c)["selectedProfileId"].is_null());
    assert!(effects.iter().all(|e| e["effect"].get("Http").is_none()));
}
#[test]
fn post_selection_deleted_or_incomplete_profile_cannot_be_restored() {
    for profiles in [
        json!([]),
        json!([{"id":"profile-1","name":"Main","setup_complete":false}]),
    ] {
        let c = CoreBridge::new();
        let identity = restored(&c, json!("profile-1"));
        let select = effect(&http(&c, &identity, 200, me(Value::Null)), "Http");
        let save = effect(&http(&c, &select, 200, json!({})), "Storage");
        let refresh = effect(&resolve(&c, &save, json!({"Ok":null})), "Http");
        let mut response = me(Value::Null);
        response["profiles"] = profiles;
        let effects = http(&c, &refresh, 200, response);
        assert_eq!(view(&c)["phase"], "Profiles");
        assert!(effects.iter().all(|e| e["effect"].get("Http").is_none()));
    }
}

#[test]
fn catalogs_preserve_addon_defined_types_and_names() {
    let input = json!([
        {"id":"top","name":"Popular","type":"movie","addon_id":1,"addon_name":"Cinemeta"},
        {"id":"anime","name":"Anime","type":"anime","addon_id":4,"addon_name":"AIOMetadata"},
        {"id":"collections","type":"collection","addon_id":4},
        {"id":"anime-series","type":"anime.series","addon_id":4},
        {"id":"anime-movies","type":"anime.movie","addon_id":4}
    ]);
    let output: Value =
        serde_json::from_str(&normalize("catalogs".into(), input.to_string(), "".into()).unwrap())
            .unwrap();
    assert_eq!(output.as_array().unwrap().len(), 5);
    assert_eq!(output[1]["type"], "anime");
    assert_eq!(output[2]["type"], "collection");
    assert_eq!(output[3]["type"], "anime.series");
    assert_eq!(output[4]["type"], "anime.movie");
    assert_eq!(output[1]["addonName"], "AIOMetadata");
}

#[test]
fn addon_catalog_namespaces_use_returned_media_types_and_report_unsupported_rows() {
    for (catalog_type, media_type) in [
        ("anime", "series"),
        ("anime.series", "series"),
        ("anime.movie", "movie"),
        ("collection", "movie"),
    ] {
        let input = json!({"type":catalog_type,"response":{"metas":[{"id":"fixture","name":"Example","type":media_type}],"has_more":false}});
        let output: Value = serde_json::from_str(
            &normalize("discoverResponse".into(), input.to_string(), "".into()).unwrap(),
        )
        .unwrap();
        assert_eq!(output["items"][0]["type"], media_type);
    }
    let input = json!({"type":"custom","response":{"metas":[{"id":"unknown","type":"custom"},{"id":"known","type":"movie"}],"has_more":false}});
    let output: Value = serde_json::from_str(
        &normalize("discoverResponse".into(), input.to_string(), "".into()).unwrap(),
    )
    .unwrap();
    assert_eq!(output["items"].as_array().unwrap().len(), 1);
    assert_eq!(output["unsupportedCount"], 1);
}

#[test]
fn stream_discovery_requests_and_polling_steps_share_one_policy() {
    // The POST body is the same item request shape metadata uses, and the
    // poll path carries the cursor so neither client rebuilds endpoints.
    let item = json!({"id":"tt123","type":"movie","name":"Example Movie"});
    let request: Value = serde_json::from_str(
        &normalize(
            "request".into(),
            json!({"operation":"sources","item":item}).to_string(),
            "".into(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(request["method"], "POST");
    assert_eq!(request["path"], "/api/streams");
    assert_eq!(request["body"]["id"], "tt123");
    let poll: Value = serde_json::from_str(
        &normalize(
            "request".into(),
            json!({"operation":"sourcesPoll","id":"job-1","after":7}).to_string(),
            "".into(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(poll["method"], "GET");
    assert_eq!(poll["path"], "/api/streams/job-1?after=7");
    assert!(poll["body"].is_null());
    assert!(
        normalize(
            "request".into(),
            json!({"operation":"sourcesPoll","id":"","after":0}).to_string(),
            "".into()
        )
        .is_err(),
        "a blank job id must not produce a poll path"
    );

    // The polling step owns the cursor, deduplication and completion rules.
    let page = json!({"events":[
        {"seq":3,"source":"iptv","streams":[{"id":"a","name":"A","url":"https://provider.test/a.m3u8"}]},
        {"seq":4,"source":"addon","streams":[{"id":"b","name":"B","url":"https://provider.test/b.mkv"}]}
    ],"done":false});
    let first: Value = serde_json::from_str(
        &normalize(
            "sourcesPollStep".into(),
            json!({"poll":page}).to_string(),
            "".into(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(first["state"]["after"], 4);
    assert_eq!(first["state"]["polls"], 1);
    assert_eq!(first["sources"].as_array().unwrap().len(), 2);
    assert!(!first["done"].as_bool().unwrap());

    // A replayed page adds nothing: the cursor did not move and the same
    // source ids are already accumulated.
    let replay = json!({"events":[
        {"seq":4,"source":"addon","streams":[{"id":"b","name":"B","url":"https://provider.test/b.mkv"}]}
    ],"done":false});
    let second: Value = serde_json::from_str(
        &normalize(
            "sourcesPollStep".into(),
            json!({"state":first["state"],"poll":replay}).to_string(),
            "".into(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(second["sources"].as_array().unwrap().len(), 2);
    assert_eq!(second["state"]["polls"], 2);

    // Server completion ends the loop regardless of budget...
    let finished = json!({"events":[],"done":true});
    let third: Value = serde_json::from_str(
        &normalize(
            "sourcesPollStep".into(),
            json!({"state":second["state"],"poll":finished}).to_string(),
            "".into(),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(third["done"].as_bool().unwrap());

    // ...and the budget ends it even when the server keeps polling.
    let mut state = json!({"after":0,"sources":[],"polls":0});
    for step in 0..119 {
        let never_done = json!({"events":[],"done":false});
        let output: Value = serde_json::from_str(
            &normalize(
                "sourcesPollStep".into(),
                json!({"state":state,"poll":never_done}).to_string(),
                "".into(),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(!output["done"].as_bool().unwrap() || step == 118);
        state = output["state"].clone();
    }
    let budget_hit: Value = serde_json::from_str(
        &normalize(
            "sourcesPollStep".into(),
            json!({"state":state,"poll":json!({"events":[],"done":false})}).to_string(),
            "".into(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(budget_hit["state"]["polls"], 120);
    assert!(
        budget_hit["done"].as_bool().unwrap(),
        "the three-minute budget must terminate discovery"
    );
    assert!(
        normalize("sourcesPollStep".into(), json!({}).to_string(), "".into()).is_err(),
        "a missing poll page is invalid input"
    );
}
#[test]
fn raw_json_deserialization_preserves_all_value_kinds() {
    use serde::Deserialize;
    use viptv_core::dto::JsonValue;

    let wire = r#"[null,true,false,-1,18446744073709551615,1.25,"映画 Café 🎬",[],{},[[false,{"id":"x"}]]]"#;
    let expected = JsonValue::Array(vec![
        JsonValue::Null,
        JsonValue::Boolean(true),
        JsonValue::Boolean(false),
        JsonValue::Number(-1.0),
        JsonValue::Number(u64::MAX as f64),
        JsonValue::Number(1.25),
        JsonValue::String("映画 Café 🎬".into()),
        JsonValue::Array(vec![]),
        JsonValue::Object(Default::default()),
        JsonValue::Array(vec![JsonValue::Array(vec![
            JsonValue::Boolean(false),
            JsonValue::Object([("id".into(), JsonValue::String("x".into()))].into()),
        ])]),
    ]);
    let value: serde_json::Value = serde_json::from_str(wire).unwrap();
    assert_eq!(serde_json::from_str::<JsonValue>(wire).unwrap(), expected);
    assert_eq!(JsonValue::deserialize(&value).unwrap(), expected);
    assert_eq!(JsonValue::deserialize(value).unwrap(), expected);
}
