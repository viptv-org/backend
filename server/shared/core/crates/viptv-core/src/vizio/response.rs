use super::failure;
use super::request::is_setting_value;
use super::types::{
    VizioFailure, VizioFailureKind, VizioInputInfo, VizioPairingChallenge, VizioProtocolResponse,
    VizioResponseResult,
};
use crate::dto::{JsonObject, JsonValue};
use serde_json::{Map, Value, json};

pub fn parse_response(status: u16, body: &str, allow_statusless: bool) -> VizioResponseResult {
    let result = parse_protocol_response(status, body, allow_statusless);
    match result {
        Ok(response) => VizioResponseResult::Ok(response),
        Err(error) => VizioResponseResult::Err(error),
    }
}

pub fn pairing_challenge(
    response: &VizioProtocolResponse,
) -> Result<VizioPairingChallenge, VizioFailure> {
    let item = response_item(&response.raw).ok_or_else(|| {
        failure(
            VizioFailureKind::InvalidResponse,
            "Pairing response is missing challenge data",
            false,
        )
    })?;
    let challenge_type = number(item, "CHALLENGE_TYPE").and_then(number_to_u16);
    let token = number(item, "PAIRING_REQ_TOKEN")
        .and_then(number_to_u32)
        .map(u64::from);
    match (challenge_type, token) {
        (Some(challenge_type), Some(token)) => Ok(VizioPairingChallenge {
            challenge_type,
            token,
        }),
        _ => Err(failure(
            VizioFailureKind::InvalidResponse,
            "Pairing response is missing challenge data",
            false,
        )),
    }
}

pub fn pairing_auth_token(response: &VizioProtocolResponse) -> Result<String, VizioFailure> {
    response_item(&response.raw)
        .and_then(|item| string(item, "AUTH_TOKEN"))
        .filter(|token| {
            !token.trim().is_empty() && token.len() <= 1024 && !token.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            failure(
                VizioFailureKind::InvalidResponse,
                "Pairing response is missing AUTH_TOKEN",
                false,
            )
        })
}

pub(super) struct SettingSnapshot {
    pub(super) hash_value: i64,
    pub(super) minimum: Option<f64>,
    pub(super) maximum: Option<f64>,
    pub(super) options: Vec<String>,
}
pub(super) fn current_input_state(
    response: &VizioProtocolResponse,
) -> Result<(String, i64), VizioFailure> {
    let item = find_response_item(response, "current_input").ok_or_else(|| {
        failure(
            VizioFailureKind::InvalidResponse,
            "Current input response is missing data",
            false,
        )
    })?;
    let value = string(item, "VALUE").filter(|value| !value.is_empty());
    let hash_value = number(item, "HASHVAL").and_then(number_to_i64);
    match (value, hash_value) {
        (Some(value), Some(hash_value)) => Ok((value.to_owned(), hash_value)),
        _ => Err(failure(
            VizioFailureKind::InvalidResponse,
            "Current input response is missing VALUE or HASHVAL",
            false,
        )),
    }
}

pub(super) fn parse_setting(
    response: &VizioProtocolResponse,
    name: &str,
) -> Result<SettingSnapshot, VizioFailure> {
    let item = find_response_item(response, name).ok_or_else(|| {
        failure(
            VizioFailureKind::InvalidResponse,
            "Setting response is missing data",
            false,
        )
    })?;
    let hash_value = number(item, "HASHVAL")
        .and_then(number_to_i64)
        .ok_or_else(|| {
            failure(
                VizioFailureKind::InvalidResponse,
                "Setting response is missing HASHVAL",
                false,
            )
        })?;
    let options = item
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("ELEMENTS"))
        .and_then(|(_, value)| match value {
            JsonValue::Array(values) => Some(values),
            _ => None,
        })
        .into_iter()
        .flatten()
        .filter_map(|entry| match entry {
            JsonValue::String(value) => Some(value.clone()),
            JsonValue::Object(value) => string(value, "NAME")
                .or_else(|| string(value, "VALUE"))
                .map(str::to_owned),
            _ => None,
        })
        .collect();
    Ok(SettingSnapshot {
        hash_value,
        minimum: number(item, "MINIMUM"),
        maximum: number(item, "MAXIMUM"),
        options,
    })
}

fn find_response_item<'a>(
    response: &'a VizioProtocolResponse,
    name: &str,
) -> Option<&'a JsonObject> {
    response_item(&response.raw)
        .into_iter()
        .chain(response_items(&response.raw))
        .find(|item| string(item, "CNAME").is_some_and(|value| value.eq_ignore_ascii_case(name)))
}

