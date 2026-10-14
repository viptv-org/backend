use super::*;
pub(super) async fn document(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    request: &NextEpisodeRequest,
    simkl: bool,
) -> Result<(Card, Vec<EpisodeAvailability>), ApiError> {
    let parent = request
        .parent_identity
        .as_ref()
        .ok_or_else(|| ApiError::from("An explicit parent identity is required"))?;
    if simkl {
        return crate::simkl::continuation_document(app, profile, &parent.kind, &parent.id).await;
    }
    let (host, revision) = configuration::apply(
        app.clone(),
        lease,
        profile,
        viptv_engine::capabilities::Capability::Metadata,
    )?;
    let raw = host.addons.meta("series", &parent.id).await?;
    configuration::validate_revision(app, profile, revision)?;
    let meta = if raw["meta"].is_object() {
        &raw["meta"]
    } else {
        &raw
    };
    if meta["id"].as_str() != Some(&parent.id) {
        return Err("Metadata returned a different parent".into());
    }
    let detail =
        viptv_engine::stremio::detail_page(&raw.to_string(), "stremio", "series", &parent.id, 0, 1)
            .map_err(|_| ApiError::from("Parent metadata unavailable"))?;
    let cards = viptv_engine::stremio::episode_cards(meta, "stremio", &detail.item)
        .map_err(|_| ApiError::from("Episode metadata unavailable"))?;
    let rows = meta["videos"]
        .as_array()
        .ok_or_else(|| ApiError::from("Episode document unavailable"))?;
    let mut identities = std::collections::HashMap::new();
    for row in rows {
        if let Some(id) = row["id"].as_str() {
            let coordinates = (row["season"].clone(), row["episode"].clone());
            if identities
                .insert(id, coordinates.clone())
                .is_some_and(|previous| previous != coordinates)
            {
                return Err("Episode metadata is ambiguous".into());
            }
        }
    }
    let by_id = rows
        .iter()
        .filter_map(|row| Some((row["id"].as_str()?, row)))
        .collect::<std::collections::HashMap<_, _>>();
    let entries = cards
        .into_iter()
        .filter_map(|card| {
            let row = by_id.get(card.identity.id.as_str())?;
            Some(EpisodeAvailability {
                card,
                released_at_millis: clock(&row["released"]).or_else(|| clock(&row["date"])),
                aired: row["aired"].as_bool(),
            })
        })
        .collect();
    Ok((detail.item, entries))
}
