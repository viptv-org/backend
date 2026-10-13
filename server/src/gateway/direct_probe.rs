//! Bounded, pinned secure direct-media inspection; failure selects a real gateway.
use crate::{account_api::Error, source_http};
use std::{collections::BTreeMap, future::Future, time::Duration};
use url::Url;

pub(crate) struct Destination {
    pub url: String,
    pub headers: BTreeMap<String, String>,
}
pub(crate) struct Probe {
    slots: tokio::sync::Semaphore,
    /// Generic protocol fixtures have no media origin. Real App::new never sets this.
    #[cfg(test)]
    pub fixture_stub: bool,
}
impl Default for Probe {
    fn default() -> Self {
        Self {
            slots: tokio::sync::Semaphore::new(32),
            #[cfg(test)]
            fixture_stub: false,
        }
    }
}
impl Probe {
    #[cfg(test)]
    pub(crate) fn protocol_fixture() -> Self {
        Self {
            fixture_stub: true,
            ..Self::default()
        }
    }

    pub async fn resolve<F, Fut>(
        &self,
        raw: &str,
        headers: &BTreeMap<String, String>,
        check: F,
    ) -> Result<Option<Destination>, Error>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<(), Error>>,
    {
        check().await?;
        #[cfg(test)]
        if self.fixture_stub {
            return Ok(Some(Destination {
                url: raw.into(),
                headers: headers.clone(),
            }));
        }
        match tokio::time::timeout(Duration::from_secs(8), async {
            let _permit = self
                .slots
                .acquire()
                .await
                .map_err(|_| Error::Code("playback_capacity"))?;
            check().await?;
            inspect(raw, headers, false, &check).await
        })
        .await
        {
            Ok(result) => result,
            Err(_) => {
                check().await?;
                Ok(None)
            }
        }
    }
}

/// A downgrade is classified before any HTTP connection. No URL is rewritten.
fn next(current: &Url, location: &str, fixture: bool) -> Option<Url> {
    if location.len() > 16384 || location.chars().any(char::is_control) {
        return None;
    }
    let target = current.join(location).ok()?;
    if current.scheme() == "https" && target.scheme() != "https" {
        return None;
    }
    source_http::validate(&target, fixture).ok()?;
    Some(target)
}
async fn inspect<F, Fut>(
    raw: &str,
    initial: &BTreeMap<String, String>,
    fixture: bool,
    check: &F,
) -> Result<Option<Destination>, Error>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    let Ok(mut target) = Url::parse(raw) else {
        return Ok(None);
    };
    let mut headers = initial.clone();
    for hop in 0..=4 {
        check().await?;
        if source_http::validate(&target, fixture).is_err()
            || (!fixture && target.scheme() != "https")
        {
            return Ok(None);
        }
        let Some(host) = target.host_str().map(str::to_owned) else {
            return Ok(None);
        };
        let Some(port) = target.port_or_known_default() else {
            return Ok(None);
        };
        let addresses = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::lookup_host((host.as_str(), port)),
        )
        .await;
        check().await?;
        let Ok(Ok(addresses)) = addresses else {
            return Ok(None);
        };
        let addresses = addresses.take(17).collect::<Vec<_>>();
        if addresses.is_empty()
            || addresses.len() > 16
            || addresses
                .iter()
                .any(|a| !source_http::public_ip(a.ip()) && !(fixture && a.ip().is_loopback()))
        {
            return Ok(None);
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .https_only(!fixture)
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(&host, &addresses)
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .build();
        let Ok(client) = client else {
            return Ok(None);
        };
        let mut request = client.get(target.clone());
        for (name, value) in &headers {
            let Ok(mut value) = reqwest::header::HeaderValue::from_str(value) else {
                return Ok(None);
            };
            value.set_sensitive(true);
            request = request.header(name, value);
        }
        check().await?;
        let response = request
            .header(reqwest::header::RANGE, "bytes=0-0")
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .send()
            .await;
        check().await?;
        let Ok(response) = response else {
            return Ok(None);
        };
        if response
            .headers()
            .iter()
            .map(|(n, v)| n.as_str().len() + v.as_bytes().len() + 4)
            .sum::<usize>()
            > 32768
        {
            return Ok(None);
        }
        match response.status().as_u16() {
            200..=299 => {
                return Ok(Some(Destination {
                    url: target.into(),
                    headers,
                }))
            }
            301 | 302 | 303 | 307 | 308 if hop < 4 => {
                let Some(location) = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                else {
                    return Ok(None);
                };
                let Some(destination) = next(&target, location, fixture) else {
                    return Ok(None);
                };
                // All arbitrary addon headers are recipient-bound credentials.
                if destination.origin() != target.origin() {
                    headers.clear();
                }
                target = destination;
            }
            _ => return Ok(None),
        }
    }
    Ok(None)
}

#[cfg(test)]
#[path = "direct_probe_tests.rs"]
mod tests;
