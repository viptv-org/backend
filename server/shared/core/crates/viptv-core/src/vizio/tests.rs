use super::response::parse_protocol_response;
use super::*;
use crate::dto::JsonValue;
use serde_json::{Value, json};

fn input(extra: Value) -> String {
    let mut base = json!({"host":"https://192.0.2.4/","authToken":"secret"});
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    base.to_string()
}

#[test]
fn upstream_remote_and_conjure_contract_is_preserved() {
    let VizioRequestResult::Ok(home) = plan_request("key", &input(json!({"key":"HOME"}))) else {
        panic!()
    };
    assert_eq!(home.url, "https://192.0.2.4:7345/key_command/");
    assert_eq!(
        home.body.unwrap()["KEYLIST"],
        json_value(json!([{"CODESET":4,"CODE":15,"ACTION":"KEYPRESS"}]))
    );
    let VizioRequestResult::Ok(conjure) = plan_request(
        "launchConjure",
        &input(json!({"url":"https://example.invalid/tv/"})),
    ) else {
        panic!()
    };
    assert_eq!(
        conjure.body.unwrap()["VALUE"],
        json_value(json!({"APP_ID":"17","NAME_SPACE":4,"MESSAGE":"https://example.invalid/tv/"}))
    );
}

#[test]
fn pairing_and_protocol_status_are_case_insensitive() {
    let response = parse_protocol_response(
        200,
        r#"{"status":{"result":"SUCCESS"},"item":{"CHALLENGE_TYPE":1,"PAIRING_REQ_TOKEN":42}}"#,
        false,
    )
    .unwrap();
    let challenge = pairing_challenge(&response).unwrap();
    assert_eq!(challenge.challenge_type, 1);
    assert_eq!(challenge.token, 42);
    let VizioResponseResult::Err(error) = parse_response(
        200,
        r#"{"STATUS":{"RESULT":"HASHVAL_ERROR","DETAIL":"stale"}}"#,
        false,
    ) else {
        panic!()
    };
    assert_eq!(error.kind, VizioFailureKind::InvalidParameter);
}

#[test]
fn text_limits_and_input_freshness_payload_are_enforced() {
    assert!(matches!(
        plan_request("text", &input(json!({"text":"Air 📺"}))),
        VizioRequestResult::Err(_)
    ));
    let VizioRequestResult::Ok(request) =
        plan_request("setInput", &input(json!({"cname":"hdmi2","hashValue":12})))
    else {
        panic!()
    };
    let body = request.body.unwrap();
    assert_eq!(body["VALUE"], JsonValue::String("hdmi2".into()));
    assert_eq!(body["HASHVAL"], JsonValue::Number(12.0));
}

#[test]
fn discovery_is_bounded_and_platform_limits_are_explicit() {
    let candidates = discovery_candidates("192.168.1").unwrap();
    assert_eq!(candidates.len(), 508);
    assert_eq!(candidates[0].host, "192.168.1.1:7345");
    assert!(matches!(
        platform_support("android").transport,
        VizioTransportSupport::Native
    ));
    assert!(matches!(
        platform_support("tizen").transport,
        VizioTransportSupport::Unavailable
    ));
    assert!(matches!(
        platform_support("android-tv").transport,
        VizioTransportSupport::Unavailable
    ));
}

#[test]
fn controller_adopts_pairing_token_before_next_command() {
    let mut controller = VizioController::new(
        r#"{"host":"192.0.2.8","deviceId":"stable-device","deviceName":"VIPTV mobile"}"#,
    )
    .unwrap();
    let start = controller.start(
        "finishPair",
        r#"{"challengeType":1,"token":42,"pin":"1234"}"#,
    );
    let id = start.request_id.unwrap();
    let done = controller.resolve(
        id,
        200,
        r#"{"STATUS":{"RESULT":"SUCCESS"},"ITEM":{"AUTH_TOKEN":"new-secret"}}"#,
    );
    assert_eq!(done.kind, VizioControllerOutputKind::Complete);
    assert!(done.credential_changed);
    assert_eq!(controller.auth_token().as_deref(), Some("new-secret"));

    let command = controller.start("powerOn", "{}");
    assert_eq!(
        command
            .request
            .unwrap()
            .headers
            .get("AUTH")
            .map(String::as_str),
        Some("new-secret")
    );
}

