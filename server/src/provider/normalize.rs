// The pure normalization, URL, and validation primitives now live in the shared
// `viptv-provider` crate so the core's fat clients apply identical rules.
pub(super) use viptv_provider::normalize::*;

pub(super) fn db_error(_: rusqlite::Error) -> String {
    "Provider database operation failed".into()
}
