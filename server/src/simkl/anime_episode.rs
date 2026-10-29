use serde_json::Value;

pub(super) fn select(
    document: &Value,
    id: Option<u64>,
    absolute: Option<u32>,
    season: Option<u32>,
    episode: Option<u32>,
) -> Option<&Value> {
    let rows = document.as_array().filter(|rows| rows.len() <= 10_000)?;
    if id.is_none() && absolute.is_none() && (season.is_none() || episode.is_none()) {
        return None;
    }
    let mut matches = rows.iter().filter(|row| {
        row["type"] == "episode"
            && row["aired"] != false
            && viptv_engine::simkl::id(row).is_some()
            && id.is_none_or(|id| viptv_engine::simkl::id(row) == Some(id))
            && absolute.is_none_or(|n| row["episode"].as_u64() == Some(u64::from(n)))
            && season.is_none_or(|s| row["tvdb"]["season"].as_u64() == Some(u64::from(s)))
            && episode.is_none_or(|n| row["tvdb"]["episode"].as_u64() == Some(u64::from(n)))
    });
    let selected = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let selected_id = viptv_engine::simkl::id(selected)?;
    if rows
        .iter()
        .filter(|row| viptv_engine::simkl::id(row) == Some(selected_id))
        .count()
        != 1
    {
        return None;
    }
    let aired_season = selected["tvdb"]["season"].as_u64()?;
    let aired_episode = selected["tvdb"]["episode"].as_u64().filter(|n| *n > 0)?;
    if rows
        .iter()
        .filter(|row| {
            row["type"] == "episode"
                && row["tvdb"]["season"].as_u64() == Some(aired_season)
                && row["tvdb"]["episode"].as_u64() == Some(aired_episode)
        })
        .count()
        != 1
    {
        return None;
    }
    if let Some(number) = selected["episode"].as_u64() {
        if number == 0
            || rows
                .iter()
                .filter(|row| row["type"] == "episode" && row["episode"].as_u64() == Some(number))
                .count()
                != 1
        {
            return None;
        }
    }
    Some(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn absolute_and_aired_coordinates_are_independent_and_every_hint_must_agree() {
        let regular = json!({"type":"episode","episode":52,"ids":{"simkl":303},"tvdb":{"season":2,"episode":1}});
        let special = json!({"type":"special","episode":52,"ids":{"simkl":999},"tvdb":{"season":0,"episode":1}});
        let doc = json!([special, regular]);
        for hints in [
            (None, Some(52), None, None),
            (Some(303), None, Some(2), Some(1)),
            (None, None, Some(2), Some(1)),
        ] {
            assert_eq!(
                viptv_engine::simkl::id(select(&doc, hints.0, hints.1, hints.2, hints.3).unwrap()),
                Some(303)
            );
        }
        assert!(select(&doc, Some(303), Some(53), Some(2), Some(1)).is_none());
        assert!(select(&doc, Some(303), Some(52), Some(1), Some(52)).is_none());
        assert!(select(&doc, None, None, Some(2), None).is_none());
        let duplicate = json!([doc[1],{"type":"episode","episode":52,"ids":{"simkl":304},"tvdb":{"season":2,"episode":2}}]);
        assert!(select(&duplicate, None, Some(52), None, None).is_none());
        assert!(select(&duplicate, Some(303), None, Some(2), Some(1)).is_none());
        let duplicate_aired = json!([doc[1],{"type":"episode","episode":53,"ids":{"simkl":304},"tvdb":{"season":2,"episode":1}}]);
        assert!(select(&duplicate_aired, None, Some(52), None, None).is_none());
    }
}
