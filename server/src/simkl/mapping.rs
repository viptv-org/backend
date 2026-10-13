use super::*;
use viptv_engine::{identity::MediaIdentity, presentation::Card};

fn unavailable_mapping() -> ApiError {
    ApiError(
        StatusCode::UNPROCESSABLE_ENTITY,
        "Verified source mapping is unavailable for this title or episode".into(),
    )
}

pub(crate) struct Resolved {
    pub target: MediaIdentity,
    parent: Card,
}

pub(crate) async fn resolve_id(
    app: &App,
    kind: &str,
    id: &str,
) -> Result<Option<Resolved>, ApiError> {
    if !id.starts_with("simkl:") {
        return Ok(None);
    }
    let parts: Vec<_> = id.split(':').collect();
    if !matches!(parts.len(), 3 | 5) {
        return Err(unavailable_mapping());
    }
    let category = parts[1];
    let title = parts[2].parse::<u64>().map_err(|_| unavailable_mapping())?;
    let auth::Principal::Account { profile_id, .. } = app.identity();
    let profile = profile_id.ok_or_else(auth::forbidden)?;
    let access = store::credential(app, profile, Capability::Metadata)?;
    let parent = metadata::title(app, profile, &access, category, title).await?;
    if parent.identity.kind != kind {
        return Err(unavailable_mapping());
    }
    let episode = if parts.len() == 5 {
        if category != "tv" {
            return Err(unavailable_mapping());
        }
        metadata::episodes(app, profile, &access, title, &parent)
            .await?
            .into_iter()
            .find(|episode| episode.identity.id == id)
            .ok_or_else(unavailable_mapping)
            .map(Some)?
    } else {
        None
    };
    let target = viptv_engine::simkl::source_identity(&parent, episode.as_ref())
        .map_err(|_| unavailable_mapping())?;
    Ok(Some(Resolved { target, parent }))
}

/// Enrich from authoritative provider metadata while retaining canonical library/playback IDs.
pub(crate) async fn resolve_request(
    app: &App,
    value: &mut Value,
) -> Result<Option<String>, ApiError> {
    let kind = media_type(value)?.to_string();
    let id = text(value, "id", 512)?.to_owned();
    let Some(resolved) = resolve_id(app, &kind, &id).await? else {
        return Ok(None);
    };
    let imdb = viptv_engine::identity::resolve(&resolved.parent.identity, "imdb", &[])
        .map_err(|_| unavailable_mapping())?;
    if let Some(coordinates) = &resolved.target.episode {
        for (key, expected) in [
            ("season", coordinates.season.unwrap_or(0)),
            ("episode", coordinates.episode),
        ] {
            if !value[key].is_null() && value[key].as_u64() != Some(u64::from(expected)) {
                return Err(unavailable_mapping());
            }
            value[key] = json!(expected);
        }
        if value["series_id"]
            .as_str()
            .is_some_and(|supplied| supplied != resolved.parent.identity.id)
        {
            return Err(unavailable_mapping());
        }
        value["series_id"] = json!(resolved.parent.identity.id);
    } else if !value["season"].is_null() || !value["episode"].is_null() {
        return Err(unavailable_mapping());
    }
    value["imdb_id"] = json!(imdb.value);
    value["name"] = json!(resolved.parent.title);
    value["year"] = resolved
        .parent
        .subtitle
        .as_deref()
        .and_then(|s| s.parse::<u32>().ok())
        .map_or(Value::Null, |year| json!(year));
    // Never let unverified alternate IDs or aliases override the authoritative mapping.
    for key in ["tmdb_id", "aliases"] {
        if let Some(object) = value.as_object_mut() {
            object.remove(key);
        }
    }
    Ok(Some(resolved.target.id))
}
