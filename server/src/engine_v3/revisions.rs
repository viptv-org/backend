//! Cheap per-profile change signals for client freshness checks.
//!
//! `GET /api/profiles/:id/revisions` and bootstrap `revisions` return opaque
//! strings read from a handful of single-row SQLite lookups. No add-on,
//! Simkl, metadata or Home work happens here.
use super::*;

/// The add-on revision a restricted kids session observes; it matches the
/// constant `GET /api/catalogs/revision` answers for such a session.
const KIDS_ADDON_REVISION: &str = "00000000000000000000000000000000";

/// Account-scoped profile-management revision. Triggers bump it whenever a
/// value projected into the account's profile list changes: profile
/// creation/deletion (ownership rows), name/presentation edits, avatar,
/// kids `enabled`/`max_age`, and the account parent PIN.
pub(crate) fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS engine_account_profile_revisions(account_id INTEGER PRIMARY KEY REFERENCES auth_accounts(id) ON DELETE CASCADE,revision INTEGER NOT NULL);")?;
    // Selecting through auth_accounts skips accounts that are being deleted,
    // so a cascading account delete never inserts an orphaned revision row.
    let account = |expr: &str| {
        format!("INSERT INTO engine_account_profile_revisions(account_id,revision) SELECT id,1 FROM auth_accounts WHERE id={expr} ON CONFLICT(account_id) DO UPDATE SET revision=revision+1;")
    };
    let owner = |expr: &str| {
        account(&format!(
            "(SELECT account_id FROM profile_owners WHERE profile_id={expr})"
        ))
    };
    let triggers = [
        (
            "owner_insert",
            "AFTER INSERT ON profile_owners",
            "",
            account("NEW.account_id"),
        ),
        (
            "owner_delete",
            "AFTER DELETE ON profile_owners",
            "",
            account("OLD.account_id"),
        ),
        (
            "owner_update",
            "AFTER UPDATE ON profile_owners",
            "",
            account("OLD.account_id") + &account("NEW.account_id"),
        ),
        (
            "profile_update",
            "AFTER UPDATE ON profiles",
            "",
            owner("NEW.id"),
        ),
        (
            "presentation_insert",
            "AFTER INSERT ON engine_profile_presentation",
            "",
            owner("NEW.profile_id"),
        ),
        (
            "presentation_update",
            "AFTER UPDATE ON engine_profile_presentation",
            "",
            owner("NEW.profile_id"),
        ),
        (
            "presentation_delete",
            "AFTER DELETE ON engine_profile_presentation",
            "",
            owner("OLD.profile_id"),
        ),
        // Approval-only kids writes bump just the profile's policy revision.
        (
            "kids_insert",
            "AFTER INSERT ON kids_profiles",
            "WHEN NEW.enabled<>0 OR NEW.max_age<>12",
            owner("NEW.profile_id"),
        ),
        (
            "kids_update",
            "AFTER UPDATE ON kids_profiles",
            "WHEN OLD.enabled IS NOT NEW.enabled OR OLD.max_age IS NOT NEW.max_age",
            owner("NEW.profile_id"),
        ),
        (
            "kids_delete",
            "AFTER DELETE ON kids_profiles",
            "WHEN OLD.enabled<>0 OR OLD.max_age<>12",
            owner("OLD.profile_id"),
        ),
        (
            "parent_insert",
            "AFTER INSERT ON parent_controls",
            "",
            account("NEW.account_id"),
        ),
        (
            "parent_update",
            "AFTER UPDATE OF pin_hash ON parent_controls",
            "WHEN OLD.pin_hash IS NOT NEW.pin_hash",
            account("NEW.account_id"),
        ),
        (
            "parent_delete",
            "AFTER DELETE ON parent_controls",
            "",
            account("OLD.account_id"),
        ),
    ];
    for (name, event, when, body) in triggers {
        db.execute_batch(&format!(
            "CREATE TRIGGER IF NOT EXISTS engine_profiles_revision_{name} {event} {when} BEGIN {body} END;"
        ))?;
    }
    Ok(())
}

fn scalar(db: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<i64, ApiError> {
    db.query_row(sql, params, |r| r.get(0))
        .optional()
        .map_err(db_error)
        .map(|v| v.unwrap_or(0))
}

/// Reads the selected profile's tokens. Callers have already checked that
/// `profile` is the session's selected profile.
pub(crate) fn read(db: &Connection, app: &App, profile: i64) -> Result<Value, ApiError> {
    let principal = app.identity();
    let account = principal.account_id().ok_or_else(auth::unauthorized)?;
    let library = history::revision(db, profile)?;
    let configuration = scalar(
        db,
        "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
        [profile],
    )?;
    let preferences = scalar(
        db,
        "SELECT revision FROM engine_profile_preferences WHERE profile_id=?1 AND account_id=?2",
        params![profile, account],
    )?;
    let policy = scalar(
        db,
        "SELECT revision FROM kids_profiles WHERE profile_id=?1",
        [profile],
    )?;
    let profiles = scalar(
        db,
        "SELECT revision FROM engine_account_profile_revisions WHERE account_id=?1",
        [account],
    )?;
    let addons = if kids::restricted(db, &principal)? {
        KIDS_ADDON_REVISION.to_owned()
    } else {
        app.addons.revision_db(db)?
    };
    // A parent grant is per session and expires without a write; it changes
    // this session's profile locks, so it is part of the profiles token.
    let unlocked = u8::from(kids::unlocked(db, &principal)?);
    Ok(json!({
        "library": library.to_string(),
        "configuration": configuration.to_string(),
        "addons": addons,
        "policy": policy.to_string(),
        "preferences": preferences.to_string(),
        "profiles": format!("{profiles}.{unlocked}"),
    }))
}

/// Reads the tokens for `profile` after the usual selected-profile check.
pub(crate) fn snapshot(app: &App, profile: i64) -> Result<Value, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    read(&db, app, profile)
}

pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let app = app.with_lease(lease);
    blocking(move || snapshot(&app, profile).map(axum::Json)).await
}
