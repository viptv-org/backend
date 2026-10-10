use super::*;
use crate::addon::credentials_v2;

const MAX_ADDONS: usize = 32;

pub(super) struct SourceAddon {
    pub id: String,
    pub name: String,
    pub resources: Vec<String>,
    pub manifest_id: Option<String>,
    pub url: Option<String>,
}

pub(super) struct VerifiedAddon {
    pub url: String,
    pub manifest: Value,
    pub expected: credentials_v2::Snapshot,
}

fn safe_name(value: &Value) -> String {
    value
        .as_str()
        .filter(|s| {
            !s.trim().is_empty()
                && s.len() <= 256
                && !s.chars().any(char::is_control)
                && !s.contains("://")
                && !s.contains('@')
        })
        .unwrap_or("Unnamed addon")
        .to_owned()
}

pub(super) fn parse_collection(value: Value) -> Result<Vec<SourceAddon>, Error> {
    let descriptors = value
        .get("addons")
        .and_then(Value::as_array)
        .filter(|v| v.len() <= MAX_ADDONS)
        .ok_or(UNAVAILABLE)?;
    Ok(descriptors
        .iter()
        .map(|v| {
            let resources = v["manifest"]["resources"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|resource| resource.as_str().or_else(|| resource["name"].as_str()))
                .filter(|r| matches!(*r, "catalog" | "meta" | "stream" | "subtitles"))
                .map(str::to_owned)
                .collect();
            SourceAddon {
                id: Uuid::new_v4().simple().to_string(),
                name: safe_name(&v["manifest"]["name"]),
                resources,
                manifest_id: v["manifest"]["id"]
                    .as_str()
                    .filter(|id| !id.is_empty() && id.len() <= 256)
                    .map(str::to_owned),
                url: v["transportUrl"]
                    .as_str()
                    .filter(|u| u.len() <= 4096)
                    .map(str::to_owned),
            }
        })
        .collect())
}

pub(super) fn display(
    app: &App,
    db: &Connection,
    addons: &[SourceAddon],
) -> Result<Vec<Value>, Error> {
    let vault = app.secret_vault.as_deref();
    let account = app.identity().account_id().ok_or_else(auth::unauthorized)?;
    addons.iter().map(|addon| {
        let (status, reason) = match (vault, addon.url.as_deref()) {
            (None, _) => ("unavailable", Some("vault_unavailable")),
            (_, None) => ("unavailable", Some("unsupported_transport")),
            (Some(vault), Some(url)) => {
                let valid = url::Url::parse(url).ok().filter(|u| u.path().ends_with("/manifest.json") && u.fragment().is_none())
                    .is_some_and(|u| crate::source_http::validate(&u, app.addons.fixture_transport()).is_ok());
                if !valid || addon.manifest_id.is_none() {
                    ("unavailable", Some("unsupported_transport"))
                } else { match credentials_v2::snapshot(db, vault, account, url) {
                    Ok(Some(_)) => ("existing", None),
                    Ok(None) => ("add", None),
                    Err(_) => ("unavailable", Some("unsupported_transport")),
                }
                }
            }
        };
        Ok(json!({"item_id":addon.id,"name":addon.name,"resources":addon.resources,"status":status,"reason":reason}))
    }).collect()
}

pub(super) async fn verify(
    app: &App,
    selected: &[&SourceAddon],
) -> Result<Vec<VerifiedAddon>, ReviewError> {
    let failed = |id: &str| ReviewError::AddonVerification {
        failed_addon_items: vec![id.to_owned()],
    };
    let mut verified = Vec::new();
    for addon in selected {
        let raw = addon.url.as_deref().ok_or_else(|| failed(&addon.id))?;
        let (url, manifest) = app
            .addons
            .prepare_manifest(raw)
            .await
            .map_err(|_| failed(&addon.id))?;
        if manifest["id"].as_str() != addon.manifest_id.as_deref()
            || !manifest["resources"].is_array()
        {
            return Err(failed(&addon.id));
        }
        verified.push((addon.id.clone(), url, manifest));
    }
    let mut urls = std::collections::HashSet::new();
    for (id, url, _) in &verified {
        if !urls.insert(url) {
            return Err(failed(id));
        }
    }
    let account = app.identity().account_id().ok_or_else(auth::unauthorized)?;
    if verified.is_empty() {
        return Ok(Vec::new());
    }
    let vault = app
        .secret_vault
        .as_deref()
        .ok_or_else(|| failed(&verified[0].0))?;
    let db = app.db.lock().map_err(|_| STORAGE)?;
    verified
        .into_iter()
        .map(|(id, url, manifest)| {
            let expected =
                credentials_v2::snapshot(&db, vault, account, &url).map_err(|_| failed(&id))?;
            Ok(VerifiedAddon {
                url,
                manifest,
                expected,
            })
        })
        .collect()
}

/// Keep only what identity verification reads, so long series (One Piece,
/// Detective Conan) are bounded by episode count rather than by overviews
/// and thumbnails.
pub(super) fn trimmed_meta(meta: &Value) -> Value {
    let mut out = json!({"id":meta["id"],"type":meta["type"]});
    if let Some(videos) = meta["videos"].as_array() {
        out["videos"] = videos
            .iter()
            .map(|v| json!({"id":v["id"],"season":v["season"],"episode":v["episode"]}))
            .collect();
    }
    out
}