#[test]
fn controller_uses_target_cname_and_a_fresh_current_input_hash() {
    let mut controller = paired_controller();
    let first = controller.start("setInput", r#"{"input":"Game console"}"#);
    assert!(
        first
            .request
            .as_ref()
            .unwrap()
            .url
            .ends_with("/devices/current_input")
    );
    let list = controller.resolve(
        first.request_id.unwrap(),
        200,
        &success(json!({"ITEM":{"CNAME":"current_input","VALUE":"hdmi1","HASHVAL":7}})),
    );
    assert!(
        list.request
            .as_ref()
            .unwrap()
            .url
            .ends_with("/devices/name_input")
    );
    let fresh = controller.resolve(
        list.request_id.unwrap(),
        200,
        &success(json!({"ITEMS":[
            {"CNAME":"hdmi1","NAME":"HDMI-1","VALUE":{"NAME":"Cable"}},
            {"CNAME":"hdmi2","NAME":"HDMI-2","VALUE":{"NAME":"Game console"}}
        ]})),
    );
    assert!(
        fresh
            .request
            .as_ref()
            .unwrap()
            .url
            .ends_with("/devices/current_input")
    );
    let write = controller.resolve(
        fresh.request_id.unwrap(),
        200,
        &success(json!({"ITEM":{"CNAME":"current_input","VALUE":"hdmi1","HASHVAL":99}})),
    );
    let body = write.request.unwrap().body.unwrap();
    assert_eq!(body["VALUE"], JsonValue::String("hdmi2".into()));
    assert_eq!(body["HASHVAL"], JsonValue::Number(99.0));
}

#[test]
fn controller_retries_a_stale_setting_hash_once() {
    let mut controller = paired_controller();
    let read = controller.start(
        "setSetting",
        r#"{"category":"audio","name":"volume","value":44}"#,
    );
    let first_write = controller.resolve(
        read.request_id.unwrap(),
        200,
        &setting_response("volume", 10, 0, 100),
    );
    assert_eq!(
        first_write.request.as_ref().unwrap().body.as_ref().unwrap()["HASHVAL"],
        JsonValue::Number(10.0)
    );
    let reread = controller.resolve(
        first_write.request_id.unwrap(),
        200,
        r#"{"STATUS":{"RESULT":"HASHVAL_ERROR"}}"#,
    );
    assert!(matches!(
        reread.request.as_ref().unwrap().method,
        VizioHttpMethod::Get
    ));
    let second_write = controller.resolve(
        reread.request_id.unwrap(),
        200,
        &setting_response("volume", 11, 0, 100),
    );
    assert_eq!(
        second_write
            .request
            .as_ref()
            .unwrap()
            .body
            .as_ref()
            .unwrap()["HASHVAL"],
        JsonValue::Number(11.0)
    );
    let failed = controller.resolve(
        second_write.request_id.unwrap(),
        200,
        r#"{"STATUS":{"RESULT":"HASHVAL_ERROR"}}"#,
    );
    assert_eq!(failed.kind, VizioControllerOutputKind::Error);
    assert_eq!(
        failed.error.unwrap().protocol_status.as_deref(),
        Some("HASHVAL_ERROR")
    );
}

#[test]
fn controller_serializes_commands_and_ignores_stale_responses() {
    let mut controller = paired_controller();
    let pending = controller.start("powerOn", "{}");
    assert_eq!(
        controller.start("powerOff", "{}").error.unwrap().kind,
        VizioFailureKind::Busy
    );
    assert_eq!(
        controller
            .resolve(pending.request_id.unwrap() + 1, 200, "{}")
            .kind,
        VizioControllerOutputKind::Error
    );
    assert!(controller.pending.is_some());
    controller.cancel();
    assert!(controller.pending.is_none());
}

#[test]
fn remote_table_and_status_redaction_match_the_upstream_contract() {
    let expected = [
        (VizioRemoteKey::SeekFwd, 2, 0),
        (VizioRemoteKey::SeekBack, 2, 1),
        (VizioRemoteKey::Pause, 2, 2),
        (VizioRemoteKey::Play, 2, 3),
        (VizioRemoteKey::Down, 3, 0),
        (VizioRemoteKey::Left, 3, 1),
        (VizioRemoteKey::Ok, 3, 2),
        (VizioRemoteKey::Right, 3, 7),
        (VizioRemoteKey::Up, 3, 8),
        (VizioRemoteKey::Back, 4, 0),
        (VizioRemoteKey::Smartcast, 4, 3),
        (VizioRemoteKey::CcToggle, 4, 4),
        (VizioRemoteKey::Info, 4, 6),
        (VizioRemoteKey::Menu, 4, 8),
        (VizioRemoteKey::Home, 4, 15),
        (VizioRemoteKey::VolDown, 5, 0),
        (VizioRemoteKey::VolUp, 5, 1),
        (VizioRemoteKey::MuteOff, 5, 2),
        (VizioRemoteKey::MuteOn, 5, 3),
        (VizioRemoteKey::MuteToggle, 5, 4),
        (VizioRemoteKey::PicMode, 6, 0),
        (VizioRemoteKey::PicSize, 6, 2),
        (VizioRemoteKey::InputNext, 7, 1),
        (VizioRemoteKey::ChDown, 8, 0),
        (VizioRemoteKey::ChUp, 8, 1),
        (VizioRemoteKey::ChPrev, 8, 2),
        (VizioRemoteKey::Exit, 9, 0),
        (VizioRemoteKey::PowOff, 11, 0),
        (VizioRemoteKey::PowOn, 11, 1),
        (VizioRemoteKey::PowToggle, 11, 2),
    ];
    for (key, code_set, code) in expected {
        let event = key.event(VizioRemoteAction::Keypress);
        assert_eq!((event.code_set, event.code), (code_set, code));
    }
    let VizioResponseResult::Err(error) = parse_response(
        200,
        r#"{"STATUS":{"RESULT":"token=should-not-escape"}}"#,
        false,
    ) else {
        panic!()
    };
    assert_eq!(error.protocol_status.as_deref(), Some("UNKNOWN"));
}

#[test]
fn explicit_https_port_is_preserved() {
    let VizioRequestResult::Ok(request) =
        plan_request("ping", r#"{"host":"https://example.test:443"}"#)
    else {
        panic!()
    };
    assert_eq!(request.url, "https://example.test/state/device/deviceinfo");
}

fn paired_controller() -> VizioController {
    VizioController::new(r#"{"host":"192.0.2.8","authToken":"secret"}"#).unwrap()
}

fn success(payload: Value) -> String {
    let mut object = payload.as_object().unwrap().clone();
    object.insert("STATUS".into(), json!({"RESULT":"SUCCESS"}));
    Value::Object(object).to_string()
}

fn setting_response(name: &str, hash: i64, minimum: i64, maximum: i64) -> String {
    success(json!({"ITEM":{
        "CNAME": name,
        "VALUE": 20,
        "HASHVAL": hash,
        "MINIMUM": minimum,
        "MAXIMUM": maximum
    }}))
}

fn json_value(value: Value) -> JsonValue {
    serde_json::from_value(value).unwrap()
}

#[test]
fn deviceinfo_name_extracts_and_trims_smartcast_identity() {
    let live = r#"{"STATUS":{"RESULT":"SUCCESS","DETAIL":"Success"},"URI":"/state/device/deviceinfo","ITEMS":[{"NAME":"VIZIO Device Info","CNAME":"deviceinfo","TYPE":"T_VIZIO_DEVICE_INFO_V1","VALUE":{"MODEL_NAME":"V655-G9","CAST_NAME":"living room 65\"","API_VERSION":"3.0.12-2507.0002"}}]}"#;
    assert_eq!(deviceinfo_name(live).as_deref(), Some("living room 65"));
    let no_cast_name = r#"{"STATUS":{"RESULT":"SUCCESS"},"ITEMS":[{"TYPE":"T_VIZIO_DEVICE_INFO_V1","VALUE":{"MODEL_NAME":"M55-C2"}}]}"#;
    assert_eq!(deviceinfo_name(no_cast_name).as_deref(), Some("M55-C2"));
}

#[test]
fn deviceinfo_name_rejects_non_vizio_documents() {
    let not_found = r#"{"STATUS":{"RESULT":"ERROR","DETAIL":"Not Found"}}"#;
    assert_eq!(deviceinfo_name(not_found), None);
    let wrong_type = r#"{"STATUS":{"RESULT":"SUCCESS"},"ITEMS":[{"TYPE":"T_SETTINGS_V1","VALUE":{"CAST_NAME":"router"}}]}"#;
    assert_eq!(deviceinfo_name(wrong_type), None);
    let empty_items = r#"{"STATUS":{"RESULT":"SUCCESS"},"ITEMS":[]}"#;
    assert_eq!(deviceinfo_name(empty_items), None);
    assert_eq!(deviceinfo_name("<html>Not Found</html>"), None);
    assert_eq!(deviceinfo_name(""), None);
}
