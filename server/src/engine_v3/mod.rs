use crate::*;
use axum::extract::OriginalUri;
use std::future::Future;
use viptv_engine::{
    capabilities::{builtins, ExecutionMode},
    profile::Scope,
};

async fn timed<T>(
    phase: &'static str,
    work: impl Future<Output = Result<T, ApiError>> + Send,
) -> Result<T, ApiError> {
    let start = Instant::now();
    let result = work.await;
    tracing::debug!(phase, elapsed_ms = start.elapsed().as_millis() as u64, ok = result.is_ok());
    result
}

async fn when_profile<T, F, Fut>(
    phase: &'static str,
    profile: Option<i64>,
    work: F,
) -> Result<Option<T>, ApiError>
where
    F: FnOnce(i64) -> Fut,
    Fut: Future<Output = Result<T, ApiError>> + Send,
{
    match profile {
        Some(profile) => timed(phase, work(profile)).await.map(Some),
        None => Ok(None),
    }
}

pub(crate) mod account_devices;
mod bootstrap_facts;
#[cfg(test)]
mod bootstrap_facts_tests;
mod catalog;
mod controls;
#[cfg(test)]
mod correction_tests;
pub(crate) mod live;
pub(crate) mod search;
#[cfg(test)]
mod search_health_tests;
#[cfg(test)]
mod search_stream_tests;
pub(crate) mod source_integrations;
#[cfg(feature = "vsources-runtime")]
pub(crate) mod preferred_discovery;
pub(crate) use controls::get as discovery_controls;
pub(crate) mod configuration;
pub(crate) mod anime_show;
#[cfg(test)]
mod anime_show_tests;
pub(crate) mod cross_ids;
#[cfg(test)]
mod cross_ids_tests;
pub(crate) mod anime_show_migration;
#[cfg(test)]
mod anime_show_migration_tests;
pub(crate) mod art;
#[cfg(test)]
mod art_tests;
pub(crate) mod continuation;
pub(crate) mod history;
mod home;
mod home_catalogs;
mod home_titles;
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
#[cfg(test)]
mod projection_tests;
mod provider_identity;
#[cfg(test)]
mod provider_identity_tests;
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
pub(crate) mod segments;
#[cfg(test)]
mod segments_tests;
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
    let started = Instant::now();
    let auth::Principal::Account { profile_id, .. } = lease.principal;
    let scoped = app.clone().with_lease(lease.clone());
    let identity = {
        let app = app.clone();
        let principal = lease.principal.clone();
        timed("identity", async move {
            let response = auth::info(
                State(app),
                OriginalUri("/api/auth/me".parse().unwrap()),
                Extension(principal),
            )
            .await?;
            let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
                .await
                .map_err(|_| ApiError::from("Identity unavailable"))?;
            Ok(serde_json::from_slice::<Value>(&bytes)
                .map_err(|_| ApiError::from("Identity unavailable"))?)
        })
    };
    let revisions = {
        let app = scoped.clone();
        when_profile("revisions", profile_id, |profile| {
            blocking(move || revisions::snapshot(&app, profile))
        })
    };
    let initial = {
        let app = app.clone();
        let lease = lease.clone();
        when_profile("home", profile_id, |profile| async move {
            home::load(app, lease, profile).await
        })
    };
    let bootstrap_library = {
        let app = app.clone();
        let lease = lease.clone();
        when_profile("library", profile_id, |profile| async move {
            library::list(
                State(app),
                Extension(lease),
                Path(profile),
                axum::extract::Query(library::Page {
                    offset: 0,
                    limit: 100,
                    ..Default::default()
                }),
            )
            .await
            .map(|json| json.0)
        })
    };
    let membership = {
        let app = scoped.clone();
        let lease = lease.clone();
        when_profile("membership", profile_id, |profile| {
            blocking(move || library::membership::snapshot(&app, &lease, profile))
        })
    };
    let continuation = {
        let app = scoped.clone();
        let lease = lease.clone();
        when_profile("continuation", profile_id, |profile| {
            blocking(move || continuation::snapshot(&app, &lease, profile, 0, 12))
        })
    };
    let subtitle_availability = {
        let app = app.clone();
        let lease = lease.clone();
        when_profile("subtitles", profile_id, |profile| {
            blocking(move || bootstrap_facts::subtitles(&app, &lease, profile))
        })
    };
    let navigation = {
        let app = app.clone();
        let lease = lease.clone();
        timed("navigation", async move {
            blocking(move || bootstrap_facts::navigation(&app, &lease, profile_id)).await
        })
    };
    let bootstrap_preferences = {
        let app = app.clone();
        let lease = lease.clone();
        when_profile("preferences", profile_id, |profile| async move {
            preferences::get(State(app), Extension(lease), Path(profile))
                .await
                .map(|json| json.0)
        })
    };
    let management = {
        let app = app.clone();
        let lease = lease.clone();
        timed("profiles", async move {
            profiles::list(State(app), Extension(lease))
                .await
                .map(|json| json.0)
        })
    };
    let (
        identity,
        revisions,
        initial,
        bootstrap_library,
        membership,
        continuation,
        subtitle_availability,
        navigation,
        bootstrap_preferences,
        management,
    ) = tokio::join!(
        identity,
        revisions,
        initial,
        bootstrap_library,
        membership,
        continuation,
        subtitle_availability,
        navigation,
        bootstrap_preferences,
        management,
    );
    let identity = identity?;
    let revisions = revisions?;
    let initial = initial?;
    let bootstrap_library = bootstrap_library?;
    let membership = membership?;
    let continuation = continuation?;
    let subtitle_availability = subtitle_availability?;
    let navigation = navigation?;
    let bootstrap_preferences = bootstrap_preferences?;
    let management = management?;
    let authorize = Instant::now();
    lease.validate(
        &*app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?,
    )?;
    tracing::debug!(phase = "authorize", elapsed_ms = authorize.elapsed().as_millis() as u64);
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
    let mut value=json!({"contract_version":3,"identity":identity,"capabilities":auth::account_capabilities(&lease.principal),"integrations":integrations,"home":initial.as_ref().map(|home| &home.snapshot),"profile_management":management,"library":bootstrap_library,"preferences":bootstrap_preferences,"library_import":import,"library_membership":membership,"continuation":continuation,"subtitle_availability":subtitle_availability,"playback_capabilities":{"required_audio_index":true},"revisions":revisions});
    value["preferred_source_discovery_supported"] = json!(cfg!(feature = "vsources-runtime"));
    value["navigation"] = navigation;
    if let Some(membership)=membership.as_ref(){
        let show_ratings = bootstrap_preferences
            .as_ref()
            .and_then(|snapshot| snapshot["display"]["show_ratings"].as_bool())
            .unwrap_or(true);
        viewing_presentation::decorate(&mut value,membership,show_ratings)?;
    }
    if let Some(home)=&initial { home.attach(&mut value["home"]); }
    tracing::debug!(phase = "bootstrap", elapsed_ms = started.elapsed().as_millis() as u64);
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
