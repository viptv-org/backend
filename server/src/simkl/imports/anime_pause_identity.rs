use super::*;
use viptv_engine::presentation::Card;
fn coordinate(value: &Value) -> Option<Option<u32>> {
    if value.is_null() {
        return Some(None);
    }
    u32::try_from(value.as_u64()?).ok().map(Some)
}
pub(super) fn select(episode: &Value, document: &Value, cards: Vec<Card>) -> Option<Card> {
    if !episode["season"].is_null() && episode["season"].as_u64() != Some(1) {
        return None;
    }
    let number = coordinate(&episode["number"])?;
    let alternate = coordinate(&episode["episode"])?;
    if number.zip(alternate).is_some_and(|(a, b)| a != b) {
        return None;
    }
    let absolute = number.or(alternate);
    if absolute == Some(0) {
        return None;
    }
    let id = if ["simkl", "simkl_id"]
        .iter()
        .any(|key| !episode["ids"][key].is_null())
    {
        Some(viptv_engine::simkl::id(episode)?)
    } else {
        None
    };
    let season = coordinate(&episode["tvdb_season"])?;
    let aired = coordinate(&episode["tvdb_number"])?;
    if aired == Some(0) {
        return None;
    }
    let row = super::super::super::anime_episode::select(document, id, absolute, season, aired)?;
    super::identity::select(
        &json!({"ids":{"simkl":viptv_engine::simkl::id(row)?},"tvdb_season":row["tvdb"]["season"],"tvdb_number":row["tvdb"]["episode"]}),
        cards,
        true,
    )
}
