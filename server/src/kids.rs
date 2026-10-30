use super::*;

pub(crate) mod blocked;
pub(crate) mod consumers;
pub(crate) mod limits;
pub(crate) mod parent_scope;
mod metadata;
mod pin;
mod policy;
pub(crate) mod profile_pin;
mod schema;
pub(crate) mod scoped;
pub(crate) mod titles;
#[cfg(test)]
mod tests;
pub(crate) fn adult(meta: &Value) -> bool {
    metadata::assessment(meta).0.is_some_and(|age| age >= 18)
}

use metadata::{media, prune_observations};
pub(crate) use metadata::observe;
pub(crate) use pin::{set_pin, unlock, PIN_CPU_PERMITS};
pub(crate) use policy::{after, before, filter_items, require_item, sql_allowed};
pub(crate) use schema::{
    get_policy, init, profile_fields, require_parent, restricted, revision,
    set_policy, status, switch_profile, unlocked,
};
use schema::{forbidden, manager, parent_required, stored_pin};
