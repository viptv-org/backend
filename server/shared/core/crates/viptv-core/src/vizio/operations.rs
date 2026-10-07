use super::failure;
use super::request::{
    RequestInput, RequestLimits, build_request, normalize_host, required_i64, required_string,
    required_u64, setting_path, validate_conjure_url, validate_identity, validate_limits,
};
use super::response::validate_setting_value;
use super::types::{
    VizioFailure, VizioFailureKind, VizioHttpMethod, VizioRemoteAction, VizioRemoteEvent,
    VizioRemoteKey, VizioRequest,
};
use serde_json::{Map, Value, json};

const SETTINGS_ROOT: &str = "/menu_native/dynamic/tv_settings";
const DEFAULT_DEVICE_ID: &str = "viptv-smartcast";
const DEFAULT_DEVICE_NAME: &str = "VIPTV";

pub(super) fn request_for(
    operation: &str,
    input: RequestInput,
) -> Result<VizioRequest, VizioFailure> {
    let base = normalize_host(&input.host)?;
    let auth = input
        .auth_token
        .as_deref()
        .filter(|token| !token.is_empty());
    let timeout_millis = input.timeout_millis.unwrap_or(10_000);
    let max_response_bytes = input.max_response_bytes.unwrap_or(2 * 1024 * 1024);
    validate_limits(input.timeout_millis, input.max_response_bytes)?;
    let device_id = input.device_id.as_deref().unwrap_or(DEFAULT_DEVICE_ID);
    let device_name = input.device_name.as_deref().unwrap_or(DEFAULT_DEVICE_NAME);
    validate_identity(Some(device_id), "device ID")?;
    validate_identity(Some(device_name), "device name")?;
    let fields = &input.fields;
    let key_events = |events: Vec<VizioRemoteEvent>| {
        json!({"KEYLIST": events.into_iter().map(|event| json!({
            "CODESET": event.code_set,
            "CODE": event.code,
            "ACTION": action_name(&event.action),
        })).collect::<Vec<_>>()})
    };
    let (method, path, body, auth_required) = match operation {
        "beginPair" => (
            VizioHttpMethod::Put,
            "/pairing/start".into(),
            Some(json!({
                "DEVICE_ID": device_id,
                "DEVICE_NAME": device_name,
            })),
            false,
        ),
        "finishPair" => {
            let challenge_type = required_u64(fields, "challengeType")?;
            let token = required_u64(fields, "token")?;
            let pin = required_string(fields, "pin")?;
            if challenge_type > u16::MAX.into()
                || token > u32::MAX.into()
                || pin.len() > 32
                || pin.is_empty()
                || pin.chars().any(char::is_control)
            {
                return Err(failure(
                    VizioFailureKind::InvalidParameter,
                    "Invalid pairing PIN",
                    false,
                ));
            }
            (
                VizioHttpMethod::Put,
                "/pairing/pair".into(),
                Some(json!({
                    "DEVICE_ID": device_id,
                    "CHALLENGE_TYPE": challenge_type,
                    "RESPONSE_VALUE": pin,
                    "PAIRING_REQ_TOKEN": token,
                })),
                false,
            )
        }
        "cancelPair" => (
            VizioHttpMethod::Put,
            "/pairing/cancel".into(),
            Some(json!({
                "DEVICE_ID": device_id,
                "DEVICE_NAME": device_name,
                "CHALLENGE_TYPE": 1,
                "RESPONSE_VALUE": "1111",
                "PAIRING_REQ_TOKEN": 0,
            })),
            false,
        ),
        "ping" | "deviceInfo" => (
            VizioHttpMethod::Get,
            "/state/device/deviceinfo".into(),
            None,
            false,
        ),
        "pingAuth" | "powerState" => (
            VizioHttpMethod::Get,
            "/state/device/power_mode".into(),
            None,
            true,
        ),
        "remote" => {
            let events = serde_json::from_value::<Vec<VizioRemoteEvent>>(
                fields.get("events").cloned().unwrap_or(Value::Null),
            )
            .map_err(|_| {
                failure(
                    VizioFailureKind::InvalidInput,
                    "Invalid remote event list",
                    false,
                )
            })?;
            if events.is_empty() {
                return Err(failure(
                    VizioFailureKind::InvalidInput,
                    "Remote event list is empty",
                    false,
                ));
            }
            if events.len() > 256 {
                return Err(failure(
                    VizioFailureKind::InvalidParameter,
                    "Remote event list is too large",
                    false,
                ));
            }
            (
                VizioHttpMethod::Put,
                "/key_command/".into(),
                Some(key_events(events)),
                true,
            )
        }
        "key" => {
            let key = serde_json::from_value::<VizioRemoteKey>(
                fields.get("key").cloned().unwrap_or(Value::Null),
            )
            .map_err(|_| {
                failure(
                    VizioFailureKind::InvalidInput,
                    "Unknown SmartCast remote key",
                    false,
                )
            })?;
            let action = fields
                .get("action")
                .cloned()
                .map(serde_json::from_value::<VizioRemoteAction>)
                .transpose()
                .map_err(|_| {
                    failure(
                        VizioFailureKind::InvalidInput,
                        "Invalid remote action",
                        false,
                    )
                })?
                .unwrap_or(VizioRemoteAction::Keypress);
            (
                VizioHttpMethod::Put,
                "/key_command/".into(),
                Some(key_events(vec![key.event(action)])),
                true,
            )
        }
        "text" => {
            let text = required_string(fields, "text")?;
            if !text.is_ascii() || text.len() > 512 {
                return Err(failure(
                    VizioFailureKind::InvalidParameter,
                    "SmartCast text entry supports at most 512 ASCII characters",
                    false,
                ));
            }
            let events = text
                .bytes()
                .map(|code| VizioRemoteEvent {
                    code_set: 0,
                    code: u16::from(code),
                    action: VizioRemoteAction::Keypress,
                })
                .collect();
            (
                VizioHttpMethod::Put,
                "/key_command/".into(),
                Some(key_events(events)),
                true,
            )
        }
        "powerOn" => key_request(VizioRemoteKey::PowOn, key_events),
        "powerOff" => key_request(VizioRemoteKey::PowOff, key_events),
        "powerToggle" => key_request(VizioRemoteKey::PowToggle, key_events),
        "volumeUp" => repeated_key(VizioRemoteKey::VolUp, fields, key_events)?,
        "volumeDown" => repeated_key(VizioRemoteKey::VolDown, fields, key_events)?,
        "mute" => key_request(VizioRemoteKey::MuteOn, key_events),
        "unmute" => key_request(VizioRemoteKey::MuteOff, key_events),
        "muteToggle" => key_request(VizioRemoteKey::MuteToggle, key_events),
        "nextInput" => key_request(VizioRemoteKey::InputNext, key_events),
        "channelUp" => key_request(VizioRemoteKey::ChUp, key_events),
        "channelDown" => key_request(VizioRemoteKey::ChDown, key_events),
        "previousChannel" => key_request(VizioRemoteKey::ChPrev, key_events),
        "getSettings" => {
            let category = setting_path(required_string(fields, "category")?)?;
            (
                VizioHttpMethod::Get,
                format!("{SETTINGS_ROOT}/{category}"),
                None,
                true,
            )
        }
        "getSetting" | "getVolume" | "isMuted" => {
            let (category, name) = if operation == "getVolume" {
                ("audio", "volume")
            } else if operation == "isMuted" {
                ("audio", "mute")
            } else {
                (
                    required_string(fields, "category")?,
                    required_string(fields, "name")?,
                )
            };
            (
                VizioHttpMethod::Get,
                format!(
                    "{SETTINGS_ROOT}/{}/{}",
                    setting_path(category)?,
                    setting_path(name)?
                ),
                None,
                true,
            )
        }
        "setSetting" => {
            let category = setting_path(required_string(fields, "category")?)?;
            let name = setting_path(required_string(fields, "name")?)?;
            let value = fields.get("value").cloned().ok_or_else(|| {
                failure(
                    VizioFailureKind::InvalidInput,
                    "Setting value is required",
                    false,
                )
            })?;
            validate_setting_value(&value, fields)?;
            let hash = required_i64(fields, "hashValue")?;
            (
                VizioHttpMethod::Put,
                format!("{SETTINGS_ROOT}/{category}/{name}"),
                Some(json!({
                    "REQUEST": "MODIFY", "VALUE": value, "HASHVAL": hash,
                })),
                true,
            )
        }
        "triggerSetting" | "blankScreen" => {
            let (category, name) = if operation == "blankScreen" {
                ("system/timers", "blank_screen")
            } else {
                (
                    required_string(fields, "category")?,
                    required_string(fields, "name")?,
                )
            };
            let hash = required_i64(fields, "hashValue")?;
            (
                VizioHttpMethod::Put,
                format!(
                    "{SETTINGS_ROOT}/{}/{}",
                    setting_path(category)?,
                    setting_path(name)?
                ),
                Some(json!({
                    "REQUEST": "ACTION", "HASHVAL": hash,
                })),
                true,
            )
        }
        "setVolume" => {
            let level = required_i64(fields, "level")?;
            if !(0..=100).contains(&level) {
                return Err(failure(
                    VizioFailureKind::InvalidParameter,
                    "TV volume must be between 0 and 100",
                    false,
                ));
            }
            (
                VizioHttpMethod::Put,
                "/audio/volume/level".into(),
                Some(json!({"VALUE": level})),
                true,
            )
        }
        "getInputs" => (
            VizioHttpMethod::Get,
            format!("{SETTINGS_ROOT}/devices/name_input"),
            None,
            true,
        ),
        "getCurrentInput" => (
            VizioHttpMethod::Get,
            format!("{SETTINGS_ROOT}/devices/current_input"),
            None,
            true,
        ),
        "setInput" => {
            let cname = required_string(fields, "cname")?;
            let hash = required_i64(fields, "hashValue")?;
            if cname.is_empty() || cname.len() > 128 {
                return Err(failure(
                    VizioFailureKind::InvalidParameter,
                    "Invalid TV input CNAME",
                    false,
                ));
            }
            (
                VizioHttpMethod::Put,
                format!("{SETTINGS_ROOT}/devices/current_input"),
                Some(json!({
                    "REQUEST": "MODIFY", "VALUE": cname, "HASHVAL": hash,
                })),
                true,
            )
        }
        "launchApp" => {
            let app_id = required_string(fields, "appId")?;
            let name_space = required_u64(fields, "nameSpace")?;
            if app_id.is_empty() || app_id.len() > 128 || name_space > u16::MAX.into() {
                return Err(failure(
                    VizioFailureKind::InvalidParameter,
                    "Invalid SmartCast app configuration",
                    false,
                ));
            }
            let mut value = json!({"APP_ID":app_id,"NAME_SPACE":name_space});
            if let Some(message) = fields.get("message").and_then(Value::as_str) {
                if message.len() > 4096 || message.chars().any(char::is_control) {
                    return Err(failure(
                        VizioFailureKind::InvalidParameter,
                        "Invalid SmartCast app message",
                        false,
                    ));
                }
                value["MESSAGE"] = json!(message);
            }
            (
                VizioHttpMethod::Put,
                "/app/launch".into(),
                Some(json!({"VALUE":value})),
                true,
            )
        }
        "launchConjure" => {
            let url = validate_conjure_url(required_string(fields, "url")?)?;
            let mut value = json!({"APP_ID":"17","NAME_SPACE":4,"MESSAGE":url});
            if fields
                .get("debug")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                value["DEBUG"] = json!(1);
            }
            (
                VizioHttpMethod::Put,
                "/app/launch".into(),
                Some(json!({"VALUE":value})),
                true,
            )
        }
        "currentApp" => (VizioHttpMethod::Get, "/app/current".into(), None, true),
        "stateExtended" => (VizioHttpMethod::Get, "/state_extended".into(), None, true),
        "systemVersions" => (VizioHttpMethod::Get, "/system/versions".into(), None, true),
        "pinDefault" => (
            VizioHttpMethod::Get,
            "/pin/is_pin_default".into(),
            None,
            true,
        ),
        "identityPrimary" | "identityLegacy" => {
            let item = setting_path(required_string(fields, "item")?)?;
            let section = setting_path(required_string(fields, "section")?)?;
            let root = if operation == "identityPrimary" {
                "admin_and_privacy/system_information"
            } else {
                "system/system_information"
            };
            (
                VizioHttpMethod::Get,
                format!("{SETTINGS_ROOT}/{root}/{section}/{item}"),
                None,
                true,
            )
        }
        _ => {
            return Err(failure(
                VizioFailureKind::InvalidInput,
                "Unknown SmartCast operation",
                false,
            ));
        }
    };
    build_request(
        base,
        auth,
        method,
        &path,
        body,
        auth_required,
        RequestLimits {
            timeout_millis,
            max_response_bytes,
        },
    )
}

