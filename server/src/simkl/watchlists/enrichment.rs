//! Private, fenced background projection over coalesced public detail reads.
use super::*;
#[derive(Default)]
pub(in crate::simkl) struct State {
    cards: HashMap<(String, String), viptv_engine::presentation::Card>,
    running: bool,
    attempted: Option<Instant>,
}
fn key(card: &viptv_engine::presentation::Card) -> (String, String) {
    (card.identity.kind.clone(), card.identity.id.clone())
}
pub(super) async fn apply(
    app: &App,
    profile: i64,
    access: &store::Credential,
    lease: &ResourceLease,
    owner: Arc<tokio::sync::Mutex<Option<sync::Snapshot>>>,
    state: Arc<tokio::sync::Mutex<State>>,
    entries: &mut [Entry],
) -> (usize, usize) {
    let mut metadata = state.lock().await;
    for entry in entries.iter_mut() {
        if let Some(card) = metadata.cards.get(&key(&entry.card)) {
            entry.card = viptv_engine::presentation::enrich_card(&entry.card, card);
        } else {
            crate::engine_v3::source_integrations::merge_cached(
                app,
                lease,
                access.configuration_revision,
                std::slice::from_mut(&mut entry.card),
            );
            if entry
                .card
                .metadata
                .as_ref()
                .is_some_and(|m| !m.genres.is_empty())
            {
                metadata.cards.insert(key(&entry.card), entry.card.clone());
            }
        }
    }
    let missing = entries
        .iter()
        .filter(|entry| !metadata.cards.contains_key(&key(&entry.card)))
        .map(|e| e.card.clone())
        .collect::<Vec<_>>();
    let known = entries.len() - missing.len();
    if !missing.is_empty()
        && !metadata.running
        && metadata
            .attempted
            .is_none_or(|at| at.elapsed() > Duration::from_secs(10))
    {
        metadata.running = true;
        metadata.attempted = Some(Instant::now());
        let app = app.clone();
        let configuration = access.configuration_revision;
        let revision = access.revision;
        let state = state.clone();
        tokio::spawn(async move {
            let _owner = owner;
            let work = async {
                use futures::{StreamExt, stream};
                let results = stream::iter(missing.into_iter().take(2000).map(|card| {
                    let app = app.clone();
                    let state = state.clone();
                    async move {
                        let _permit = app.simkl.watchlist_metadata_limit.acquire().await.ok()?;
                        let access =
                            store::credential(&app, profile, Capability::Authorization).ok()?;
                        if access.configuration_revision != configuration
                            || access.revision != revision
                        {
                            return None;
                        }
                        let id = card
                            .identity
                            .ids
                            .iter()
                            .find(|id| id.namespace == "simkl")?
                            .value
                            .parse::<u64>()
                            .ok()?;
                        let category = if card.category.as_deref() == Some("anime") {
                            "anime"
                        } else if card.identity.kind == "movie" {
                            "movie"
                        } else {
                            "tv"
                        };
                        let result = tokio::time::timeout(
                            Duration::from_secs(3),
                            metadata::title(&app, profile, &access, category, id),
                        )
                        .await
                        .ok()?
                        .ok()?;
                        if result.identity.id != card.identity.id
                            || result.identity.kind != card.identity.kind
                            || store::validate(&app, profile, &access).is_err()
                        {
                            return None;
                        }
                        state.lock().await.cards.insert(key(&result), result);
                        Some(())
                    }
                }))
                .buffer_unordered(3);
                tokio::pin!(results);
                while results.next().await.is_some() {}
            };
            let _ = tokio::time::timeout(Duration::from_secs(90), work).await;
            state.lock().await.running = false;
        });
    }
    (known, entries.len())
}
