//! Resolve a Play intent on the server. Renderers never choose a series episode
//! or derive resume policy from a locally cached episode/history list.
use super::*;
use viptv_engine::{
    continuation::ContinuationStatus,
    presentation::{Card, DetailSnapshot},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    card: Card,
    #[serde(default)]
    restart: bool,
}

pub(crate) async fn resolve(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(request): axum::Json<Request>,
) -> ApiResult {
    request
        .card
        .validate_library()
        .map_err(|_| ApiError::from("Invalid title selection"))?;
    let scoped = app.clone().with_lease(lease.clone());
    {
        let db = scoped
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        scoped.require_profile(&db, profile)?;
        lease.validate(&db)?;
    }
    let selected = &request.card;
    let identity = selected
        .parent_identity
        .as_ref()
        .unwrap_or(&selected.identity);
    let mut expectation = selected.clone();
    expectation.identity = identity.clone();
    expectation.parent_identity = None;
    let addon = selected
        .provider_id
        .strip_prefix("addon:")
        .and_then(|s| s.parse::<i64>().ok());
    let make_page = |offset| -> Result<catalog::DetailPage, ApiError> {
        Ok(catalog::DetailPage {
            offset,
            limit: Some(128),
            provider_id: if selected.metadata_origin.is_some() {
                None
            } else if addon.is_some() {
                Some("stremio".into())
            } else if matches!(selected.provider_id.as_str(), "simkl" | "stremio") {
                Some(selected.provider_id.clone())
            } else {
                None
            },
            addon_id: addon,
            metadata_origin: selected
                .metadata_origin
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|_| ApiError::from("Invalid metadata origin"))?,
            card: selected
                .metadata_origin
                .as_ref()
                .map(|_| serde_json::to_string(&expectation))
                .transpose()
                .map_err(|_| ApiError::from("Invalid title selection"))?,
            ..Default::default()
        })
    };
    let value = catalog::detail(
        State(app.clone()),
        Extension(lease.clone()),
        Path((identity.kind.clone(), identity.id.clone())),
        axum::extract::Query(make_page(0)?),
    )
    .await?
    .0;
    let detail: DetailSnapshot =
        serde_json::from_value(value).map_err(|_| ApiError::from("Metadata unavailable"))?;
    let mut target = if selected.identity.episode.is_some() {
        let mut window = detail.clone();
        loop {
            if let Some(episode) = window.episodes.iter().find(|card| {
                card.identity == selected.identity
                    && card.parent_identity == selected.parent_identity
            }) {
                break episode.clone();
            }
            let offset: usize = window
                .next_cursor
                .as_ref()
                .and_then(|cursor| cursor.parse().ok())
                .ok_or("This episode is unavailable")?;
            let value = catalog::detail(
                State(app.clone()),
                Extension(lease.clone()),
                Path((identity.kind.clone(), identity.id.clone())),
                axum::extract::Query(make_page(offset)?),
            )
            .await?
            .0;
            window = serde_json::from_value(value)
                .map_err(|_| ApiError::from("Metadata unavailable"))?;
        }
    } else {
        detail.item.clone()
    };
    let mut resume = false;
    let mut progress = None;
    let mut remaining_label = None;
    if !request.restart {
        let mut offset = 0;
        loop {
            let page = continuation::snapshot(&scoped, &lease, profile, offset, 100)?;
            if let Some(entry) = page.entries.iter().find(|entry| {
                entry.card.metadata_origin == selected.metadata_origin
                    && (entry.card.identity == selected.identity
                        || (selected.identity.episode.is_none()
                            && entry.card.parent_identity.as_ref() == Some(&selected.identity)))
                    && matches!(
                        entry.status,
                        ContinuationStatus::Resume | ContinuationStatus::Next
                    )
            }) {
                target = entry.card.clone();
                resume = entry.status == ContinuationStatus::Resume;
                if resume && entry.record.duration_millis > 0 {
                    progress = Some(
                        (entry.record.position_millis as f64 / entry.record.duration_millis as f64)
                            .clamp(0.0, 1.0),
                    );
                    let remaining = entry
                        .record
                        .duration_millis
                        .saturating_sub(entry.record.position_millis)
                        .div_ceil(60_000);
                    remaining_label = Some(format!("{remaining}m left"));
                }
                break;
            }
            if page.complete {
                break;
            }
            offset = page
                .next_cursor
                .and_then(|s| s.parse().ok())
                .ok_or("Invalid continuation cursor")?;
        }
    }
    if target.identity.kind == "series" && target.identity.episode.is_none() {
        target = detail
            .episodes
            .first()
            .cloned()
            .ok_or("No episodes are available for this title")?;
    }
    let preferences = preferences::get(State(app.clone()), Extension(lease.clone()), Path(profile))
        .await?
        .0;
    let verb = if resume { "Resume" } else { "Play" };
    let label = match &target.identity.episode {
        Some(episode) => match episode.season {
            Some(season) => format!("{verb} S{season} E{}", episode.episode),
            None => format!("{verb} E{}", episode.episode),
        },
        None => verb.to_owned(),
    };
    let mut value = json!({"card":target,"label":label,"resume":resume,"progress":progress,"remaining_label":remaining_label,"source_selection":preferences["preferences"]["source_selection"]});
    viewing_presentation::for_profile(&app, &lease, profile, &mut value)?;
    Ok(axum::Json(value))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AddonActivation {
    connection_id: String,
    enabled: bool,
}
/// Translate the viewer's enable/disable intent into the canonical assignment
/// update on the server, including the connection's supported capabilities.
pub(crate) async fn set_addon(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(input): axum::Json<AddonActivation>,
) -> ApiResult {
    if !input.connection_id.starts_with("addon:") || input.connection_id.len() > 128 {
        return Err("Invalid add-on selection".into());
    }
    let value = configuration::get(State(app.clone()), Extension(lease.clone()), Path(profile))
        .await?
        .0;
    let connection = value["connections"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == input.connection_id))
        .ok_or("This add-on is unavailable")?;
    let mut assignments = value["configuration"]["assignments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assignments.retain(|row| row["connection_id"] != input.connection_id);
    if input.enabled {
        assignments.push(
            json!({"connection_id":input.connection_id,"enabled":connection["capabilities"]}),
        );
    }
    let update = serde_json::from_value(json!({"revision":value["configuration"]["scope"]["revision"],"browsing_plugin":value["configuration"]["browsing_plugin"],"assignments":assignments})).map_err(|_|ApiError::from("Invalid integration configuration"))?;
    configuration::update(
        State(app),
        Extension(lease),
        Path(profile),
        axum::Json(update),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth_integration_tests::fixture, test_support::request};
    #[tokio::test]
    async fn action_rejects_cross_profile_and_invalid_identity() {
        let app = fixture();
        let card = json!({"identity":{"kind":"movie","id":"tt123","ids":[]},"provider_id":"stremio","title":"Title","badges":[]});
        let (status, _) = request(
            &app,
            "member-token-1",
            "POST",
            "/api/profiles/2/viewing-action",
            json!({"card":card}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status,_)=request(&app,"member-token-1","POST","/api/profiles/1/viewing-action",json!({"card":{"identity":{"kind":"movie","id":"","ids":[]},"provider_id":"stremio","title":"Title","badges":[]}})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    #[tokio::test]
    async fn server_selects_first_episode_and_does_not_trust_client_title() {
        let mock=Router::new().route("/meta/series/parent.json",get(||async{axum::Json(json!({"meta":{"id":"parent","type":"series","name":"Server title","videos":[{"id":"episode-1","season":1,"episode":1,"title":"First"},{"id":"episode-2","season":1,"episode":2,"title":"Second"}]}}))}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
        let mut app = fixture();
        app.addons.allow_test_loopback = true;
        app.db.lock().unwrap().execute("INSERT INTO addons(name,manifest_url,manifest,account_id) VALUES('Metadata',?1,?2,1)",params![format!("http://{address}/manifest.json"),json!({"id":"fixture","name":"Metadata","version":"1","resources":["meta"],"types":["series"]}).to_string()]).unwrap();
        crate::test_support::encrypt_fixture_sources(&app);
        let (status,result)=request(&app,"member-token-1","POST","/api/profiles/1/viewing-action",json!({"card":{"identity":{"kind":"series","id":"parent","ids":[]},"provider_id":"stremio","title":"Untrusted label","badges":[]},"restart":true})).await;
        task.abort();
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["card"]["identity"]["id"], "episode-1");
        assert_eq!(result["resume"], false);
        assert_eq!(result["label"], "Play S1 E1");
        assert_ne!(result["card"]["title"], "Untrusted label");
    }
    #[tokio::test]
    async fn addon_activation_rejects_foreign_or_unknown_connections() {
        let app = fixture();
        for id in ["addon:99999", "m3u:1"] {
            let (status, _) = request(
                &app,
                "member-token-1",
                "PUT",
                "/api/profiles/1/addon-activation",
                json!({"connection_id":id,"enabled":true}),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
    }
}

/// Profile activation facts over the existing server configuration. Inventory
/// remains account-owned; clients do not reconstruct assignment semantics.
pub(crate) async fn addon_states(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let value = configuration::get(State(app), Extension(lease), Path(profile))
        .await?
        .0;
    let assignments = value["configuration"]["assignments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let addons = value["connections"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|connection| connection["plugin_id"] == "stremio")
        .map(|connection| {
            let enabled = assignments
                .iter()
                .find(|assignment| assignment["connection_id"] == connection["id"])
                .and_then(|assignment| assignment["enabled"].as_array())
                .is_some_and(|capabilities| !capabilities.is_empty());
            json!({"connection_id":connection["id"],"enabled":enabled})
        })
        .collect::<Vec<_>>();
    Ok(axum::Json(json!({"addons":addons})))
}

#[cfg(test)]
mod activation_projection_tests {
    use super::*;
    use crate::{auth_integration_tests::fixture, test_support::request};
    #[tokio::test]
    async fn activation_projection_reports_confirmed_profile_state_without_credentials() {
        let app = fixture();
        app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id) VALUES(101,'Source',?1,?2,1)",params!["https://addons.example/manifest.json",json!({"id":"fixture","name":"Source","version":"1","resources":["stream"],"types":["movie"]}).to_string()]).unwrap();
        crate::test_support::encrypt_fixture_sources(&app);
        let path = "/api/profiles/1/addon-activation";
        let (status, initial) = request(&app, "member-token-1", "GET", path, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{initial}");
        assert_eq!(initial["addons"][0]["connection_id"], "addon:101");
        assert_eq!(initial["addons"][0]["enabled"], true);
        assert!(!initial.to_string().contains("addons.example"));
        let (status, receipt) = request(
            &app,
            "member-token-1",
            "PUT",
            path,
            json!({"connection_id":"addon:101","enabled":false}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{receipt}");
        let (_, updated) = request(&app, "member-token-1", "GET", path, Value::Null).await;
        assert_eq!(updated["addons"][0]["enabled"], false);
        assert_eq!(
            request(&app, "member-token-2", "GET", path, Value::Null)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
}
