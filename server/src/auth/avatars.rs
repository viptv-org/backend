use super::*;

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
        let choice = seed
            .strip_prefix(&format!("viptv-{style}-"))
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1);
        return items.get(choice.saturating_sub(1)).unwrap_or(&items[0])["url"]
            .as_str()
            .unwrap_or_default()
            .to_string();
    }
    format!("https://api.dicebear.com/10.x/{style}/png?seed={seed}&size=256")
}
pub(crate) fn selected_avatar_seed(
    value: &Value,
    style: &str,
    fallback: String,
) -> Result<String, ApiError> {
    if let Some(items) = character_avatars().get(style).and_then(Value::as_array) {
        let choice = value
            .get("avatar_choice")
            .and_then(Value::as_u64)
            .or_else(|| {
                fallback
                    .strip_prefix(&format!("viptv-{style}-"))
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(1);
        if choice == 0 || choice as usize > items.len() {
            return Err("Invalid avatar_choice".into());
        }
        return Ok(format!("viptv-{style}-{choice}"));
    }
    Ok(value
        .get("avatar_choice")
        .and_then(Value::as_u64)
        .map_or(fallback, |choice| format!("viptv-{style}-{choice}")))
}
pub(crate) fn profile_json(id: i64, name: &str, style: &str, seed: &str, complete: bool) -> Value {
    json!({
        "id":id.to_string(),
        "name":name,
        "avatar_style":style,
        "avatar_choice":seed.strip_prefix(&format!("viptv-{style}-")).and_then(|id| id.parse::<u64>().ok()).filter(|id| (1..=48).contains(id)),
        "avatar_url":avatar_url(style,seed),
        "setup_complete":complete
    })
}
pub(crate) fn validate_profile_payload(value: &Value, update: bool) -> Result<(), ApiError> {
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
    if let Some(style) = object.get("avatar_style") {
        avatar_style(Some(style.as_str().ok_or("Invalid avatar_style")?))?;
    }
    if let Some(choice) = object.get("avatar_choice") {
        if !choice.as_u64().is_some_and(|id| (1..=48).contains(&id)) {
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
