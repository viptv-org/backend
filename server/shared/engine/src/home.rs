use crate::{presentation::*, profile::Scope};
use futures::{
    future::BoxFuture,
    stream::{FuturesUnordered, StreamExt},
};
use std::time::Duration;

pub struct RowRequest {
    pub id: String,
    pub title: String,
    pub provider_id: String,
    pub load: BoxFuture<'static, Result<Vec<Card>, String>>,
}

/// A slow provider never holds healthy rows behind its deadline. Dropping cancels remaining futures.
pub async fn assemble(scope: Scope, requests: Vec<RowRequest>, budget: Duration) -> HomeSnapshot {
    let mut snapshot = HomeSnapshot {
        revision: scope.revision,
        scope,
        hero: None,
        featured: vec![],
        rows: vec![],
        complete: false,
    };
    let mut pending = FuturesUnordered::new();
    for (index, request) in requests.into_iter().take(16).enumerate() {
        snapshot.rows.push(HomeRow {browse:None,
            id: request.id,
            title: request.title,
            provider_id: request.provider_id,
            state: RowState::Pending,
            cards: vec![],
            next_cursor: None,
            message: None,
        });
        pending.push(async move { (index, request.load.await) });
    }
    let deadline = tokio::time::sleep(budget);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            biased;
            _ = &mut deadline => break,
            next = pending.next() => match next {
                None => { snapshot.complete = true; break; },
                Some((index, result)) => {
                    let row = &mut snapshot.rows[index];
                    match result {
                        Ok(cards) => { row.cards = cards.into_iter().take(24).collect(); row.state = if row.cards.is_empty() { RowState::Empty } else { RowState::Ready }; },
                        Err(_) => { row.state = RowState::Failed; row.message = Some("This row is unavailable. Try again.".into()); }
                    }
                }
            }
        }
    }
    // There is no continuation after this function returns: remaining futures
    // are dropped. Expose a terminal retry state instead of a permanent spinner.
    for row in &mut snapshot.rows {
        if row.state == RowState::Pending {
            row.state = RowState::Failed;
            row.message = Some("This row took too long. Try again.".into());
        }
    }
    snapshot.complete = true;
    snapshot.featured = select_featured(&snapshot.rows);
    snapshot.hero = snapshot.featured.first().cloned();
    snapshot
}

/// Personal rows can never become the initial featured title.
pub fn select_hero(rows: &[HomeRow]) -> Option<Card> {
    select_featured(rows).into_iter().next()
}

/// Stable diversity across trending catalogs; no personal rows or duplicate canonical titles.
pub fn select_featured(rows: &[HomeRow]) -> Vec<Card> {
    let eligible = |row: &&HomeRow| {
        row.provider_id != "library" && !matches!(row.id.as_str(), "continue" | "favorites")
    };
    let trending = |row: &&HomeRow| {
        row.id.ends_with(":trending")
            || row.id.ends_with(":top")
            || row.title.to_lowercase().contains("trending")
    };
    let mut selected = rows
        .iter()
        .filter(eligible)
        .filter(trending)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        selected = rows.iter().filter(eligible).collect();
    }
    let catalogs = selected
        .into_iter()
        .map(|row| {
            let mut cards = row
                .cards
                .iter()
                .filter(|card| {
                    card.landscape.is_some()
                        && card.identity.episode.is_none()
                        && matches!(card.identity.kind.as_str(), "movie" | "series")
                })
                .collect::<Vec<_>>();
            // Prefer actual logo evidence or explicit artwork mappings without fetching each row card.
            cards.sort_by_key(|card| {
                !(card.logo.is_some() || card.identity.ids.iter().any(|id| id.namespace == "imdb"))
            });
            cards
        })
        .collect::<Vec<_>>();
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for index in 0..24 {
        for cards in &catalogs {
            if let Some(card) = cards.get(index) {
                if seen.insert((card.identity.kind.clone(), card.identity.id.clone())) {
                    result.push((*card).clone());
                    if result.len() == 5 {
                        return result;
                    }
                }
            }
        }
    }
    result
}