pub(super) fn validate_setting_snapshot(
    value: &Value,
    setting: &SettingSnapshot,
) -> Result<(), VizioFailure> {
    if !is_setting_value(value) {
        return Err(failure(
            VizioFailureKind::InvalidParameter,
            "Setting value must be a string, number, or boolean",
            false,
        ));
    }
    let mut fields = Map::new();
    if let Some(minimum) = setting.minimum {
        fields.insert("minimum".into(), Value::from(minimum));
    }
    if let Some(maximum) = setting.maximum {
        fields.insert("maximum".into(), Value::from(maximum));
    }
    if !setting.options.is_empty() {
        fields.insert("options".into(), json!(setting.options));
    }
    validate_setting_value(value, &fields)
}

pub(super) fn object_value(value: Value) -> Option<JsonObject> {
    value_object(value).ok()
}

pub fn parse_inputs(response: &VizioProtocolResponse, current: &str) -> Vec<VizioInputInfo> {
    response_items(&response.raw)
        .into_iter()
        .filter_map(|item| {
            let cname = string(item, "CNAME")?.to_owned();
            let name = string(item, "NAME")?.to_owned();
            let metadata = record(item, "VALUE");
            let meta_name = metadata
                .and_then(|value| string(value, "NAME").or_else(|| string(value, "METADATA")))
                .unwrap_or_default()
                .to_owned();
            let is_current = [&cname, &name, &meta_name]
                .iter()
                .any(|value| !value.is_empty() && value.eq_ignore_ascii_case(current));
            Some(VizioInputInfo {
                cname,
                name,
                meta_name,
                current: is_current,
                hash_value: number(item, "HASHVAL").map(|value| value as i64),
            })
        })
        .collect()
}

pub fn match_input<'a>(
    wanted: &str,
    inputs: &'a [VizioInputInfo],
) -> Result<&'a VizioInputInfo, VizioFailure> {
    let wanted = wanted.trim();
    let matches = inputs
        .iter()
        .filter(|candidate| {
            [&candidate.cname, &candidate.name, &candidate.meta_name]
                .iter()
                .any(|value| !value.is_empty() && value.eq_ignore_ascii_case(wanted))
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [single] => Ok(single),
        [] => Err(failure(
            VizioFailureKind::InvalidInput,
            "Unknown TV input",
            false,
        )),
        _ => Err(failure(
            VizioFailureKind::InvalidInput,
            "TV input is ambiguous",
            false,
        )),
    }
}
pub(super) fn parse_protocol_response(
    status: u16,
    body: &str,
    allow_statusless: bool,
) -> Result<VizioProtocolResponse, VizioFailure> {
    if status == 401 || status == 403 {
        return Err(failure(
            VizioFailureKind::Authentication,
            "TV rejected the pairing token",
            false,
        ));
    }
    if !(200..=299).contains(&status) {
        return Err(failure(
            VizioFailureKind::HttpStatus,
            &format!("Vizio request failed with HTTP {status}"),
            status == 408 || status == 429 || status >= 500,
        ));
    }
    if body.len() > 8 * 1024 * 1024 {
        return Err(failure(
            VizioFailureKind::InvalidResponse,
            "TV response exceeded the size limit",
            false,
        ));
    }
    let raw_value = serde_json::from_str::<Value>(body).map_err(|_| {
        failure(
            VizioFailureKind::InvalidResponse,
            "TV returned invalid JSON",
            false,
        )
    })?;
    let raw = raw_value.as_object().ok_or_else(|| {
        failure(
            VizioFailureKind::InvalidResponse,
            "TV returned invalid JSON",
            false,
        )
    })?;
    let status_record = ci_field(raw, "STATUS").and_then(Value::as_object);
    if status_record.is_none() && allow_statusless {
        return Ok(VizioProtocolResponse {
            status: "SUCCESS".into(),
            detail: "Success".into(),
            raw: value_object(raw_value)?,
        });
    }
    let status_record = status_record.ok_or_else(|| {
        failure(
            VizioFailureKind::InvalidResponse,
            "TV response is missing STATUS",
            false,
        )
    })?;
    let reported_result = ci_field(status_record, "RESULT")
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN")
        .to_ascii_uppercase();
    let result = match reported_result.as_str() {
        "SUCCESS"
        | "REQUIRES_PAIRING"
        | "PAIRING_DENIED"
        | "CHALLENGE_INCORRECT"
        | "INVALID_PARAMETER"
        | "HASHVAL_ERROR"
        | "VALUE_OUT_OF_RANGE"
        | "URI_NOT_FOUND"
        | "BLOCKED"
        | "MAX_CHALLENGES_EXCEEDED" => reported_result,
        _ => "UNKNOWN".to_owned(),
    };
    let detail = ci_field(status_record, "DETAIL")
        .and_then(Value::as_str)
        .unwrap_or(&result)
        .to_owned();
    let error_kind = match result.as_str() {
        "SUCCESS" => {
            return Ok(VizioProtocolResponse {
                status: result,
                detail,
                raw: value_object(raw_value)?,
            });
        }
        "REQUIRES_PAIRING" | "PAIRING_DENIED" | "CHALLENGE_INCORRECT" => {
            VizioFailureKind::Authentication
        }
        "INVALID_PARAMETER" | "HASHVAL_ERROR" | "VALUE_OUT_OF_RANGE" => {
            VizioFailureKind::InvalidParameter
        }
        "URI_NOT_FOUND" => VizioFailureKind::EndpointNotFound,
        "BLOCKED" | "MAX_CHALLENGES_EXCEEDED" => VizioFailureKind::Busy,
        _ => VizioFailureKind::InvalidResponse,
    };
    let retryable = error_kind == VizioFailureKind::Busy;
    let message = match error_kind {
        VizioFailureKind::Authentication => "TV rejected SmartCast authentication",
        VizioFailureKind::InvalidParameter => "TV rejected a SmartCast parameter",
        VizioFailureKind::EndpointNotFound => "TV does not support this SmartCast endpoint",
        VizioFailureKind::Busy => "TV is busy with another SmartCast request",
        _ => "TV rejected the SmartCast request",
    };
    let mut error = failure(error_kind, message, retryable);
    error.protocol_status = Some(result);
    Err(error)
}

