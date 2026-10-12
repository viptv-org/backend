use crate::*;
use viptv_engine::{
    identity::{ExternalId, MediaIdentity},
    presentation::Card,
};

fn text(value: &Value, key: &str, maximum: usize) -> Option<String> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.chars().take(maximum).collect())
}

pub(super) fn card(item: &Value, provider: &str) -> Option<Card> {
    let id = text(item, "id", 512)?;
    let kind = text(item, "type", 64).unwrap_or_else(|| "movie".into());
    let ids = if id
        .strip_prefix("tt")
        .is_some_and(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()))
    {
        vec![ExternalId {
            namespace: "imdb".into(),
            value: id.clone(),
        }]
    } else {
        vec![]
    };
    let progress = item["position"]
        .as_f64()
        .zip(item["duration"].as_f64())
        .filter(|(p, d)| p.is_finite() && d.is_finite() && *d > 0.)
        .map(|(p, d)| (p / d).clamp(0., 1.));
    Some(Card {
        identity: MediaIdentity {
            id: id.clone(),
            kind,
            ids,
            episode: None,
            edition: None,
        },
        provider_id: provider.into(),
        title: text(item, "name", 512).unwrap_or(id),
        subtitle: text(item, "releaseInfo", 256).or_else(|| text(item, "episodeTitle", 256)),
        description: text(item, "description", 1600),
        poster: text(item, "poster", 4096),
        landscape: text(item, "background", 4096).or_else(|| text(item, "thumbnail", 4096)),
        logo: text(item, "logo", 4096),
        progress,
        badges: vec![],
    })
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
