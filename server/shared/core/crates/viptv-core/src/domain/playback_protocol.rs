//! Closed negotiation only; support does not confer native admission or authority.
use crate::{CoreError, dto::PlaybackProtocol};
use serde::Deserialize;
use serde_json::value::RawValue;

const MAX_PROTOCOL_BYTES: usize = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtocolWire<'a> {
    #[serde(borrow)]
    version: &'a RawValue,
    #[serde(borrow)]
    native_torrent_versions: Vec<&'a RawValue>,
}

/// Parse the original text: Value would discard duplicate fields and numeric spelling.
pub(crate) fn parse(input: &str) -> Result<PlaybackProtocol, CoreError> {
    parse_version(input, 1)
}

pub(crate) fn parse_runtime(input: &str) -> Result<PlaybackProtocol, CoreError> {
    parse_version(input, 2)
}

fn parse_version(input: &str, version: u32) -> Result<PlaybackProtocol, CoreError> {
    if input.len() > MAX_PROTOCOL_BYTES {
        return Err(CoreError::InvalidInput);
    }
    let wire: ProtocolWire<'_> =
        serde_json::from_str(input).map_err(|_| CoreError::InvalidInput)?;
    let token = if version == 1 { "1" } else { "2" };
    if wire.version.get() != token
        || wire.native_torrent_versions.len() > 1
        || wire
            .native_torrent_versions
            .iter()
            .any(|value| value.get() != token)
    {
        return Err(CoreError::InvalidInput);
    }
    Ok(PlaybackProtocol {
        version,
        native_torrent_versions: wire
            .native_torrent_versions
            .iter()
            .map(|_| version)
            .collect(),
    })
}

#[derive(Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
enum ProtocolRequest {
    #[serde(rename = "playbackProtocolV2")]
    Protocol {},
    #[serde(rename = "torrentRuntimeProtocol")]
    RuntimeProtocol {},
    #[serde(rename = "playbackV2CancelRequest")]
    Cancel {
        #[serde(rename = "requestId")]
        request_id: String,
    },
    #[serde(rename = "playbackV2Status")]
    Poll { id: String },
    #[serde(rename = "playbackV2Heartbeat")]
    Heartbeat { id: String },
    #[serde(rename = "playbackV2FirstFrame")]
    FirstFrame { id: String },
    #[serde(rename = "playbackV2Stop")]
    Release { id: String },
}

/// Keep the added bodyless bridge inputs closed without tightening legacy inputs.
pub(crate) fn validate_request(input: &str) -> Result<(), CoreError> {
    match serde_json::from_str::<ProtocolRequest>(input).map_err(|_| CoreError::InvalidInput)? {
        ProtocolRequest::Protocol {} | ProtocolRequest::RuntimeProtocol {} => Ok(()),
        ProtocolRequest::Cancel { request_id } if valid_identifier(&request_id) => Ok(()),
        ProtocolRequest::Poll { id }
        | ProtocolRequest::Heartbeat { id }
        | ProtocolRequest::FirstFrame { id }
        | ProtocolRequest::Release { id }
            if valid_identifier(&id) =>
        {
            Ok(())
        }
        _ => Err(CoreError::InvalidInput),
    }
}

pub(crate) fn valid_identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
