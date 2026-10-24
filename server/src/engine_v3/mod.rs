use crate::*;
use axum::extract::OriginalUri;
use viptv_engine::{
    capabilities::{builtins, ExecutionMode},
    profile::Scope,
};
pub(crate) mod account_devices;
mod bootstrap_facts;
#[cfg(test)]
mod bootstrap_facts_tests;
mod catalog;
mod controls;
pub(crate) mod live;
pub(crate) mod search;
pub(crate) mod source_integrations;
pub(crate) use controls::get as discovery_controls;
pub(crate) mod configuration;
pub(crate) mod continuation;
pub(crate) mod history;
mod home;
pub(crate) mod library;
pub(crate) mod library_stats;
#[cfg(test)]
mod library_tests;
pub(crate) mod preferences;
#[cfg(test)]
mod profile_tests;
pub(crate) mod profiles;
pub(crate) mod revisions;
#[cfg(test)]
mod revisions_tests;
mod projection;
pub(crate) mod viewing_action;
pub(crate) mod viewing_presentation;
pub(crate) mod origins;
pub(crate) mod scoped_library;
#[cfg(test)] mod origin_tests;
mod simkl_catalog;
pub(crate) mod watched;
#[cfg(test)]
mod watched_tests;
pub(crate) mod source_ranking;
pub(crate) mod subtitles;
pub(crate) use catalog::{detail, discover};
#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod source_integration_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod remote_tv_acceptance;

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
    // Read change tokens before Home is built: a write committing during the
    // build then shows up as a change on the client's next revision check.
    let revisions = profile_id
        .map(|profile| revisions::snapshot(&app.clone().with_lease(lease.clone()), profile))
        .transpose()?;
    let initial = if let Some(profile) = profile_id {
        Some(home::load(app.clone(), lease.clone(), profile).await?)
    } else {
        None
    };
    let bootstrap_library = if let Some(profile) = profile_id {
        Some(
            library::list(
                State(app.clone()),
                Extension(lease.clone()),
                Path(profile),
                axum::extract::Query(library::Page {
                    offset: 0,
                    limit: 100,
                    ..Default::default()
                }),
            )
            .await?
            .0,
        )
    } else {
        None
    };
    let membership = profile_id
        .map(|profile| {
            library::membership::snapshot(&app.clone().with_lease(lease.clone()), &lease, profile)
        })
        .transpose()?;
    let continuation = profile_id
        .map(|profile| {
            continuation::snapshot(
                &app.clone().with_lease(lease.clone()),
                &lease,
                profile,
                0,
                12,
            )
        })
        .transpose()?;
    let subtitle_availability = profile_id
        .map(|profile| bootstrap_facts::subtitles(&app, &lease, profile))
        .transpose()?;
    let navigation = bootstrap_facts::navigation(&app, &lease, profile_id)?;
    let bootstrap_preferences = if let Some(profile) = profile_id {
        Some(
            preferences::get(State(app.clone()), Extension(lease.clone()), Path(profile))
                .await?
                .0,
        )
    } else {
        None
    };
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
    if cfg!(feature = "vsources-runtime") && app.secret_vault.is_some() {
        if let Some(plugin) = builtins().into_iter().find(|p| p.id == "vsources") {
            integrations.push(plugin);
        }
    }
    if cfg!(feature = "live-runtime") && app.secret_vault.is_some() {
        if let Some(plugin) = builtins().into_iter().find(|p| p.id == "m3u") {
            integrations.push(plugin);
        }
    }
    let import = profile_id
        .and_then(|profile| {
            crate::simkl::imports::schedule(&app.clone().with_lease(lease.clone()), &lease, profile)
        })
        .map(|s| s.json());
    let axum::Json(management) = profiles::list(State(app), Extension(lease)).await?;
    let mut value=json!({"contract_version":3,"identity":identity,"integrations":integrations,"home":initial.as_ref().map(|home| &home.snapshot),"profile_management":management,"library":bootstrap_library,"preferences":bootstrap_preferences,"library_import":import,"library_membership":membership,"continuation":continuation,"subtitle_availability":subtitle_availability,"playback_capabilities":{"required_audio_index":true},"revisions":revisions});
    value["navigation"] = navigation;
    if let Some(membership)=membership.as_ref(){
        // The bootstrap already loaded the profile's preferences snapshot;
        // its display section carries the ratings visibility decision.
        let show_ratings = bootstrap_preferences
            .as_ref()
            .and_then(|snapshot| snapshot["display"]["show_ratings"].as_bool())
            .unwrap_or(true);
        viewing_presentation::decorate(&mut value,membership,show_ratings)?;
    }
    if let Some(home)=&initial { home.attach(&mut value["home"]); }
    Ok(axum::Json(value))
}

pub(crate) async fn home(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
) -> ApiResult {
    let home=home::load(app.clone(),lease.clone(),profile).await?;
    let mut value=serde_json::to_value(&home.snapshot).map_err(|_|ApiError::from("Home unavailable"))?;
    viewing_presentation::for_profile(&app,&lease,profile,&mut value)?;
    home.attach(&mut value);
    Ok(axum::Json(value))
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

#[cfg(test)]
mod episode_page_tests;

mod episode_progress;
#[cfg(test)]
mod episode_progress_tests;

#[cfg(test)]
mod discovery_choice_tests;
