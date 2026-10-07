use super::failure;
use super::operations::request_for;
use super::response::value_object;
use super::types::{
    VizioFailure, VizioFailureKind, VizioHttpMethod, VizioRequest, VizioRequestResult,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RequestInput {
    pub(super) host: String,
    pub(super) auth_token: Option<String>,
    pub(super) device_id: Option<String>,
    pub(super) device_name: Option<String>,
    pub(super) timeout_millis: Option<u64>,
    pub(super) max_response_bytes: Option<u64>,
    #[serde(flatten)]
    pub(super) fields: Map<String, Value>,
}

pub(super) struct RequestLimits {
    pub(super) timeout_millis: u64,
    pub(super) max_response_bytes: u64,
}

pub fn plan_request(operation: &str, input: &str) -> VizioRequestResult {
    let result = serde_json::from_str::<RequestInput>(input)
        .map_err(|_| {
            failure(
                VizioFailureKind::InvalidInput,
                "Invalid SmartCast command",
                false,
            )
        })
        .and_then(|input| request_for(operation, input));
    match result {
        Ok(request) => VizioRequestResult::Ok(request),
        Err(error) => VizioRequestResult::Err(error),
    }
}
pub(super) fn command_fields(input: &str) -> Result<Map<String, Value>, VizioFailure> {
    if input.len() > 64 * 1024 {
        return Err(failure(
            VizioFailureKind::InvalidInput,
            "SmartCast command is too large",
            false,
        ));
    }
    let value = if input.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str::<Value>(input).map_err(|_| {
            failure(
                VizioFailureKind::InvalidInput,
                "Invalid SmartCast command",
                false,
            )
        })?
    };
    let fields = value.as_object().cloned().ok_or_else(|| {
        failure(
            VizioFailureKind::InvalidInput,
            "SmartCast command must be an object",
            false,
        )
    })?;
    if fields.keys().any(|key| {
        [
            "host",
            "authToken",
            "deviceId",
            "deviceName",
            "timeoutMillis",
            "maxResponseBytes",
            "hashValue",
            "cname",
        ]
        .contains(&key.as_str())
    }) {
        return Err(failure(
            VizioFailureKind::InvalidInput,
            "SmartCast command contains controller-owned data",
            false,
        ));
    }
    Ok(fields)
}
pub(super) fn is_setting_value(value: &Value) -> bool {
    value.is_string() || value.is_boolean() || value.is_number()
}

pub(super) fn setting_names(fields: &Map<String, Value>) -> Result<(String, String), VizioFailure> {
    let category = setting_path(required_string(fields, "category")?)?.to_owned();
    let name = setting_path(required_string(fields, "name")?)?.to_owned();
    Ok((category, name))
}

pub(super) fn setting_fields(category: &str, name: &str) -> Map<String, Value> {
    Map::from_iter([
        ("category".into(), Value::String(category.to_owned())),
        ("name".into(), Value::String(name.to_owned())),
    ])
}

pub(super) fn validate_identity(value: Option<&str>, label: &str) -> Result<(), VizioFailure> {
    if value.is_some_and(|value| {
        value.is_empty() || value.len() > 128 || value.chars().any(char::is_control)
    }) {
        return Err(failure(
            VizioFailureKind::InvalidConfig,
            &format!("Invalid SmartCast {label}"),
            false,
        ));
    }
    Ok(())
}

pub(super) fn validate_limits(
    timeout: Option<u64>,
    response_bytes: Option<u64>,
) -> Result<(), VizioFailure> {
    let timeout = timeout.unwrap_or(10_000);
    let response_bytes = response_bytes.unwrap_or(2 * 1024 * 1024);
    if !(250..=60_000).contains(&timeout) || response_bytes == 0 || response_bytes > 8 * 1024 * 1024
    {
        return Err(failure(
            VizioFailureKind::InvalidConfig,
            "Invalid SmartCast request limits",
            false,
        ));
    }
    Ok(())
}
pub(super) fn build_request(
    base: String,
    auth: Option<&str>,
    method: VizioHttpMethod,
    path: &str,
    body: Option<Value>,
    auth_required: bool,
    limits: RequestLimits,
) -> Result<VizioRequest, VizioFailure> {
    if auth_required && auth.is_none() {
        return Err(failure(
            VizioFailureKind::Authentication,
            "This SmartCast endpoint requires pairing",
            false,
        ));
    }
    let mut headers = BTreeMap::from([
        ("Accept".into(), "application/json".into()),
        ("Content-Type".into(), "application/json".into()),
    ]);
    if auth_required {
        headers.insert("AUTH".into(), auth.unwrap_or_default().into());
    }
    let body = body.map(value_object).transpose()?;
    Ok(VizioRequest {
        method,
        url: format!(
            "{base}{}",
            if path.starts_with('/') {
                path.into()
            } else {
                format!("/{path}")
            }
        ),
        headers,
        body,
        timeout_millis: limits.timeout_millis,
        max_response_bytes: limits.max_response_bytes,
    })
}

