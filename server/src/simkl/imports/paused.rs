#[path = "anime_pause_identity.rs"]
mod anime_identity;
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
    let mut titles = [("movie", "movie"), ("show", "tv"), ("anime", "anime")]
        .into_iter()
        .filter_map(|(key, category)| value[key].is_object().then_some((&value[key], category)));
    let (data, category) = titles.next()?;
    if titles.next().is_some() {
        return None;
    }
    let id = viptv_engine::simkl::id(data)?;
    let parent = entries
        .iter()
        .find(|entry| entry.card.identity.id == format!("simkl:{category}:{id}"))
        .map(|entry| entry.card.clone())
        .or_else(|| viptv_engine::simkl::card(data, category).ok())?;
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
        let document = cache::metadata(app, profile, access, &format!("/{category}/episodes/{id}"))
            .await
            .ok()?;
        let all = viptv_engine::simkl::episodes(&document, &verified).ok()?;
        if category == "anime" {
            anime_identity::select(episode, &document, all)?
        } else {
            identity::select(episode, all, false)?
        }
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
