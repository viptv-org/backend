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
    let safe = kids::filter_items(&db, &app.identity(), items)?;
    Ok(safe
        .iter()
        .take(24)
        .filter_map(|v| card(v, provider))
        .collect())
}
