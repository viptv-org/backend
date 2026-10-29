pub(super) use viptv_provider::normalize::*;

pub(super) fn db_error(_: rusqlite::Error) -> String {
    "Provider database operation failed".into()
}