fn key_request(
    key: VizioRemoteKey,
    key_events: impl FnOnce(Vec<VizioRemoteEvent>) -> Value,
) -> (VizioHttpMethod, String, Option<Value>, bool) {
    (
        VizioHttpMethod::Put,
        "/key_command/".into(),
        Some(key_events(vec![key.event(VizioRemoteAction::Keypress)])),
        true,
    )
}

fn repeated_key(
    key: VizioRemoteKey,
    fields: &Map<String, Value>,
    key_events: impl FnOnce(Vec<VizioRemoteEvent>) -> Value,
) -> Result<(VizioHttpMethod, String, Option<Value>, bool), VizioFailure> {
    let steps = fields
        .get("steps")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1);
    if steps > 100 {
        return Err(failure(
            VizioFailureKind::InvalidParameter,
            "Remote repeat is too large",
            false,
        ));
    }
    Ok((
        VizioHttpMethod::Put,
        "/key_command/".into(),
        Some(key_events(
            (0..steps)
                .map(|_| key.event(VizioRemoteAction::Keypress))
                .collect(),
        )),
        true,
    ))
}
fn action_name(action: &VizioRemoteAction) -> &'static str {
    match action {
        VizioRemoteAction::Keypress => "KEYPRESS",
        VizioRemoteAction::Keydown => "KEYDOWN",
        VizioRemoteAction::Keyup => "KEYUP",
    }
}
