//! One scoped authoritative metadata document for generic next-episode resolution.
use super::*;
use viptv_engine::{continuation::EpisodeAvailability, presentation::Card};
pub(crate) async fn episodes(
    app: &App,
    profile: i64,
    kind: &str,
    parent_id: &str,
) -> Result<(Card, Vec<EpisodeAvailability>), ApiError> {
    let mapped = if parent_id.starts_with("simkl:") {
        None
    } else {
        imports::remote_identity(app, profile, kind, parent_id)?
    };
    let lookup = mapped.as_deref().unwrap_or(parent_id);
    let parts = lookup.split(':').collect::<Vec<_>>();
    let ["simkl", category, id] = parts.as_slice() else {
        return Err("Verified Simkl parent identity is required".into());
    };
    if !matches!(*category, "tv" | "anime") {
        return Err("A series parent identity is required".into());
    }
    let id = id
        .parse::<u64>()
        .map_err(|_| ApiError::from("Invalid Simkl identity"))?;
    let access = store::credential(app, profile, Capability::Metadata)?;
    let mut parent = metadata::title(app, profile, &access, category, id).await?;
    if parent.identity.kind != kind {
        return Err("Parent title kind does not match".into());
    }
    let raw = cache::metadata(app, profile, &access, &format!("/{category}/episodes/{id}")).await?;
    let cards = viptv_engine::simkl::episodes(&raw, &parent)
        .map_err(|_| ApiError::from("Episode metadata is ambiguous"))?;
    if mapped.is_some() {
        if !parent
            .identity
            .ids
            .iter()
            .any(|id| id.namespace == "imdb" && id.value == parent_id)
        {
            return Err("Verified title mapping changed".into());
        }
        parent.identity.id = parent_id.into();
    }
    let rows = raw
        .as_array()
        .ok_or_else(|| ApiError::from("Invalid episode document"))?;
    let by_id = rows
        .iter()
        .filter_map(|row| Some((viptv_engine::simkl::id(row)?, row)))
        .collect::<HashMap<_, _>>();
    let mut available = Vec::new();
    for mut card in cards {
        let Some(id) = card
            .identity
            .ids
            .iter()
            .find(|id| id.namespace == "simkl_episode")
            .and_then(|id| id.value.parse::<u64>().ok())
        else {
            continue;
        };
        let Some(raw) = by_id.get(&id) else {
            continue;
        };
        if mapped.is_some() {
            let coordinates = card
                .identity
                .episode
                .as_ref()
                .ok_or_else(|| ApiError::from("Invalid episode coordinates"))?;
            card.identity.id = format!(
                "{parent_id}:{}:{}",
                coordinates
                    .season
                    .ok_or_else(|| ApiError::from("Unresolved episode season"))?,
                coordinates.episode
            );
            card.parent_identity = Some(parent.identity.clone());
        }
        available.push(EpisodeAvailability {
            card,
            released_at_millis: crate::engine_v3::continuation::release_millis(&raw["date"]),
            aired: raw["aired"].as_bool(),
        });
    }
    store::validate(app, profile, &access)?;
    Ok((parent, available))
}
