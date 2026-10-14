//! Activities-gated private reads. Delta merges never publish private memos.
use super::*;
#[derive(Clone)]
pub(in crate::simkl) struct Snapshot {
    pub(in crate::simkl) checked: Instant,
    pub(in crate::simkl) activities: Value,
    pub items: Arc<Value>,
    pub(in crate::simkl) metadata: Arc<tokio::sync::Mutex<enrichment::State>>,
}
fn sections(value: &Value) -> Result<(), ApiError> {
    if !value.is_object()
        || ["movies", "shows", "anime"].iter().any(|key| {
            value
                .get(*key)
                .is_some_and(|v| !v.is_null() && !v.is_array())
        })
    {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "Watchlist data was invalid".into(),
        ));
    }
    Ok(())
}
fn id(item: &Value, key: &str) -> Option<u64> {
    item[if key == "movies" { "movie" } else { "show" }]["ids"]["simkl"].as_u64()
}
fn clean(value: &mut Value) {
    for key in ["movies", "shows", "anime"] {
        for item in value[key].as_array_mut().into_iter().flatten() {
            if let Some(object) = item.as_object_mut() {
                object.remove("memo");
            }
        }
    }
}
fn merge(current: &mut Value, delta: Value, ratings_only: bool) -> Result<(), ApiError> {
    sections(&delta)?;
    for key in ["movies", "shows", "anime"] {
        let mut records = current[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| id(item, key).map(|id| (id, item.clone())))
            .collect::<std::collections::BTreeMap<_, _>>();
        for item in delta[key].as_array().into_iter().flatten() {
            let Some(id) = id(item, key) else { continue };
            if ratings_only {
                if let Some(existing) = records.get_mut(&id).and_then(Value::as_object_mut) {
                    for field in ["user_rating", "user_rated_at"] {
                        if let Some(value) = item.get(field) {
                            existing.insert(field.into(), value.clone());
                        }
                    }
                }
            } else {
                records.insert(id, item.clone());
            }
        }
        current[key] = Value::Array(records.into_values().collect());
    }
    projection::entries(current)?;
    clean(current);
    Ok(())
}
fn changed(current: &Value, previous: &Value, field: &str) -> bool {
    ["movies", "tv_shows", "anime"]
        .iter()
        .any(|key| current[*key][field] != previous[*key][field])
}
pub(super) async fn read(
    app: &App,
    profile: i64,
    access: &store::Credential,
    previous: Option<&Snapshot>,
) -> Result<Snapshot, ApiError> {
    let activities = store::private_read(app, profile, access, "/sync/activities").await?;
    if !activities.get("all").is_some_and(|v| {
        v.is_null()
            || v.as_str()
                .is_some_and(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok())
    }) {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "Watchlist activities were invalid".into(),
        ));
    }
    let mut items = if let Some(previous) = previous {
        if activities == previous.activities {
            return Ok(Snapshot {
                checked: Instant::now(),
                activities,
                items: previous.items.clone(),
                metadata: previous.metadata.clone(),
            });
        }
        let mut items = (*previous.items).clone();
        if let Some(from) = previous.activities["all"].as_str() {
            if [
                "all",
                "watching",
                "plantowatch",
                "hold",
                "completed",
                "dropped",
            ]
            .iter()
            .any(|field| changed(&activities, &previous.activities, field))
            {
                let encoded =
                    url::form_urlencoded::byte_serialize(from.as_bytes()).collect::<String>();
                let delta = store::private_read(
                    app,
                    profile,
                    access,
                    &format!("/sync/all-items?date_from={encoded}&next_watch_info=yes&extended=full&include_all_episodes=original&episode_watched_at=yes&episode_runtime=yes&episode_tvdb_id=yes"),
                )
                .await?;
                merge(&mut items, delta, false)?;
            }
        } else {
            items =
                store::private_read(app, profile, access, "/sync/all-items?next_watch_info=yes&extended=full&include_all_episodes=original&episode_watched_at=yes&episode_runtime=yes&episode_tvdb_id=yes")
                    .await?;
        }
        if changed(&activities, &previous.activities, "rated_at") {
            merge(
                &mut items,
                store::private_read(app, profile, access, "/sync/ratings").await?,
                true,
            )?;
        }
        if changed(&activities, &previous.activities, "removed_from_list") {
            let ids = store::private_read(
                app,
                profile,
                access,
                "/sync/all-items?extended=simkl_ids_only",
            )
            .await?;
            sections(&ids)?;
            for key in ["movies", "shows", "anime"] {
                let keep = ids[key]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|item| id(item, key))
                    .collect::<std::collections::HashSet<_>>();
                if let Some(records) = items[key].as_array_mut() {
                    records.retain(|item| id(item, key).is_some_and(|id| keep.contains(&id)));
                }
            }
        }
        items
    } else {
        store::private_read(app, profile, access, "/sync/all-items?next_watch_info=yes&extended=full&include_all_episodes=original&episode_watched_at=yes&episode_runtime=yes&episode_tvdb_id=yes").await?
    };
    sections(&items)?;
    clean(&mut items);
    projection::entries(&items)?;
    Ok(Snapshot {
        checked: Instant::now(),
        activities,
        items: Arc::new(items),
        metadata: previous.map(|p| p.metadata.clone()).unwrap_or_default(),
    })
}
