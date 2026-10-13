use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Watched {
    category: String,
    id: u64,
    season: Option<u32>,
    episode: Option<u32>,
}

/// An explicit user action only. Never infer completion from a title or bulk-mark a series.
pub(crate) async fn watched(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(q): axum::Json<Watched>,
) -> ApiResult {
    if q.id == 0
        || !matches!(q.category.as_str(), "movie" | "tv")
        || (q.category == "movie" && (q.season.is_some() || q.episode.is_some()))
        || (q.category == "tv" && (q.season.is_none() || q.episode.unwrap_or(0) == 0))
    {
        return Err("An exact movie or TV episode is required".into());
    }
    let app = app.with_lease(lease.clone());
    let credential = store::credential(&app, profile, Capability::Tracking)?;
    let parent = metadata::title(&app, profile, &credential, &q.category, q.id).await?;
    let body = if q.category == "movie" {
        json!({"movies":[{"ids":{"simkl":q.id}}]})
    } else {
        let episodes = metadata::episodes(&app, profile, &credential, q.id, &parent).await?;
        if !episodes.iter().any(|e| {
            e.identity
                .episode
                .as_ref()
                .is_some_and(|e| e.season == q.season && Some(e.episode) == q.episode)
        }) {
            return Err("Simkl episode could not be verified".into());
        }
        json!({"shows":[{"ids":{"simkl":q.id},"seasons":[{"number":q.season,"episodes":[{"number":q.episode}]}]}]})
    };
    {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
        kids::require_item(
            &db,
            &lease.principal,
            &parent.identity.kind,
            &parent.identity.id,
        )?;
    }
    let result = store::request(&app, profile, &credential, "/sync/history", Some(body)).await?;
    // Do not echo upstream account data. A partial/not-found result is not a successful watch update.
    let missed = result["not_found"].as_object().is_some_and(|o| {
        o.values()
            .any(|v| v.as_array().is_some_and(|a| !a.is_empty()))
    });
    let field = if q.category == "movie" {
        "movies"
    } else {
        "episodes"
    };
    let added = result["added"][field].as_u64().unwrap_or(0);
    if missed || added != 1 {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "Simkl did not confirm the watch update".into(),
        ));
    }
    Ok(axum::Json(
        json!({"recorded":true,"category":q.category,"id":q.id,"season":q.season,"episode":q.episode}),
    ))
}
