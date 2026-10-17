use crate::*;
use viptv_engine::{identity::EpisodeCoordinates, presentation::Card, stremio};

pub(super) fn card(item: &Value, provider: &str) -> Option<Card> {
    let mut normalized = item.clone();
    if normalized["type"].is_null() {
        normalized["type"] = json!("movie");
    }
    if normalized["background"].is_null() {
        normalized["background"] = normalized["thumbnail"].clone();
    }
    if normalized["releaseInfo"].is_null() {
        normalized["releaseInfo"] = normalized["episodeTitle"].clone();
    }
    // Use the same bounded identity and presentation projection as local hosts.
    // Truncating provider identifiers would silently identify another title.
    let mut card = stremio::cards(&json!({"metas":[normalized]}).to_string(), provider)
        .ok()?
        .into_iter()
        .next()?;
    if card.identity.kind == "series" {
        if let (Some(season), Some(episode)) = (item["season"].as_u64(), item["episode"].as_u64()) {
            card.identity.episode = Some(EpisodeCoordinates {
                order: "aired".into(),
                season: Some(u32::try_from(season).ok()?),
                episode: u32::try_from(episode).ok()?,
            });
        }
    }
    // Watch progress is private library state, never provider-supplied metadata.
    if provider == "library" {
        card.progress = item["position"]
            .as_f64()
            .zip(item["duration"].as_f64())
            .filter(|(p, d)| p.is_finite() && d.is_finite() && *d > 0.)
            .map(|(p, d)| (p / d).clamp(0., 1.));
    }
    Some(card)
}

pub(super) fn cards(app: &App, items: Vec<Value>, provider: &str) -> Result<Vec<Card>, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_media(&db)?;
    let origin = super::origins::for_provider_db(
        &db,
        app.identity().account_id().ok_or_else(auth::forbidden)?,
        provider,
    )?;
    let restricted = kids::restricted(&db, &app.identity())?;
    let mut cards = Vec::new();
    for item in items.iter().take(24) {
        let Some(mut projected) = card(item, provider) else {
            continue;
        };
        projected.metadata_origin = origin.clone();
        if restricted {
            if projected.identity.id.starts_with("simkl:")
                && (provider == "stremio" || provider.starts_with("addon:"))
            {
                continue;
            }
            if let Some(origin) = &origin {
                if let Ok(stored) = kids::scoped::lookup_db(
                    &db,
                    &app.identity(),
                    origin,
                    &projected.identity.kind,
                    &projected.identity.id,
                    Some(&projected),
                ) {
                    cards.push(stored);
                    continue;
                }
            }
            if origin.as_ref().is_some_and(|origin| {
                !kids::titles::canonical(
                    &origin.provider_id,
                    &projected.identity.kind,
                    &projected.identity.id,
                )
            }) {
                continue;
            }
            if kids::require_item(
                &db,
                &app.identity(),
                &projected.identity.kind,
                &projected.identity.id,
            )
            .is_err()
            {
                continue;
            }
            let safe = kids::filter_items(&db, &app.identity(), vec![item.clone()])?;
            let Some(mut projected) = safe.first().and_then(|item| card(item, provider)) else {
                continue;
            };
            projected.metadata_origin = origin.clone();
            cards.push(projected);
        } else {
            cards.push(projected);
        }
    }
    Ok(cards)
}

pub(super) fn allowed_cards(app: &App, cards: Vec<Card>) -> Result<Vec<Card>, ApiError> {
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_media(&db)?;
    let restricted = kids::restricted(&db, &app.identity())?;
    let mut safe = Vec::new();
    for card in cards {
        if !restricted {
            safe.push(card);
            continue;
        }
        if card.identity.id.starts_with("simkl:")
            && (card.provider_id == "stremio" || card.provider_id.starts_with("addon:"))
        {
            continue;
        }
        if let Some(origin) = &card.metadata_origin {
            if let Ok(stored) = kids::scoped::lookup_db(
                &db,
                &app.identity(),
                origin,
                &card.identity.kind,
                &card.identity.id,
                Some(&card),
            ) {
                safe.push(stored);
                continue;
            }
            if !kids::titles::canonical(&origin.provider_id, &card.identity.kind, &card.identity.id)
            {
                continue;
            }
        }
        if kids::require_item(&db, &app.identity(), &card.identity.kind, &card.identity.id).is_ok()
        {
            safe.push(card);
        }
    }
    Ok(safe)
}
