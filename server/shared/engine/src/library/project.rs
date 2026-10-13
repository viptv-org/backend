use super::*;
/// Date lower bounds are supplied by the host from its real UTC clock, never title/date labels.
pub fn project(
    scope: Scope,
    revision: u64,
    query: LibraryQuery,
    mut entries: Vec<Entry>,
    sources: Vec<LibrarySource>,
    now: u64,
    added_lower: Option<u64>,
) -> Result<LibrarySnapshot, Error> {
    query.validate()?;
    if query.source == "favorites"
        && (query.status.is_some()
            || query.rated.is_some()
            || query.episodes_left_max.is_some()
            || !matches!(query.show.as_str(), "all" | "watched" | "not_watched"))
    {
        return Err(Error::InvalidQuery);
    }
    let controls = controls(&entries, &sources, &query.source);
    entries.retain(|entry| {
        let c = &entry.card;
        let s = &entry.statistics;
        let kind = if c.category.as_deref() == Some("anime") {
            "anime"
        } else {
            c.identity.kind.as_str()
        };
        if query.r#type != "all" && kind != query.r#type {
            return false;
        }
        if query
            .status
            .as_ref()
            .is_some_and(|status| s.status.as_ref() != Some(status))
        {
            return false;
        }
        if query.genre.as_ref().is_some_and(|genre| {
            !genre.is_empty()
                && !c
                    .metadata
                    .as_ref()
                    .is_some_and(|m| m.genres.iter().any(|g| g.eq_ignore_ascii_case(genre)))
        }) {
            return false;
        }
        if let Some(lower) = added_lower {
            if c.added_at_millis
                .is_none_or(|added| added < lower || added > now)
            {
                return false;
            }
        }
        if query.rated.as_deref() == Some("rated_8_plus")
            && s.user_rating.is_none_or(|rating| rating < 8.0)
        {
            return false;
        }
        if query.rated.as_deref() == Some("not_rated") && s.user_rating.is_some() {
            return false;
        }
        if query.episodes_left_max.is_some_and(|max| {
            s.total_episode_count
                .zip(s.watched_episode_count)
                .is_none_or(|(total, watched)| total.saturating_sub(watched) > max)
        }) {
            return false;
        }
        match query.show.as_str() {
            "watched" => s.watched,
            "not_watched" => !s.watched,
            "has_new_episode" => {
                s.has_unwatched_aired_episodes.unwrap_or_else(|| s.next_episode.is_some() && s.next_air_millis.is_some_and(|date| date <= now))
            }
            "not_started" => {
                s.watched_episode_count == Some(0) || c.identity.kind == "movie" && !s.watched
            }
            "caught_up" => s.has_unwatched_aired_episodes.map(|has| !has).unwrap_or_else(|| s
                .aired_episode_count
                .zip(s.watched_episode_count)
                .is_some_and(|(aired, watched)| aired > 0 && watched >= aired)),
            "episodes_to_air" => s
                .total_episode_count
                .zip(s.aired_episode_count)
                .is_some_and(|(total, aired)| total > aired),
            _ => true,
        }
    });
    entries.sort_by(|a, b| {
        let name = || {
            a.card
                .title
                .to_lowercase()
                .cmp(&b.card.title.to_lowercase())
                .then_with(|| a.card.identity.id.cmp(&b.card.identity.id))
        };
        match query.sort.as_str() {
            "name_asc" => name(),
            "name_desc" => name().reverse(),
            "last_watched" => b
                .statistics
                .last_watched_millis
                .cmp(&a.statistics.last_watched_millis)
                .then_with(name),
            "times_watched" => b
                .statistics
                .times_watched
                .cmp(&a.statistics.times_watched)
                .then_with(name),
            _ => b
                .card
                .added_at_millis
                .cmp(&a.card.added_at_millis)
                .then_with(name),
        }
    });
    let total = entries.len();
    let page = entries
        .into_iter()
        .skip(query.offset as usize)
        .take(query.limit as usize)
        .collect::<Vec<_>>();
    let next = query.offset as usize + page.len();
    let complete = next >= total;
    Ok(LibrarySnapshot {
        scope,
        revision,
        query,
        cards: page.iter().map(|e| e.card.clone()).collect(),
        statistics: page.into_iter().map(|e| e.statistics).collect(),
        controls,
        sources,
        next_cursor: (!complete).then(|| next.to_string()),
        complete,
        availability: Availability::Available,
        reason: None,
    })
}