pub(super) fn validate_setting_value(
    value: &Value,
    fields: &Map<String, Value>,
) -> Result<(), VizioFailure> {
    if let Some(number) = value.as_f64() {
        if fields
            .get("minimum")
            .and_then(Value::as_f64)
            .is_some_and(|minimum| number < minimum)
        {
            return Err(failure(
                VizioFailureKind::InvalidParameter,
                "Setting is below its minimum",
                false,
            ));
        }
        if fields
            .get("maximum")
            .and_then(Value::as_f64)
            .is_some_and(|maximum| number > maximum)
        {
            return Err(failure(
                VizioFailureKind::InvalidParameter,
                "Setting is above its maximum",
                false,
            ));
        }
    }
    if let Some(text) = value.as_str()
        && let Some(options) = fields.get("options").and_then(Value::as_array)
        && !options.is_empty()
        && !options
            .iter()
            .filter_map(Value::as_str)
            .any(|option| option.eq_ignore_ascii_case(text))
    {
        return Err(failure(
            VizioFailureKind::InvalidParameter,
            "Setting value is unsupported",
            false,
        ));
    }
    Ok(())
}

fn ci_field<'a>(value: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    value
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}
fn record<'a>(value: &'a JsonObject, name: &str) -> Option<&'a JsonObject> {
    value
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| match value {
            JsonValue::Object(value) => Some(value),
            _ => None,
        })
}
fn string<'a>(value: &'a JsonObject, name: &str) -> Option<&'a str> {
    value
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| match value {
            JsonValue::String(value) => Some(value.as_str()),
            _ => None,
        })
}
fn number(value: &JsonObject, name: &str) -> Option<f64> {
    value
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| match value {
            JsonValue::Number(value) => Some(*value),
            JsonValue::String(value) => value.parse().ok(),
            _ => None,
        })
}
fn response_item(raw: &JsonObject) -> Option<&JsonObject> {
    record(raw, "ITEM")
}
fn response_items(raw: &JsonObject) -> Vec<&JsonObject> {
    raw.iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("ITEMS"))
        .and_then(|(_, value)| match value {
            JsonValue::Array(values) => Some(values),
            _ => None,
        })
        .into_iter()
        .flatten()
        .filter_map(|value| match value {
            JsonValue::Object(value) => Some(value),
            _ => None,
        })
        .collect()
}
fn number_to_u16(value: f64) -> Option<u16> {
    (value.fract() == 0.0 && value >= 0.0 && value <= u16::MAX as f64).then_some(value as u16)
}
fn number_to_u32(value: f64) -> Option<u32> {
    (value.fract() == 0.0 && value >= 0.0 && value <= u32::MAX as f64).then_some(value as u32)
}
fn number_to_i64(value: f64) -> Option<i64> {
    (value.is_finite()
        && value.fract() == 0.0
        && value >= i64::MIN as f64
        && value <= i64::MAX as f64)
        .then_some(value as i64)
}
pub(super) fn value_object(value: Value) -> Result<JsonObject, VizioFailure> {
    serde_json::from_value(value).map_err(|_| {
        failure(
            VizioFailureKind::InvalidResponse,
            "Invalid SmartCast JSON object",
            false,
        )
    })
}
