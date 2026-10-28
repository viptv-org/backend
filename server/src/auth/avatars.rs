use super::*;
use viptv_engine::profile::{is_catalog_avatar, DEFAULT_AVATAR_STYLE};

pub(crate) fn avatar_style(value: Option<&str>) -> Result<&str, ApiError> {
    let style = value.unwrap_or("critters");
    if AVATAR_STYLES.contains(&style) {
        Ok(style)
    } else {
        Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Unsupported avatar style".into(),
        ))
    }
}
pub(crate) fn character_avatars() -> &'static Value {
    static CATALOG: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../../assets/character-avatars.json"))
            .expect("bundled character catalog")
    })
}
fn avatar_url(style: &str, seed: &str) -> String {
    if let Some(items) = character_avatars().get(style).and_then(Value::as_array) {
        let choice = avatar_choice(style, seed).unwrap_or(1);
        return items.get(choice as usize - 1).unwrap_or(&items[0])["url"]
            .as_str()
            .unwrap_or_default()
            .to_string();
    }
    format!("https://api.dicebear.com/10.x/{style}/png?seed={seed}&size=256")
}
/// Whether `choice` is a position of the catalog world `style`. The bound is
/// the engine's per-world size (`viptv_engine::profile::AVATAR_WORLDS`), not
/// one span for every style.
pub(crate) fn is_catalog_choice(style: &str, choice: u64) -> bool {
    u16::try_from(choice).is_ok_and(|choice| is_catalog_avatar(style, choice))
}
/// The seed a write stores for `style`. A choice the payload sends must be a
/// position of that world; without one the stored seed keeps its choice, and a
/// bundled character world with no usable choice shows its first character.
pub(crate) fn selected_avatar_seed(
    value: &Value,
    style: &str,
    fallback: String,
) -> Result<String, ApiError> {
    if let Some(choice) = value.get("avatar_choice").and_then(Value::as_u64) {
        if !is_catalog_choice(style, choice) {
            return Err("Invalid avatar_choice".into());
        }
        return Ok(format!("viptv-{style}-{choice}"));
    }
    let stored = fallback
        .strip_prefix(&format!("viptv-{style}-"))
        .and_then(|id| id.parse::<u64>().ok());
    match stored {
        Some(choice) if is_catalog_choice(style, choice) => Ok(format!("viptv-{style}-{choice}")),
        // A stored choice outside the world is no catalog choice (direct DB
        // edits, imports or older data): the read side projects it as none, so
        // a write that picks no avatar leaves it alone instead of failing.
        Some(_) => Ok(fallback),
        // No catalog choice at all: a bundled world shows its first character,
        // a generated world keeps the random seed that makes its avatar.
        None if character_avatars().get(style).and_then(Value::as_array).is_some() => {
            Ok(format!("viptv-{style}-1"))
        }
        None => Ok(fallback),
    }
}
/// The 1-based catalog position encoded in a `viptv-{style}-{choice}` seed;
/// `None` for a random seed, or for a position the world never had, which the
/// engine's `is_catalog_avatar` rejects.
pub(crate) fn avatar_choice(style: &str, seed: &str) -> Option<u64> {
    seed.strip_prefix(&format!("viptv-{style}-"))
        .and_then(|id| id.parse::<u64>().ok())
        .filter(|id| is_catalog_choice(style, *id))
}
pub(crate) fn profile_json(id: i64, name: &str, style: &str, seed: &str, complete: bool) -> Value {
    json!({
        "id":id.to_string(),
        "name":name,
        "avatar_style":style,
        "avatar_choice":avatar_choice(style,seed),
        "avatar_url":avatar_url(style,seed),
        "setup_complete":complete
    })
}
/// Validates a profile write. `current_style` is the world a choice without a
/// payload `avatar_style` is measured in: the profile's stored world, or `None`
/// when the profile is being created (the payload's world or the default one).
pub(crate) fn validate_profile_payload(
    value: &Value,
    update: bool,
    current_style: Option<&str>,
) -> Result<(), ApiError> {
    let object = value.as_object().ok_or("Invalid profile")?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "name" | "avatar_style" | "avatar_choice" | "setup_complete"
        )
    }) {
        return Err("Unknown profile field".into());
    }
    if update && object.is_empty() {
        return Err("Empty profile update".into());
    }
    if !update && !object.contains_key("name") {
        return Err("Missing name".into());
    }
    if !update && object.contains_key("setup_complete") {
        return Err("setup_complete is only valid when updating an imported profile".into());
    }
    if let Some(name) = object.get("name") {
        let name = name.as_str().ok_or("Invalid name")?;
        if name.trim().is_empty() || name.len() > 80 {
            return Err("Invalid name".into());
        }
    }
    let style = match object.get("avatar_style") {
        Some(style) => avatar_style(Some(style.as_str().ok_or("Invalid avatar_style")?)),
        None => Ok(current_style.unwrap_or(DEFAULT_AVATAR_STYLE)),
    }?;
    if let Some(choice) = object.get("avatar_choice") {
        if !choice.as_u64().is_some_and(|id| is_catalog_choice(style, id)) {
            return Err("Invalid avatar_choice".into());
        }
    }
    if let Some(complete) = object.get("setup_complete") {
        if complete != &Value::Bool(true) {
            return Err("Invalid setup_complete".into());
        }
    }
    Ok(())
}
pub(crate) fn require_household_manager(principal: &Principal) -> Result<(), ApiError> {
    if matches!(principal, Principal::Account {role,..} if role == "device") {
        return Err(forbidden());
    }
    Ok(())
}
pub(crate) fn primary_profile(db: &Connection, account: i64) -> Result<Option<i64>, ApiError> {
    db.query_row(
        "SELECT min(profile_id) FROM profile_owners WHERE account_id=?1",
        [account],
        |r| r.get(0),
    )
    .map_err(crate::db_error)
}
