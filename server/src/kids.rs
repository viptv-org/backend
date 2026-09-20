//! Household policy: server-observed identities, bounded parent authority, and fail-closed browsing.
use super::*;

mod metadata;
mod pin;
mod policy;
mod schema;

use metadata::{media, prune_observations};
pub(crate) use metadata::{observe, search};
pub(crate) use pin::{set_pin, unlock};
pub(crate) use policy::{after, before, require_item, sql_allowed};
pub(crate) use schema::{
    approvals, approve, get_policy, init, profile_fields, require_parent, restricted, revision,
    set_policy, status, switch_profile,
};
use schema::{forbidden, manager, parent_required, stored_pin};