pub(super) fn normalize_host(host: &str) -> Result<String, VizioFailure> {
    let host = host.trim().trim_end_matches('/');
    if host.is_empty() {
        return Err(failure(
            VizioFailureKind::InvalidConfig,
            "TV host is required",
            false,
        ));
    }
    if host.to_ascii_lowercase().starts_with("http://") {
        return Err(failure(
            VizioFailureKind::InvalidConfig,
            "SmartCast TV connections require HTTPS",
            false,
        ));
    }
    let without_scheme = host
        .strip_prefix("https://")
        .or_else(|| host.strip_prefix("HTTPS://"))
        .unwrap_or(host);
    let authority = without_scheme.split('/').next().unwrap_or_default();
    let has_explicit_port = if authority.starts_with('[') {
        authority
            .find(']')
            .is_some_and(|end| authority.as_bytes().get(end + 1) == Some(&b':'))
    } else {
        authority.rsplit_once(':').is_some_and(|(_, port)| {
            !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
        })
    };
    let mut url = url::Url::parse(&format!("https://{without_scheme}"))
        .map_err(|_| failure(VizioFailureKind::InvalidConfig, "Invalid TV host", false))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || url.host_str().is_none()
    {
        return Err(failure(
            VizioFailureKind::InvalidConfig,
            "Invalid TV host",
            false,
        ));
    }
    if !has_explicit_port {
        url.set_port(Some(7345))
            .map_err(|_| failure(VizioFailureKind::InvalidConfig, "Invalid TV host", false))?;
    }
    Ok(url.origin().ascii_serialization())
}

pub(super) fn validate_conjure_url(input: &str) -> Result<String, VizioFailure> {
    let url = url::Url::parse(input).map_err(|_| {
        failure(
            VizioFailureKind::InvalidParameter,
            "Conjure apps require an HTTP or HTTPS URL reachable by the TV",
            false,
        )
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(failure(
            VizioFailureKind::InvalidParameter,
            "Conjure apps require an HTTP or HTTPS URL reachable by the TV",
            false,
        ));
    }
    Ok(url.to_string())
}
pub(super) fn setting_path(value: &str) -> Result<&str, VizioFailure> {
    if value.is_empty()
        || value.len() > 256
        || value.starts_with('/')
        || value.ends_with('/')
        || value.split('/').any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
    {
        return Err(failure(
            VizioFailureKind::InvalidParameter,
            "Invalid SmartCast setting path",
            false,
        ));
    }
    Ok(value)
}

pub(super) fn required_string<'a>(
    fields: &'a Map<String, Value>,
    name: &str,
) -> Result<&'a str, VizioFailure> {
    fields.get(name).and_then(Value::as_str).ok_or_else(|| {
        failure(
            VizioFailureKind::InvalidInput,
            "SmartCast command is missing required data",
            false,
        )
    })
}
pub(super) fn required_u64(fields: &Map<String, Value>, name: &str) -> Result<u64, VizioFailure> {
    fields.get(name).and_then(Value::as_u64).ok_or_else(|| {
        failure(
            VizioFailureKind::InvalidInput,
            "SmartCast command is missing required data",
            false,
        )
    })
}
pub(super) fn required_i64(fields: &Map<String, Value>, name: &str) -> Result<i64, VizioFailure> {
    fields.get(name).and_then(Value::as_i64).ok_or_else(|| {
        failure(
            VizioFailureKind::InvalidInput,
            "SmartCast command is missing required data",
            false,
        )
    })
}