pub(super) fn checked_meta(response: Value, kind: &str, id: &str) -> Option<Value> {
    let meta = trimmed_meta(response.get("meta")?);
    (meta["id"] == id
        && meta["type"] == kind
        && serde_json::to_vec(&meta).is_ok_and(|v| v.len() <= 256_000)
        && meta["videos"].as_array().is_none_or(|v| v.len() <= 2000))
    .then_some(meta)
}

fn imdb(id: &str) -> bool {
    id.strip_prefix("tt").is_some_and(|digits| {
        (5..=12).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
    })
}
/// Fixed public Cinemeta, for IMDb movie/series ids only.
pub(super) async fn cinemeta(app: &App, kind: &str, id: &str) -> Option<Value> {
    if !(matches!(kind, "movie" | "series") && imdb(id)) {
        return None;
    }
    #[cfg(test)]
    let base = app
        .stremio_import
        .public_metadata_endpoint
        .as_deref()
        .unwrap_or("https://v3-cinemeta.strem.io");
    #[cfg(not(test))]
    let base = "https://v3-cinemeta.strem.io";
    let url = url::Url::parse(&format!("{base}/meta/{kind}/{id}.json")).ok()?;
    // The raw response carries overviews and thumbnails; only the trimmed
    // identity fields are retained.
    let value = crate::source_http::json(
        url,
        4_000_000,
        Duration::from_secs(8),
        app.addons.fixture_transport(),
        false,
    )
    .await
    .ok()?;
    checked_meta(value, kind, id)
}

const MAX_RETAINED_METADATA_BYTES: usize = 16 * 1024 * 1024;

fn retain_metadata(
    found: &mut HashMap<(String, String), Value>,
    remaining: &mut usize,
    identity: (String, String),
    meta: Value,
) -> bool {
    let Ok(bytes) = serde_json::to_vec(&meta) else {
        return false;
    };
    let Some(next) = remaining.checked_sub(bytes.len()) else {
        return false;
    };
    *remaining = next;
    found.insert(identity, meta);
    true
}
pub(super) async fn metadata(
    app: &App,
    items: &[Value],
    selected: &[VerifiedAddon],
) -> HashMap<(String, String), Value> {
    use futures::{stream, StreamExt};
    let mut found = HashMap::new();
    let mut remaining = MAX_RETAINED_METADATA_BYTES;
    let mut seen = std::collections::HashSet::new();
    let identities: Vec<_> = items
        .iter()
        .filter_map(|item| {
            let (kind, id) = (item["type"].as_str()?, item["_id"].as_str()?);
            seen.insert((kind, id))
                .then(|| (kind.to_owned(), id.to_owned()))
        })
        .collect();
    #[cfg(test)]
    let duration = app
        .stremio_import
        .metadata_deadline
        .unwrap_or(Duration::from_secs(30));
    #[cfg(not(test))]
    // Leave room for add-on verification and publication within the dashboard's
    // 45-second request timeout. Completed metadata survives this earlier cutoff.
    let duration = Duration::from_secs(30);
    let deadline = tokio::time::Instant::now() + duration;
    let mut pending = stream::iter(identities)
        .map(|(kind, id)| async move {
            let (kind, id) = (kind.as_str(), id.as_str());
            let from_owned = app
                .addons
                .clone()
                .for_account(app.identity().account_id().unwrap_or_default())
                .with_protected_fetch()
                .meta(kind, id)
                .await
                .ok();
            let mut meta = from_owned.and_then(|r| checked_meta(r, kind, id));
            if meta.is_none() {
                for addon in selected
                    .iter()
                    .filter(|a| crate::addon::supports(&a.manifest, "meta", kind, id))
                    .take(8)
                {
                    if let Ok(value) = app.addons.metadata_from(&addon.url, kind, id).await {
                        meta = checked_meta(value, kind, id);
                        if meta.is_some() {
                            break;
                        }
                    }
                }
            }
            if meta.is_none() {
                meta = cinemeta(app, kind, id).await;
            }
            meta.map(|meta| ((kind.to_owned(), id.to_owned()), meta))
        })
        .buffer_unordered(8);
    // Retain completed results within both the time and total byte budgets.
    // Dropping unfinished work leaves its identities review-only, never guessed.
    while let Ok(Some(result)) = tokio::time::timeout_at(deadline, pending.next()).await {
        if let Some((identity, meta)) = result {
            if !retain_metadata(&mut found, &mut remaining, identity, meta) {
                break;
            }
        }
    }
    found
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn retained_metadata_has_an_aggregate_byte_bound() {
        let mut found = HashMap::new();
        let mut remaining = MAX_RETAINED_METADATA_BYTES;
        let mut stopped = false;
        for index in 0..100 {
            let id = format!("tt{:07}", index);
            let meta = json!({"id":id,"type":"series","description":"x".repeat(240_000)});
            assert!(serde_json::to_vec(&meta).unwrap().len() < 256_000);
            if !retain_metadata(
                &mut found,
                &mut remaining,
                ("series".into(), id.clone()),
                meta,
            ) {
                assert!(!found.contains_key(&("series".into(), id)));
                stopped = true;
                break;
            }
        }
        assert!(
            stopped,
            "metadata beyond the batch budget must stay unresolved"
        );
        let retained: usize = found
            .values()
            .map(|v| serde_json::to_vec(v).unwrap().len())
            .sum();
        assert!(retained <= MAX_RETAINED_METADATA_BYTES);
        assert_eq!(retained + remaining, MAX_RETAINED_METADATA_BYTES);
        assert!(
            found.len() > 16,
            "the budget must not restore the old 16-title cap"
        );
    }
}
