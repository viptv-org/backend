//! Cached configuration facts; startup never probes a subtitle provider.
use super::*;
pub(super) fn subtitles(app: &App, lease: &ResourceLease, profile: i64) -> Result<Value, ApiError> {
    let scoped = app.clone().with_lease(lease.clone());
    let (mut configuration, connections) = configuration::read(&scoped, lease, profile)?;
    // Match execution: removed connections cannot revive a stale assignment.
    configuration
        .assignments
        .retain(|a| connections.iter().any(|c| c.id == a.connection_id));
    let providers = viptv_engine::registry::Registry::builtin()
        .connections_for(
            &configuration,
            &connections,
            viptv_engine::capabilities::Capability::Subtitles,
        )
        .map_err(|_| ApiError::from("Subtitle configuration unavailable"))?;
    configuration::validate_revision(&scoped, profile, configuration.scope.revision)?;
    Ok(json!({"available":!providers.is_empty(),"provider_count":providers.len()}))
}

/// Shared viewing navigation, projected from cached configuration only. A
/// configured provider's temporary network failure remains a retryable screen;
/// startup does not probe providers to decide whether their route exists.
pub(super) fn navigation(app: &App, lease: &ResourceLease, profile: Option<i64>) -> Result<Value, ApiError> {
    use viptv_engine::capabilities::Capability;
    let Some(profile) = profile else {
        return Ok(json!([{"id":"profiles","label":"Profile"}]));
    };
    let scoped = app.clone().with_lease(lease.clone());
    let (mut configuration, connections) = configuration::read(&scoped, lease, profile)?;
    configuration.assignments.retain(|a| connections.iter().any(|c| c.id == a.connection_id));
    let registry = viptv_engine::registry::Registry::builtin();
    let enabled = |capability| registry.connections_for(&configuration, &connections, capability)
        .map(|connections| !connections.is_empty())
        .map_err(|_| ApiError::from("Viewing configuration unavailable"));
    let (restricted, live_allowed, simkl_linked) = {
        let db = scoped.db.lock().map_err(|_| ApiError::from("Database unavailable"))?;
        lease.validate(&db)?;
        let restricted = kids::restricted(&db, &lease.principal)?;
        let live_allowed = !restricted || kids::unlocked(&db, &lease.principal)?;
        let linked: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM simkl_links WHERE profile_id=?1 AND account_id=?2 AND credential IS NOT NULL)",
            params![profile, lease.principal.account_id()], |row| row.get(0),
        ).map_err(db_error)?;
        (restricted, live_allowed, linked)
    };
    let (movies, series) = if restricted {
        // Restricted discovery is the scoped approved Library projection.
        (true, true)
    } else if configuration.browsing_plugin == "simkl" {
        let discovery = enabled(Capability::Discovery)?;
        (discovery, discovery)
    } else {
        let (catalog_scope, _) = configuration::apply(scoped.clone(), lease, profile, Capability::Discovery)?;
        let catalogs = catalog_scope.addons.catalogs()?;
        let has = |kind: &str| catalogs.as_array().into_iter().flatten().any(|catalog| catalog["type"] == kind);
        (has("movie"), has("series"))
    };
    let search = restricted || (enabled(Capability::Search)?
        && (configuration.browsing_plugin != "simkl" || simkl_linked));
    let live = live_allowed && enabled(Capability::LiveTv)?;
    let calendar = !restricted && configuration.browsing_plugin == "simkl" && enabled(Capability::Discovery)?;
    configuration::validate_revision(&scoped, profile, configuration.scope.revision)?;
    let navigation = [
        ("search", "Search", search),
        ("home", "Home", true),
        ("movie", "Movies", movies),
        ("series", "Series", series),
        ("live", "Live", live),
        ("calendar", "Calendar", calendar),
        ("library", "My List", true),
        ("profiles", "Profile", true),
    ].into_iter().filter(|(_,_,visible)| *visible)
        .map(|(id,label,_)| json!({"id":id,"label":label})).collect::<Vec<_>>();
    Ok(json!(navigation))
}
