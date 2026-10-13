//! Canonical live channel/guide contracts; private URI resolution stays inside the host.
use crate::{
    Error, capabilities::Availability, presentation::Card, profile::IntegrationConnection,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use url::Url;
mod guide;
mod m3u;
pub use guide::{parse_xmltv, parse_xtream_guide};
pub use m3u::parse_m3u;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LiveGroup {
    pub id: String,
    pub label: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LiveChannel {
    pub id: String,
    pub connection_id: String,
    pub card: Card,
    pub group_id: Option<String>,
    pub group_name: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LivePage {
    pub channels: Vec<LiveChannel>,
    pub groups: Vec<LiveGroup>,
    pub connections: Vec<IntegrationConnection>,
    pub next_cursor: Option<String>,
    pub complete: bool,
    pub availability: Availability,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LiveProgram {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub start_millis: u64,
    pub end_millis: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LiveGuide {
    pub channel_id: String,
    pub programs: Vec<LiveProgram>,
    pub availability: Availability,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveQuery {
    pub connection_id: Option<String>,
    pub group_id: Option<String>,
    pub search: Option<String>,
    #[serde(default)]
    pub offset: u32,
    pub limit: Option<u32>,
}
impl LiveQuery {
    pub fn validate(&self) -> Result<(), Error> {
        if self.offset > 10000
            || self.limit.is_some_and(|v| v == 0 || v > 200)
            || self
                .search
                .as_ref()
                .is_some_and(|v| v.len() > 200 || v.chars().any(char::is_control))
            || self.connection_id.as_ref().is_some_and(|v| v.len() > 128)
            || self.group_id.as_ref().is_some_and(|v| v.len() > 256)
        {
            return Err(Error::InvalidQuery);
        }
        Ok(())
    }
}
/// Vault-only data. Never serialize this into live directory or guide responses.
#[derive(Clone, Serialize, Deserialize)]
pub struct LiveSource {
    pub url: String,
    pub headers: BTreeMap<String, String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct PrivateChannel {
    pub channel: LiveChannel,
    pub epg_id: Option<String>,
    pub stream_id: Option<String>,
    pub sources: Vec<LiveSource>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct M3uConfiguration {
    pub playlist_url: String,
    #[serde(default)]
    pub epg_url: Option<String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct XtreamConfiguration {
    pub server_url: String,
    pub username: String,
    pub password: String,
}
impl M3uConfiguration {
    pub fn validate(&self) -> Result<(), Error> {
        let url = Url::parse(&self.playlist_url).map_err(|_| Error::InvalidQuery)?;
        viptv_provider::validate_url(url.as_str()).map_err(|_| Error::InvalidQuery)?;
        if url.as_str().len() > 16384 {
            return Err(Error::InvalidQuery);
        }
        if let Some(epg) = self.epg_url.as_ref().filter(|s| !s.is_empty()) {
            viptv_provider::validate_url(epg).map_err(|_| Error::InvalidQuery)?;
        }
        Ok(())
    }
}
impl XtreamConfiguration {
    pub fn validate(&self) -> Result<(), Error> {
        viptv_provider::normalize::base_url(&self.server_url).map_err(|_| Error::InvalidQuery)?;
        if [&self.username, &self.password]
            .iter()
            .any(|v| v.is_empty() || v.len() > 256 || v.chars().any(char::is_control))
        {
            return Err(Error::InvalidQuery);
        }
        Ok(())
    }
}
pub fn channel_id(connection: &str, identity: &str) -> String {
    use sha2::Digest;
    format!(
        "live:{:x}",
        sha2::Sha256::digest(format!("{connection}\0{identity}").as_bytes())
    )
}
pub fn live_card(id: String, name: String, logo: Option<String>, provider_id: String) -> Card {
    Card {
        identity: crate::identity::MediaIdentity {
            id,
            kind: "live".into(),
            ids: vec![],
            episode: None,
            edition: None,
        },
        provider_id,
        title: name,
        subtitle: None,
        description: None,
        poster: logo.clone(),
        landscape: logo,
        logo: None,
        progress: None,
        badges: vec![],
        parent_identity: None,
        added_at_millis: None,
        category:Some("live".into()),
        metadata:None,
    }
}
pub fn artwork(raw: Option<&str>) -> Option<String> {
    let url = Url::parse(raw?).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(url.to_string())
}
pub fn xtream_request(
    configuration: &XtreamConfiguration,
    action: &str,
    stream: Option<&str>,
) -> Result<Url, Error> {
    configuration.validate()?;
    if !["get_live_streams", "get_live_categories", "get_short_epg"].contains(&action) {
        return Err(Error::InvalidQuery);
    }
    let mut url = viptv_provider::normalize::endpoint(&configuration.server_url, "player_api.php")
        .map_err(|_| Error::InvalidQuery)?;
    url.query_pairs_mut()
        .append_pair("username", &configuration.username)
        .append_pair("password", &configuration.password)
        .append_pair("action", action);
    if let Some(id) = stream {
        url.query_pairs_mut().append_pair("stream_id", id);
    }
    Ok(url)
}
pub fn parse_xtream(
    raw: &str,
    connection: &str,
    configuration: &XtreamConfiguration,
) -> Result<Vec<PrivateChannel>, Error> {
    let values: Vec<serde_json::Value> =
        serde_json::from_str(raw).map_err(|_| Error::InvalidQuery)?;
    let mut output = Vec::new();
    for value in values.into_iter().take(10000) {
        let Some(stream) = viptv_provider::normalize::scalar(&value["stream_id"]) else {
            continue;
        };
        let Some(name) = value["name"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 512)
        else {
            continue;
        };
        let id = channel_id(connection, &stream);
        let url = viptv_provider::normalize::media_url(
            &configuration.server_url,
            &configuration.username,
            &configuration.password,
            "live",
            &stream,
            "ts",
        )
        .map_err(|_| Error::InvalidQuery)?;
        output.push(PrivateChannel {
            channel: LiveChannel {
                id: id.clone(),
                connection_id: connection.into(),
                card: live_card(
                    id,
                    name.into(),
                    artwork(value["stream_icon"].as_str()),
                    connection.into(),
                ),
                group_id: viptv_provider::normalize::scalar(&value["category_id"]),
                group_name: None,
            },
            epg_id: value["epg_channel_id"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 256)
                .map(str::to_owned),
            stream_id: Some(stream),
            sources: vec![LiveSource {
                url,
                headers: BTreeMap::new(),
            }],
        });
    }
    Ok(output)
}

pub fn xtream_groups(raw:&str)->Result<Vec<LiveGroup>,Error>{let values:Vec<serde_json::Value>=serde_json::from_str(raw).map_err(|_|Error::InvalidQuery)?;Ok(values.into_iter().take(1000).filter_map(|v|Some(LiveGroup{id:viptv_provider::normalize::scalar(&v["category_id"])?,label:v["category_name"].as_str()?.to_owned()})).collect())}
