//! Generic source configuration; credential values are write-only host-vault data.
use crate::Error;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum IntegrationState {
    NeedsConfiguration,
    Ready,
    Unavailable,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CredentialField {
    pub id: String,
    pub label: String,
    pub required: bool,
    pub secret: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderChoice {
    pub id: String,
    pub label: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceIntegrationSchema {
    pub plugin_id: String,
    pub name: String,
    pub state: IntegrationState,
    pub runtime_enabled: bool,
    pub credential_fields: Vec<CredentialField>,
    pub connection_id: Option<String>,
    pub provider_choices: Vec<ProviderChoice>,
    pub maximum_providers: u32,
}
pub fn vsources_schema(configured: bool, connection_id: Option<String>) -> SourceIntegrationSchema {
    let enabled = cfg!(feature = "vsources-runtime");
    SourceIntegrationSchema {
        plugin_id: "vsources".into(),
        name: "VIPTV Sources".into(),
        state: if !enabled {
            IntegrationState::Unavailable
        } else if configured {
            IntegrationState::Ready
        } else {
            IntegrationState::NeedsConfiguration
        },
        runtime_enabled: enabled,
        credential_fields: vec![CredentialField {
            id: "tmdb_api_key".into(),
            label: "TMDB API key".into(),
            required: true,
            secret: true,
        }],
        connection_id,
        provider_choices: provider_choices(),
        maximum_providers: 12,
    }
}
/// Never serialize this into a public response or include it in diagnostics.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VsourcesConfiguration {
    pub tmdb_api_key: String,
    #[serde(default)]
    pub providers: Vec<String>,
}
impl VsourcesConfiguration {
    pub fn validate(&self) -> Result<(), Error> {
        if self.tmdb_api_key.trim().is_empty()
            || self.tmdb_api_key.len() > 512
            || self.tmdb_api_key.chars().any(char::is_control)
            || self.providers.len() > 12
            || self.providers.iter().any(|p| {
                p.is_empty()
                    || p.len() > 64
                    || !p
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
        {
            return Err(Error::InvalidQuery);
        }
        Ok(())
    }
}

fn provider_choices() -> Vec<ProviderChoice> {
    #[cfg(feature = "vsources-runtime")]
    {
        let config = VsourcesConfiguration {
            tmdb_api_key: "schema-only-no-network".into(),
            providers: vec![],
        };
        return crate::vsources_runtime::engine(
            &config,
            std::sync::Arc::new(crate::vsources_runtime::ScopedFetcher::new(
                std::sync::Arc::new(|| false),
            )),
        )
        .map(|engine| {
            engine
                .providers()
                .iter()
                .map(|p| ProviderChoice {
                    id: p.id.to_string(),
                    label: p.label.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    }
    #[cfg(not(feature = "vsources-runtime"))]
    {
        vec![]
    }
}
