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
