#[path = "paused_identity.rs"]
mod identity;
use super::*;
use viptv_engine::presentation::Card;
pub(super) struct Pause {
    pub card: Card,
    pub parent: Option<Card>,
    pub library_card: Card,
    pub position: f64,
    pub duration: f64,
    pub updated: u64,
}
pub(super) async fn pause(
    app: &App,
    profile: i64,
    access: &store::Credential,
    value: &Value,
    entries: &[viptv_engine::library::Entry],
) -> Option<Pause> {
    let percentage = value["progress"]
        .as_f64()
        .filter(|p| p.is_finite() && *p > 0.0 && *p < 100.0)?;
    let updated =
        progress::millis(&value["paused_at"]).or_else(|| progress::millis(&value["watched_at"]))?;
    let data = ["movie", "show", "anime"]
        .into_iter()
        .find_map(|key| value[key].is_object().then_some(&value[key]))?;
    let id = data["ids"]["simkl"].as_u64()?;
    let parent =
        entries
            .iter()
            .find(|e| {
                e.card.identity.ids.iter().any(|external| {
                    external.namespace == "simkl" && external.value == id.to_string()
                })
            })
            .map(|e| e.card.clone())
            .or_else(|| {
                let category = if value["movie"].is_object() {
                    "movie"
                } else if value["anime"].is_object() {
                    "anime"
                } else {
                    "tv"
                };
                viptv_engine::simkl::card(data, category).ok()
            })?;
    let category = if parent.category.as_deref() == Some("anime") {
        "anime"
    } else if parent.identity.kind == "movie" {
        "movie"
    } else {
        "tv"
    };
    let verified = metadata::title(app, profile, access, category, id)
        .await
        .ok()?;
    if verified.identity.id != parent.identity.id
        || verified.identity.kind != parent.identity.kind
        || parent.identity.ids.iter().any(|hint| {
            verified
                .identity
                .ids
                .iter()
                .any(|actual| actual.namespace == hint.namespace && actual.value != hint.value)
        })
    {
        return None;
    }
    let card = if verified.identity.kind == "movie" {
        verified.clone()
    } else {
        let episode = &value["episode"];
        let all = metadata::episodes(app, profile, access, id, &verified)
            .await
            .ok()?;
        // Supplied Simkl IDs and aired-coordinate hints must agree.
        identity::select(episode, all, category == "anime")?
    };
    let duration = card
        .metadata
        .as_ref()?
        .runtime_minutes
        .map(|n| f64::from(n) * 60.0)?;
    Some(Pause {
        library_card: verified.clone(),
        parent: (verified.identity.kind == "series").then_some(verified),
        card,
        position: duration * percentage / 100.0,
        duration,
        updated,
    })
}
