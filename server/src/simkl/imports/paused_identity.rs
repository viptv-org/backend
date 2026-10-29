use serde_json::Value;
use viptv_engine::presentation::Card;

fn coordinate(value: &Value) -> Option<Option<u32>> {
    if value.is_null() {
        return Some(None);
    }
    u32::try_from(value.as_u64()?).ok().map(Some)
}

pub(super) fn select(episode: &Value, cards: Vec<Card>, anime: bool) -> Option<Card> {
    let mut exact_id: Option<String> = None;
    for key in ["simkl", "simkl_id"] {
        let value = &episode["ids"][key];
        if value.is_null() {
            continue;
        }
        let id = value.as_u64().filter(|id| *id > 0)?.to_string();
        if exact_id.as_ref().is_some_and(|current| current != &id) {
            return None;
        }
        exact_id = Some(id);
    }
    let (season, number) = if anime {
        (
            coordinate(&episode["tvdb_season"])?,
            coordinate(&episode["tvdb_number"])?,
        )
    } else {
        let number = coordinate(&episode["number"])?;
        let alternate = coordinate(&episode["episode"])?;
        if number.zip(alternate).is_some_and(|(a, b)| a != b) {
            return None;
        }
        (coordinate(&episode["season"])?, number.or(alternate))
    };
    if number == Some(0) || exact_id.is_none() && (season.is_none() || number.is_none()) {
        return None;
    }
    let mut matches = cards.into_iter().filter(|card| {
        exact_id.as_ref().is_none_or(|id| {
            card.identity
                .ids
                .iter()
                .any(|external| external.namespace == "simkl_episode" && &external.value == id)
        }) && card.identity.episode.as_ref().is_some_and(|coordinates| {
            coordinates.order == "aired"
                && season.is_none_or(|season| coordinates.season == Some(season))
                && number.is_none_or(|number| coordinates.episode == number)
        })
    });
    let matched = matches.next()?;
    matches.next().is_none().then_some(matched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn cards(category: &str) -> Vec<Card> {
        let parent =
            viptv_engine::simkl::card(&json!({"title":"Fixture","ids":{"simkl":30}}), category)
                .unwrap();
        viptv_engine::simkl::episodes(
            &json!([
                {"type":"episode","season":1,"episode":1,"ids":{"simkl":301},"tvdb":{"season":1,"episode":1}},
                {"type":"episode","season":1,"episode":2,"ids":{"simkl":302},"tvdb":{"season":1,"episode":2}}
            ]),
            &parent,
        )
        .unwrap()
    }
    #[test]
    fn conflicting_hints_never_depend_on_episode_order_and_single_verified_forms_still_work() {
        let conflicting = json!({"ids":{"simkl":302},"tvdb_season":1,"tvdb_number":1});
        let mut rows = cards("anime");
        assert!(select(&conflicting, rows.clone(), true).is_none());
        rows.reverse();
        assert!(select(&conflicting, rows, true).is_none());
        for episode in [
            json!({"ids":{"simkl":302}}),
            json!({"ids":{"simkl_id":302}}),
            json!({"tvdb_season":1,"tvdb_number":2}),
        ] {
            assert_eq!(
                select(&episode, cards("anime"), true).unwrap().identity.id,
                "simkl:anime:30:1:2"
            );
        }
        assert!(
            select(
                &json!({"ids":{"simkl":302},"season":1,"number":1}),
                cards("tv"),
                false
            )
            .is_none()
        );
        assert!(
            select(
                &json!({"ids":{"simkl":302},"season":1,"number":2,"episode":1}),
                cards("tv"),
                false
            )
            .is_none()
        );
        assert_eq!(
            select(&json!({"season":1,"number":2}), cards("tv"), false)
                .unwrap()
                .identity
                .id,
            "simkl:tv:30:1:2"
        );
        for invalid in [
            json!({"ids":{"simkl":302,"simkl_id":301}}),
            json!({"ids":{"simkl":302},"tvdb_season":2}),
            json!({"tvdb_number":2}),
            json!({"ids":{"simkl":302},"tvdb_number":0}),
            json!({"ids":{"simkl":"invalid"},"tvdb_season":1,"tvdb_number":2}),
        ] {
            assert!(select(&invalid, cards("anime"), true).is_none());
        }
    }
}
