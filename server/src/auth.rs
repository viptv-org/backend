//! Account authentication, browser sessions and explicit device authorization.
//! All bearer credentials are random; only SHA-256 digests are persisted.
use super::*;
mod actions;
mod avatars;
mod device_actions;
mod device_pairing;
mod handlers;
mod middleware;
mod principal;
mod profiles_actions;
mod schema;
#[cfg(test)]
mod tests;
mod tokens;

pub use middleware::{authenticate, router, router_with_auth};
pub use principal::Principal;
pub use schema::init;
pub use tokens::create_owner_offline;

pub(crate) use actions::{action, dispatch_prepared};
pub(crate) use avatars::{
    avatar_style, primary_profile, profile_json, require_household_manager, selected_avatar_seed,
    validate_profile_payload,
};
pub(crate) use device_actions::dispatch_devices;
pub(crate) use device_pairing::device_qr;
pub(crate) use handlers::{info, remove, status};
pub(crate) use middleware::{
    auth_error, bearer, canonical_origin, constant_eq, cookie, csrf_for_session, forbidden,
    issue_csrf, origin, required_origins, unauthorized,
};
pub(crate) use profiles_actions::{
    create_profile, dispatch_profiles, list_profiles, update_profile,
};
pub(crate) use schema::{
    hash, now, token, ACCESS, AVATAR_STYLES, DEVICE_EXPIRY, MAX_APPROVED_PAIRINGS_PER_ACCOUNT,
    MAX_AUTH_BUCKETS, MAX_MEMBER_SESSIONS, MAX_PAIRINGS, MAX_PENDING_PAIRINGS, MAX_PROFILES,
    MAX_REFRESH_TOMBSTONES, MAX_REFRESH_TOMBSTONES_PER_ACCOUNT, MAX_REFRESH_TOMBSTONES_PER_FAMILY,
    MAX_SESSIONS, MAX_SESSIONS_PER_ACCOUNT, REFRESH,
};
pub(crate) use tokens::{
    acquire_auth_cpu, canonical, endpoint_global_limit, event, field, identifier, login_snapshot,
    pairing_code, prepare_auth, rate, response, session, username, PreparedAuth, AUTH_CPU,
};

#[cfg(test)]
pub(crate) use actions::dispatch;
#[cfg(test)]
pub(crate) use avatars::character_avatars;
#[cfg(test)]
use axum::http::{HeaderMap, HeaderValue, Method};
#[cfg(test)]
pub(crate) use middleware::{check_origin, parse_origin, public};
