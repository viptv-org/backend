use super::*;

/// One series' episode document from the profile's provider (Simkl when it is
/// the browsing integration, otherwise the profile's metadata add-ons) or,
/// when that provider cannot serve the series, from the series' own provider:
/// a `tt…` Stremio import Simkl has no identity for is read from the metadata
/// add-ons, a `simkl:` series the add-ons cannot read from Simkl. A series
/// Simkl does identify stays on Simkl, also while Simkl fails, so its facts
/// never alternate between providers. Continue Watching's release facts and
/// the Calendar's library episodes both read through here, so they agree.
/// `fresh` skips the add-on response cache; `timeout` bounds the whole read.
pub(super) async fn series_document(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    request: &NextEpisodeRequest,
    simkl: bool,
    fresh: bool,
    timeout: Duration,
) -> Result<(Card, Vec<EpisodeAvailability>), ApiError> {
    let parent = request
        .parent_identity
        .as_ref()
        .ok_or_else(|| ApiError::from("An explicit parent identity is required"))?;
    let deadline = tokio::time::Instant::now() + timeout;
    // Simkl browsing leaves the profile's own metadata scope without add-ons,
    // so reading them for Simkl's misses names them explicitly.
    let addons = ("stremio".to_owned(), None, String::new());
    let read = |simkl: bool, named: bool| {
        let explicit = named.then_some(&addons);
        async move {
            tokio::time::timeout_at(
                deadline,
                document(app, lease, profile, request, simkl, explicit, None, fresh),
            )
            .await
            .map_err(|_| ApiError::from("Metadata unavailable"))?
        }
    };
    if simkl && !crate::simkl::continuation_identifies(app, profile, &parent.kind, &parent.id) {
        return read(false, true).await;
    }
    match read(simkl, false).await {
        Err(error)
            if !simkl
                && parent.id.starts_with("simkl:")
                && !matches!(error.0, StatusCode::UNAUTHORIZED | StatusCode::CONFLICT) =>
        {
            read(true, false).await
        }
        result => result,
    }
}

pub(super) async fn document(
    app: &App,
    lease: &ResourceLease,
    profile: i64,
    request: &NextEpisodeRequest,
    simkl: bool,
    explicit: Option<&(String, Option<i64>, String)>,
    metadata_origin: Option<&viptv_engine::origin::MetadataOrigin>,
    fresh: bool,
) -> Result<(Card, Vec<EpisodeAvailability>), ApiError> {
    let parent = request
        .parent_identity
        .as_ref()
        .ok_or_else(|| ApiError::from("An explicit parent identity is required"))?;
    if simkl {
        let (mut parent, mut entries) =
            crate::simkl::continuation_document(app, profile, &parent.kind, &parent.id).await?;
        if let Some(origin) = metadata_origin {
            parent.metadata_origin = Some(origin.clone());
            for entry in &mut entries {
                entry.card.metadata_origin = Some(origin.clone());
            }
        }
        return Ok((parent, entries));
    }
    let (host, origin, revision) = if let Some((provider, addon, _)) = explicit {
        super::super::search::metadata_scope(app.clone(), lease, profile, provider, *addon)?
    } else {
        let (host, revision) = configuration::apply(
            app.clone(),
            lease,
            profile,
            viptv_engine::capabilities::Capability::Metadata,
        )?;
        (host, "stremio".to_owned(), revision)
    };
    if fresh {
        let _ = host.addons.forget_meta("series", &parent.id).await;
    }
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
    let mut detail =
        viptv_engine::stremio::detail_page(&raw.to_string(), &origin, "series", &parent.id, 0, 1)
            .map_err(|_| ApiError::from("Parent metadata unavailable"))?;
    let mut cards = viptv_engine::stremio::episode_cards(meta, &origin, &detail.item)
        .map_err(|_| ApiError::from("Episode metadata unavailable"))?;
    if let Some(origin) = metadata_origin {
        detail.item.metadata_origin = Some(origin.clone());
        for card in &mut cards {
            card.metadata_origin = Some(origin.clone());
        }
        let mut db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        let tx = db.transaction().map_err(db_error)?;
        lease.validate(&tx)?;
        kids::scoped::observe_document(
            &tx,
            lease.principal.account_id().ok_or_else(auth::forbidden)?,
            &detail.item,
            &cards,
            meta,
        )?;
        kids::scoped::require_card_db(&tx, &lease.principal, &detail.item)?;
        tx.commit().map_err(db_error)?;
    }
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
