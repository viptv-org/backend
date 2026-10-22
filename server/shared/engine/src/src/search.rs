//! One host ordering across provider categories; identities and category remain authoritative.
use crate::presentation::Card;
fn normalized(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn score(query: &str, title: &str) -> u8 {
    let title = normalized(title);
    if title == query {
        0
    } else if title.starts_with(&(query.to_owned() + " ")) {
        1
    } else if query
        .split_whitespace()
        .all(|word| title.split_whitespace().any(|s| s == word))
    {
        2
    } else if title.contains(query) {
        3
    } else {
        4
    }
}
pub fn rank(cards: &mut [Card], query: &str) {
    let query = normalized(query);
    if query.is_empty() {
        return;
    }
    cards.sort_by_key(|card| {
        let metadata = card.metadata.as_ref();
        let relevance = metadata
            .into_iter()
            .flat_map(|m| &m.alternate_titles)
            .map(|s| score(&query, s))
            .chain(std::iter::once(score(&query, &card.title)))
            .min()
            .unwrap_or(4);
        (
            relevance,
            std::cmp::Reverse(metadata.and_then(|m| m.rating_votes).unwrap_or(0)),
        )
    });
}
