use crate::*;
use axum::extract::OriginalUri;
use viptv_engine::{
    capabilities::{builtins, ExecutionMode},
    profile::Scope,
};
mod catalog;
pub(crate) mod live;
pub(crate) mod source_integrations;
mod controls;
pub(crate) use controls::get as discovery_controls;
pub(crate) mod configuration;
pub(crate) mod history;
mod home;
pub(crate) mod library;
#[cfg(test)]
mod library_tests;
mod projection;
pub(crate) mod profiles;
pub(crate) mod preferences;
pub(crate) mod source_ranking;
#[cfg(test)]
mod profile_tests;
mod simkl_catalog;
pub(crate) mod subtitles;
pub(crate) use catalog::{detail, discover};
#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod source_integration_tests;

pub(crate) async fn bootstrap(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
) -> ApiResult {
    let response = auth::info(
        State(app.clone()),
        OriginalUri("/api/auth/me".parse().unwrap()),
        Extension(lease.principal.clone()),
    )
    .await?;
    let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .map_err(|_| ApiError::from("Identity unavailable"))?;
    let identity: Value =
        serde_json::from_slice(&bytes).map_err(|_| ApiError::from("Identity unavailable"))?;
    let auth::Principal::Account { profile_id, .. } = lease.principal;
    let initial = if let Some(profile) = profile_id {
        Some(home::load(app.clone(), lease.clone(), profile).await?)
    } else {
        None
    };
    let bootstrap_library=if let Some(profile)=profile_id{Some(library::list(State(app.clone()),Extension(lease.clone()),Path(profile),axum::extract::Query(library::Page{offset:0,limit:Some(100)})).await?.0)}else{None};
    let bootstrap_preferences=if let Some(profile)=profile_id{Some(preferences::get(State(app.clone()),Extension(lease.clone()),Path(profile)).await?.0)}else{None};
    lease.validate(
        &*app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?,
    )?;
    // Only implemented integrations are advertised, even though the engine registry has more descriptors.
    let mut integrations: Vec<_> = builtins()
        .into_iter()
        .filter(|p| {
            p.locations.contains(&ExecutionMode::Server)
                && matches!(p.id.as_str(), "stremio" | "xtream")
        })
        .collect();
    if app.simkl.configured() && app.secret_vault.is_some() {
        if let Some(mut simkl) = builtins().into_iter().find(|p| p.id == "simkl") {
            simkl.capabilities = crate::simkl::capabilities();
            integrations.push(simkl);
        }
    }
    if cfg!(feature="vsources-runtime") && app.secret_vault.is_some(){if let Some(plugin)=builtins().into_iter().find(|p|p.id=="vsources"){integrations.push(plugin);}}
    let axum::Json(management)=profiles::list(State(app),Extension(lease)).await?;
    Ok(axum::Json(json!({"contract_version":3,"identity":identity,"integrations":integrations,"home":initial,"profile_management":management,"library":bootstrap_library,"preferences":bootstrap_preferences})))
}

pub(crate) async fn home(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    Ok(axum::Json(
        serde_json::to_value(home::load(app, lease, profile).await?)
            .map_err(|_| ApiError::from("Home unavailable"))?,
    ))
}

fn scope(lease: &ResourceLease, profile: i64, revision: u64) -> Scope {
    Scope {
        connection_id: "server".into(),
        account_id: lease.principal.account_id().unwrap_or_default().to_string(),
        profile_id: profile.to_string(),
        revision,
    }
}

#[cfg(test)]
mod source_ranking_tests;

#[cfg(test)]
mod live_tests;
