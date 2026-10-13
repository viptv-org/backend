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
        rows: vec![],
        complete: false,
    };
    let mut pending = FuturesUnordered::new();
    for (index, request) in requests.into_iter().take(12).enumerate() {
        snapshot.rows.push(HomeRow {
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
    snapshot.hero = select_hero(&snapshot.rows);
    snapshot
}

/// Personal rows can never become the initial featured title.
pub fn select_hero(rows: &[HomeRow]) -> Option<Card> {
    let eligible = |row: &&HomeRow| {
        row.provider_id != "library" && !matches!(row.id.as_str(), "continue" | "favorites")
    };
    let featured = |row: &&HomeRow| {
        row.id.ends_with(":trending")
            || row.id.ends_with(":top")
            || row.title.to_lowercase().contains("trending")
    };
    rows.iter()
        .filter(eligible)
        .filter(featured)
        .chain(rows.iter().filter(eligible))
        .flat_map(|r| &r.cards)
        .find(|c| c.landscape.is_some() && c.identity.episode.is_none())
        .cloned()
}
