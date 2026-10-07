//! Portable Vizio SmartCast protocol behavior.
//!
//! Ported from get-air/vizio at 124b5fb8f2b2b04b3fb9237d4c72b1c4e1a19e3d.
//! Copyright (c) 2026 Air contributors; used under the MIT license recorded in
//! THIRD_PARTY_LICENSES/get-air-vizio.txt. Network/TLS and credential storage
//! remain platform adapters; this module never relaxes certificate validation.

mod controller;
mod operations;
mod platform;
mod request;
mod response;
mod types;

pub use controller::VizioController;
pub use platform::{deviceinfo_name, discovery_candidates, platform_support};
pub use request::plan_request;
pub use response::{
    match_input, pairing_auth_token, pairing_challenge, parse_inputs, parse_response,
};
pub use types::*;

fn failure(kind: VizioFailureKind, message: &str, retryable: bool) -> VizioFailure {
    VizioFailure {
        kind,
        message: message.into(),
        retryable,
        protocol_status: None,
    }
}

#[cfg(test)]
mod tests;
