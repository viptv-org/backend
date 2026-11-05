use super::{Card, EpisodeSeason};
/// Host pagination uses one stable sequence; opaque provider IDs remain unchanged.
pub fn canonical_episodes(mut cards: Vec<Card>) -> Vec<Card> {
    let mut seen = std::collections::HashSet::new();
    cards.retain(|card| seen.insert((card.identity.kind.clone(), card.identity.id.clone())));
    cards.sort_by(|a, b| {
        let coordinates = |card: &Card| {
            card.identity
                .episode
                .as_ref()
                .map(|e| (e.season_number(), e.episode))
                .unwrap_or((u32::MAX, u32::MAX))
        };
        coordinates(a)
            .cmp(&coordinates(b))
            .then_with(|| a.identity.id.cmp(&b.identity.id))
    });
    cards
}
/// Index the displayed season of every episode; only a card without coordinates has none.
pub fn episode_seasons(cards: &[Card]) -> Vec<EpisodeSeason> {
    let mut seasons = std::collections::BTreeMap::<Option<u32>, EpisodeSeason>::new();
    for (offset, card) in cards.iter().enumerate() {
        let season = card.identity.episode.as_ref().map(|e| e.season_number());
        let Ok(offset) = u32::try_from(offset) else {
            continue;
        };
        let entry = seasons.entry(season).or_insert(EpisodeSeason {
            season,
            episode_count: 0,
            offset,
        });
        entry.episode_count = entry.episode_count.saturating_add(1);
    }
    let mut seasons = seasons.into_values().collect::<Vec<_>>();
    seasons.sort_by_key(|season| season.season.unwrap_or(u32::MAX));
    seasons
}
