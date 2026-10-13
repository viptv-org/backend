use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Watched {
    pub(super) category: String,
    pub(super) id: u64,
    pub(super) season: Option<u32>,
    pub(super) episode: Option<u32>,
}

impl Watched {
    pub(super) fn canonical(value: &Value) -> Result<Self, ApiError> {
        let id = text(value, "id", 512)?;
        let parts: Vec<_> = id.split(':').collect();
        let movie = parts.len() == 3 && parts[0] == "simkl" && parts[1] == "movie";
        let tv = parts.len() == 5 && parts[0] == "simkl" && parts[1] == "tv";
        if !movie && !tv {
            return Err("An exact Simkl movie or TV episode is required".into());
        }
        let q = Self {
            category: parts[1].into(),
            id: parts[2]
                .parse()
                .map_err(|_| ApiError::from("Invalid Simkl title"))?,
            season: if tv {
                Some(
                    parts[3]
                        .parse()
                        .map_err(|_| ApiError::from("Invalid Simkl season"))?,
                )
            } else {
                None
            },
            episode: if tv {
                Some(
                    parts[4]
                        .parse()
                        .map_err(|_| ApiError::from("Invalid Simkl episode"))?,
                )
            } else {
                None
            },
        };
        if value["type"].as_str() != Some(if movie { "movie" } else { "series" }) {
            return Err("Conflicting Simkl content type".into());
        }
        for (key, coordinate) in [("season", q.season), ("episode", q.episode)] {
            if !value[key].is_null() && value[key].as_u64() != coordinate.map(u64::from) {
                return Err("Conflicting Simkl episode coordinates".into());
            }
        }
        if tv
            && value["series_id"]
                .as_str()
                .is_some_and(|s| s != format!("simkl:tv:{}", q.id))
        {
            return Err("Conflicting Simkl title identity".into());
        }
        Ok(q)
    }
}

pub(super) async fn verify(
    app: &App,
    profile: i64,
    q: &Watched,
    credential: &store::Credential,
) -> Result<(), ApiError> {
    if q.id == 0
        || !matches!(q.category.as_str(), "movie" | "tv")
        || (q.category == "movie" && (q.season.is_some() || q.episode.is_some()))
        || (q.category == "tv" && (q.season.is_none() || q.episode.unwrap_or(0) == 0))
    {
        return Err("An exact movie or TV episode is required".into());
    }
    let parent = metadata::title(app, profile, credential, &q.category, q.id).await?;
    if q.category == "tv"
        && !metadata::episodes(app, profile, credential, q.id, &parent)
            .await?
            .iter()
            .any(|e| {
                e.identity
                    .episode
                    .as_ref()
                    .is_some_and(|e| e.season == q.season && Some(e.episode) == q.episode)
            })
    {
        return Err("Simkl episode could not be verified".into());
    }
    let db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    app.require_profile(&db, profile)?;
    kids::require_item(
        &db,
        &app.identity(),
        &parent.identity.kind,
        &parent.identity.id,
    )
}

pub(super) async fn record(
    app: &App,
    profile: i64,
    q: &Watched,
    credential: &store::Credential,
    watched_at: Option<&str>,
) -> Result<(), ApiError> {
    verify(app, profile, q, credential).await?;
    let mut body = if q.category == "movie" {
        json!({"movies":[{"ids":{"simkl":q.id}}]})
    } else {
        json!({"shows":[{"ids":{"simkl":q.id},"seasons":[{"number":q.season,"episodes":[{"number":q.episode}]}]}]})
    };
    if let Some(at) = watched_at {
        if q.category == "movie" {
            body["movies"][0]["watched_at"] = json!(at);
        } else {
            body["shows"][0]["seasons"][0]["episodes"][0]["watched_at"] = json!(at);
        }
    }
    let result = store::request(app, profile, credential, "/sync/history", Some(body)).await?;
    let missed = result["not_found"].as_object().is_some_and(|o| {
        o.values()
            .any(|v| v.as_array().is_some_and(|a| !a.is_empty()))
    });
    let field = if q.category == "movie" {
        "movies"
    } else {
        "episodes"
    };
    if missed || result["added"][field].as_u64() != Some(1) {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "Simkl did not confirm the watch update".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn watched(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(q): axum::Json<Watched>,
) -> ApiResult {
    let app = app.with_lease(lease);
    let credential = store::credential(&app, profile, Capability::Tracking)?;
    record(&app, profile, &q, &credential, None).await?;
    Ok(axum::Json(
        json!({"recorded":true,"category":q.category,"id":q.id,"season":q.season,"episode":q.episode}),
    ))
}
