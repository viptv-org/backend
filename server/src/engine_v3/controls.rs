//! Advertise scoped provider controls using the shared QueryControl shape.
use super::*;
use viptv_engine::{
    capabilities::{Availability, Capability},
    query::{Choice, ControlKind, QueryControl},
    registry::Registry,
};
#[derive(Deserialize, Default)]
pub(crate) struct ControlsQuery {
    #[serde(rename = "type")]
    kind: Option<String>,
}
fn state(availability: Availability, message: Option<&str>) -> Value {
    json!({"availability":availability,"message":message})
}
fn text(id: &str, label: &str, required: bool) -> QueryControl {
    QueryControl {
        id: id.into(),
        label: label.into(),
        kind: ControlKind::Text,
        required,
        choices: vec![],
        minimum: None,
        maximum: None,
    }
}
pub(crate) async fn get(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Query(q): Query<ControlsQuery>,
) -> ApiResult {
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = lease.principal
    else {
        return Err(auth::forbidden());
    };
    let app = app.with_lease(lease.clone());
    let (mut configuration, connections) = configuration::read(&app, &lease, profile)?;
    configuration
        .assignments
        .retain(|a| connections.iter().any(|c| c.id == a.connection_id));
    let registry = Registry::builtin();
    let discovery = registry
        .connections_for(&configuration, &connections, Capability::Discovery)
        .map_err(|_| ApiError::from("Integration unavailable"))?;
    let search = registry
        .connections_for(&configuration, &connections, Capability::Search)
        .map_err(|_| ApiError::from("Integration unavailable"))?;
    let available = |enabled: bool| {
        if enabled {
            state(Availability::Available, None)
        } else {
            state(
                Availability::NeedsConfiguration,
                Some("Enable this capability for the selected profile"),
            )
        }
    };
    let is_simkl = configuration.browsing_plugin == "simkl";
    let kinds = if is_simkl {
        vec!["movie", "series", "anime"]
    } else {
        vec!["movie", "series"]
    };
    let kind = q.kind.unwrap_or_else(|| "movie".into());
    if !kinds.contains(&kind.as_str()) {
        return Err("Invalid discovery type".into());
    }
    let mut controls = vec![crate::simkl::browse::choice(
        "type",
        "Type",
        kinds
            .iter()
            .map(|kind| Choice {
                value: (*kind).into(),
                label: match *kind {
                    "movie" => "Movies",
                    "series" => "TV Shows",
                    _ => "Anime",
                }
                .into(),
            })
            .collect(),
    )];
    if !search.is_empty() {
        controls.push(text("search", "Search", false));
    }
    let restricted = {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        app.require_profile(&db, profile)?;
        kids::restricted(&db, &app.identity())?
    };
    let mut catalogs = Vec::new();
    let mut filter_state = state(
        Availability::Unsupported,
        Some("No discovery filters are configured"),
    );
    if restricted {
        return Ok(axum::Json(
            json!({"provider_id":"library","media_types":["movie","series"],"controls":controls.into_iter().take(2).collect::<Vec<_>>(),"catalogs":[],"discovery":available(true),"search":available(true),"filters":filter_state,"calendar":state(Availability::Unsupported,Some("Calendar requires approved library observations"))}),
        ));
    }
    if is_simkl && !discovery.is_empty() {
        let category = if kind == "series" {
            "tv"
        } else {
            kind.as_str()
        };
        match tokio::time::timeout(
            Duration::from_secs(4),
            crate::simkl::browse::controls(&app, profile, category),
        )
        .await
        {
            Ok(Ok(extra)) => {
                controls.extend(extra);
                filter_state = state(Availability::Available, None);
            }
            _ => {
                filter_state = state(
                    Availability::TemporarilyUnavailable,
                    Some("Simkl genre controls are unavailable. Try again."),
                )
            }
        }
    } else if !is_simkl {
        let (scoped, _) =
            configuration::apply(app.clone(), &lease, profile, Capability::Discovery)?;
        for catalog in scoped
            .addons
            .catalogs()?
            .as_array()
            .into_iter()
            .flatten()
            .take(100)
        {
            if catalog["type"] != kind {
                continue;
            }
            let mut options = Vec::new();
            if let Some(genres) = catalog["genres"].as_array() {
                let choices = genres
                    .iter()
                    .filter_map(|g| g.as_str())
                    .take(128)
                    .map(|g| Choice {
                        value: g.into(),
                        label: g.into(),
                    })
                    .collect::<Vec<_>>();
                if !choices.is_empty() {
                    options.push(crate::simkl::browse::choice("genre", "Genre", choices));
                }
            }
            for extra in catalog["extra"].as_array().into_iter().flatten().take(16) {
                let Some(id) = extra["name"].as_str().filter(|s| s.len() <= 128) else {
                    continue;
                };
                if matches!(id, "genre" | "search" | "skip") {
                    continue;
                }
                let choices = extra["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str())
                    .take(128)
                    .map(|s| Choice {
                        value: s.into(),
                        label: s.into(),
                    })
                    .collect::<Vec<_>>();
                let control = if choices.is_empty() {
                    text(id, id, extra["isRequired"] == true)
                } else {
                    crate::simkl::browse::choice(id, id, choices)
                };
                options.push(control);
            }
            catalogs.push(json!({"id":catalog["id"],"name":catalog["name"],"type":catalog["type"],"addon_id":catalog["addon_id"],"supports_search":catalog["supports_search"],"supports_skip":catalog["supports_skip"],"controls":options}));
        }
        if !catalogs.is_empty() {
            filter_state = state(Availability::Available, None);
        }
    }
    configuration::validate_revision(&app, profile, configuration.scope.revision)?;
    let search_state = if is_simkl && !search.is_empty() {
        let db = app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?;
        let linked:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM simkl_links WHERE profile_id=?1 AND account_id=?2 AND credential IS NOT NULL)",params![profile,lease.principal.account_id()],|r|r.get(0)).map_err(db_error)?;
        if linked {
            available(true)
        } else {
            state(
                Availability::NeedsAuthorization,
                Some("Link Simkl for authenticated search"),
            )
        }
    } else {
        available(!search.is_empty())
    };
    Ok(axum::Json(
        json!({"provider_id":configuration.browsing_plugin,"media_types":kinds,"controls":controls,"catalogs":catalogs,"discovery":available(!discovery.is_empty()),"search":search_state,"filters":filter_state,"calendar":if is_simkl{available(!discovery.is_empty())}else{state(Availability::Unsupported,Some("Calendar is not available for Stremio addons"))}}),
    ))
}
