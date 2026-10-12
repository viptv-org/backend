use crate::{Error, capabilities::*, profile::*};

#[derive(Default)]
pub struct Registry {
    plugins: Vec<PluginDescriptor>,
}

impl Registry {
    pub fn builtin() -> Self {
        Self {
            plugins: builtins(),
        }
    }
    pub fn plugins(&self, mode: ExecutionMode) -> Vec<&PluginDescriptor> {
        self.plugins
            .iter()
            .filter(|p| p.locations.contains(&mode))
            .collect()
    }
    pub fn validate(
        &self,
        profile: &ProfileConfiguration,
        connections: &[IntegrationConnection],
    ) -> Result<(), Error> {
        let browser = self
            .plugins
            .iter()
            .find(|p| p.id == profile.browsing_plugin)
            .ok_or(Error::Unavailable)?;
        if !browser.locations.contains(&profile.mode)
            || !browser.capabilities.contains(&Capability::Discovery)
        {
            return Err(Error::Unavailable);
        }
        let mut seen = std::collections::HashSet::new();
        for assignment in &profile.assignments {
            if !seen.insert(&assignment.connection_id) {
                return Err(Error::Conflict);
            }
            let connection = connections
                .iter()
                .find(|c| c.id == assignment.connection_id)
                .ok_or(Error::Forbidden)?;
            if connection.account_id != profile.scope.account_id
                || connection
                    .private_profile_id
                    .as_ref()
                    .is_some_and(|p| p != &profile.scope.profile_id)
            {
                return Err(Error::Forbidden);
            }
            let plugin = self
                .plugins
                .iter()
                .find(|p| p.id == connection.plugin_id)
                .ok_or(Error::Unavailable)?;
            if !plugin.locations.contains(&profile.mode) {
                return Err(Error::Unavailable);
            }
            if assignment.enabled.iter().any(|cap| {
                !plugin.capabilities.contains(cap) || !connection.capabilities.contains(cap)
            }) {
                return Err(Error::Unavailable);
            }
        }
        Ok(())
    }
    pub fn connections_for<'a>(
        &self,
        profile: &ProfileConfiguration,
        connections: &'a [IntegrationConnection],
        capability: Capability,
    ) -> Result<Vec<&'a IntegrationConnection>, Error> {
        self.validate(profile, connections)?;
        Ok(connections
            .iter()
            .filter(|c| {
                c.enabled
                    && profile
                        .assignments
                        .iter()
                        .any(|a| a.connection_id == c.id && a.enabled.contains(&capability))
                    && (!matches!(
                        capability,
                        Capability::Discovery | Capability::Search | Capability::Metadata
                    ) || c.plugin_id == profile.browsing_plugin)
            })
            .collect())
    }
}
