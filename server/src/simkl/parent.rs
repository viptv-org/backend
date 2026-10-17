//! Explicit parent selection uses authoritative provider documents, never client card claims.
use super::*;
use viptv_engine::{presentation::Card, simkl as projection};
pub(crate) async fn search(
    app: &App,
    parent_scope: &crate::kids::parent_scope::ParentScope,
    category: &str,
    query: &str,
) -> Result<Vec<Card>, ApiError> {
    parent_scope.recheck(app)?;
    let profile = parent_scope.profile();
    projection::category(category).map_err(|_| ApiError::from("Invalid Simkl category"))?;
    let credential = store::credential_parent(app, parent_scope, Capability::Search)?;
    let params = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("q", query)
        .append_pair("page", "1")
        .append_pair("limit", "24")
        .append_pair("extended", "full")
        .finish();
    let value = store::request(
        app,
        profile,
        &credential,
        &format!("/search/{category}?{params}"),
        None,
    )
    .await?;
    let items = value
        .as_array()
        .filter(|items| items.len() <= 24)
        .ok_or("Simkl search was invalid")?;
    let cards = items
        .iter()
        .filter(|item| !crate::kids::adult(item))
        .filter_map(|item| projection::card(item, category).ok())
        .collect();
    store::validate(app, profile, &credential)?;
    parent_scope.recheck(app)?;
    Ok(cards)
}
pub(crate) async fn metadata(
    app: &App,
    parent_scope: &crate::kids::parent_scope::ParentScope,
    kind: &str,
    identity: &str,
) -> Result<(Value, Option<u64>), ApiError> {
    parent_scope.recheck(app)?;
    let profile = parent_scope.profile();
    let parts = identity.split(':').collect::<Vec<_>>();
    let ["simkl", category, id] = parts.as_slice() else {
        return Err("Invalid Simkl title identity".into());
    };
    let id = id
        .parse::<u64>()
        .map_err(|_| ApiError::from("Invalid Simkl title identity"))?;
    let credential = store::credential_parent(app, parent_scope, Capability::Metadata)?;
    let endpoint = if *category == "movie" {
        "movies"
    } else {
        category
    };
    let document = cache::metadata(
        app,
        profile,
        &credential,
        &format!("/{endpoint}/{id}?extended=full"),
    )
    .await?;
    let mut document = (*document).clone();
    if projection::id(&document) != Some(id) {
        return Err("Metadata identity did not match the requested title".into());
    }
    let parent = projection::card(&document, category)
        .map_err(|_| ApiError::from("Simkl title was invalid"))?;
    if parent.identity.id != identity || parent.identity.kind != kind {
        return Err("Metadata identity did not match the requested title".into());
    }
    document["id"] = json!(identity);
    document["type"] = json!(kind);
    document["name"] = json!(parent.title);
    document["poster"] = json!(parent.poster);
    document["background"] = json!(parent.landscape);
    document["description"] = json!(parent.description);
    if kind == "series" {
        let raw = cache::metadata(
            app,
            profile,
            &credential,
            &format!("/{category}/episodes/{id}"),
        )
        .await?;
        let episodes = projection::episodes(&raw, &parent)
            .map_err(|_| ApiError::from("Simkl episode identity was invalid"))?;
        let by_id = raw
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|video| projection::id(video).map(|id| (id, video)))
            .collect::<HashMap<_, _>>();
        let mut videos = Vec::new();
        for card in episodes.into_iter().take(2000) {
            let coordinates = card.identity.episode.as_ref();
            let mut video = json!({"id":card.identity.id,"season":coordinates.and_then(|c|c.season),"episode":coordinates.map(|c|c.episode),"title":card.title,"thumbnail":card.landscape});
            if let Some(original) = card
                .identity
                .ids
                .iter()
                .find(|id| id.namespace == "simkl_episode")
                .and_then(|id| id.value.parse::<u64>().ok())
                .and_then(|id| by_id.get(&id))
            {
                for key in [
                    "certification",
                    "contentRating",
                    "ageRating",
                    "mpaaRating",
                    "genres",
                ] {
                    if let Some(value) = original.get(key) {
                        video[key] = value.clone();
                    }
                }
            }
            videos.push(video);
        }
        document["videos"] = json!(videos);
    }
    store::validate(app, profile, &credential)?;
    parent_scope.recheck(app)?;
    Ok((document, credential.revision))
}
