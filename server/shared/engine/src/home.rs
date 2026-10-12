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
    snapshot.hero = snapshot
        .rows
        .iter()
        .flat_map(|r| &r.cards)
        .find(|c| c.landscape.is_some())
        .cloned();
    snapshot
}
