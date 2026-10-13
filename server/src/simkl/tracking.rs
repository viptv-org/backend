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
        let movie =
            parts.len() == 3 && parts[0] == "simkl" && matches!(parts[1], "movie" | "anime");
        let tv = parts.len() == 5 && parts[0] == "simkl" && matches!(parts[1], "tv" | "anime");
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
                .is_some_and(|s| s != format!("simkl:{}:{}", q.category, q.id))
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
) -> Result<viptv_engine::presentation::Card, ApiError> {
    if q.id == 0
        || !matches!(q.category.as_str(), "movie" | "tv" | "anime")
        || (q.category == "movie" && (q.season.is_some() || q.episode.is_some()))
        || (q.category == "tv" && q.season.is_none())
        || (q.season.is_some() != q.episode.is_some())
        || q.episode == Some(0)
    {
        return Err("An exact movie or TV episode is required".into());
    }
    let parent = metadata::title(app, profile, credential, &q.category, q.id).await?;
    if parent.identity.kind
        != if q.episode.is_some() {
            "series"
        } else {
            "movie"
        }
    {
        return Err("Conflicting Simkl content type".into());
    }
    let selected = if q.episode.is_some() {
        metadata::episodes(app, profile, credential, q.id, &parent)
            .await?
            .into_iter()
            .find(|card| {
                card.identity
                    .episode
                    .as_ref()
                    .is_some_and(|e| e.season == q.season && Some(e.episode) == q.episode)
            })
            .ok_or("Simkl episode could not be verified")?
    } else {
        parent.clone()
    };
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
    )?;
    Ok(selected)
}

pub(super) async fn record(
    app: &App,
    profile: i64,
    q: &Watched,
    credential: &store::Credential,
    watched_at: Option<&str>,
) -> Result<(), ApiError> {
    let verified = verify(app, profile, q, credential).await?;
    let mut body = if q.category == "movie" {
        json!({"movies":[{"ids":{"simkl":q.id}}]})
    } else if q.category == "anime" {
        if q.episode.is_none() {
            json!({"anime":[{"ids":{"simkl":q.id}}]})
        } else {
            let number = anime_number(app, profile, q, credential, &verified).await?;
            json!({"anime":[{"ids":{"simkl":q.id},"seasons":[{"number":1,"episodes":[{"number":number}]}]}]})
        }
    } else {
        json!({"shows":[{"ids":{"simkl":q.id},"seasons":[{"number":q.season,"episodes":[{"number":q.episode}]}]}]})
    };
    if let Some(at) = watched_at {
        if q.category == "movie" {
            body["movies"][0]["watched_at"] = json!(at);
        } else if q.category == "anime" && q.episode.is_none() {
            body["anime"][0]["watched_at"] = json!(at);
        } else {
            let field = if q.category == "anime" {
                "anime"
            } else {
                "shows"
            };
            body[field][0]["seasons"][0]["episodes"][0]["watched_at"] = json!(at);
        }
    }
    let result = store::request(app, profile, credential, "/sync/history", Some(body)).await?;
    let missed = result["not_found"].as_object().is_some_and(|o| {
        o.values()
            .any(|v| v.as_array().is_some_and(|a| !a.is_empty()))
    });
    let field = if q.category == "movie" {
        "movies"
    } else if q.category == "anime" && q.episode.is_none() {
        "shows"
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

/// Translate only an exact verified provider episode ID. UI coordinates and
/// list position cannot stand in for AniDB's sequential episode number.
pub(super) async fn anime_number(
    app: &App,
    profile: i64,
    q: &Watched,
    credential: &store::Credential,
    verified: &viptv_engine::presentation::Card,
) -> Result<u32, ApiError> {
    let episode_id = verified
        .identity
        .ids
        .iter()
        .find(|id| id.namespace == "simkl_episode")
        .and_then(|id| id.value.parse::<u64>().ok())
        .ok_or("Simkl anime episode mapping is unavailable")?;
    let value = cache::metadata(
        app,
        profile,
        credential,
        &format!("/anime/episodes/{}", q.id),
    )
    .await?;
    let mut matches = value
        .as_array()
        .ok_or("Simkl anime episode mapping is unavailable")?
        .iter()
        .filter(|v| viptv_engine::simkl::id(v) == Some(episode_id));
    let episode = matches
        .next()
        .ok_or("Simkl anime episode mapping is unavailable")?;
    if matches.next().is_some()
        || episode["type"] != "episode"
        || episode["tvdb"]["season"].as_u64() != q.season.map(u64::from)
        || episode["tvdb"]["episode"].as_u64() != q.episode.map(u64::from)
    {
        return Err("Simkl anime episode mapping is unavailable".into());
    }
    let number = episode["episode"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or("Simkl anime episode mapping is unavailable")?;
    store::validate(app, profile, credential)?;
    Ok(number)
}
