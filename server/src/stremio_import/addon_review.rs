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

pub(super) const VERIFY_BUDGET: Duration = Duration::from_secs(30);

fn reason(code: &str) -> &'static str {
    match code {
        "invalid_addon_endpoint" | "addon_private_destination" => "addon_unsupported_transport",
        "addon_redirect_rejected" => "addon_redirect_rejected",
        "addon_timeout" | "Upstream timed out" => "addon_timeout",
        "addon_access_denied" => "addon_access_denied",
        "addon_rate_limited" => "addon_rate_limited",
        "addon_protocol_invalid" | "addon_response_too_large" => "addon_protocol_invalid",
        "Invalid addon manifest" => "addon_manifest_invalid",
        "addon_checks_busy" => "addon_checks_busy",
        _ => "addon_unreachable",
    }
}

async fn check(
    app: &App,
    addon: &SourceAddon,
    deadline: tokio::time::Instant,
) -> Result<(String, Value), &'static str> {
    let raw = addon.url.as_deref().ok_or("addon_unsupported_transport")?;
    let (url, manifest) = app
        .addons
        .prepare_manifest_within(raw, deadline)
        .await
        .map_err(|error| reason(&error))?;
    if manifest["id"].as_str() != addon.manifest_id.as_deref() {
        return Err("manifest_id_changed");
    }
    if !manifest["resources"].is_array() {
        return Err("addon_manifest_invalid");
    }
    Ok((url, manifest))
}

pub(super) type VerifiedManifests = Mutex<HashMap<String, (String, Value)>>;

pub(super) async fn verify(
    app: &App,
    selected: &[&SourceAddon],
    deadline: tokio::time::Instant,
    known: &VerifiedManifests,
) -> Result<Vec<VerifiedAddon>, ReviewError> {
    use futures::{stream, StreamExt};
    let checked: Vec<_> = stream::iter(0..selected.len())
        .map(|index| {
            let addon = selected[index];
            async move {
                let cached = known.lock().ok().and_then(|k| k.get(&addon.id).cloned());
                let result = match cached {
                    Some(manifest) => Ok(manifest),
                    None => {
                        let result = check(app, addon, deadline).await;
                        if let (Ok(manifest), Ok(mut known)) = (&result, known.lock()) {
                            known.insert(addon.id.clone(), manifest.clone());
                        }
                        result
                    }
                };
                (addon, result)
            }
        })
        .buffered(4)
        .collect()
        .await;
    let mut failed = Vec::new();
    let mut verified = Vec::new();
    let mut urls = std::collections::HashSet::new();
    for (position, (addon, result)) in checked.into_iter().enumerate() {
        let outcome = result.and_then(|(url, manifest)| {
            if urls.insert(url.clone()) { Ok((url, manifest)) } else { Err("duplicate_manifest_url") }
        });
        match outcome {
            Ok((url, manifest)) => verified.push((addon.id.clone(), url, manifest)),
            Err(reason) => {
                tracing::warn!(position, selected = selected.len(), reason, "Stremio import add-on verification failed");
                failed.push((addon.id.clone(), reason));
            }
        }
    }
    if !failed.is_empty() {
        return Err(ReviewError::AddonVerification { failed });
    }
    if verified.is_empty() {
        return Ok(Vec::new());
    }
    let account = app.identity().account_id().ok_or_else(auth::unauthorized)?;
    let storage = |verified: &[(String, String, Value)]| ReviewError::AddonVerification {
        failed: verified.iter().map(|(id, _, _)| (id.clone(), "addon_storage_unavailable")).collect(),
    };
    let Some(vault) = app.secret_vault.as_deref() else { return Err(storage(&verified)) };
    let db = app.db.lock().map_err(|_| STORAGE)?;
    let mut failed = Vec::new();
    let mut out = Vec::new();
    for (id, url, manifest) in verified {
        match credentials_v2::snapshot(&db, vault, account, &url) {
            Ok(expected) => out.push(VerifiedAddon { url, manifest, expected }),
            Err(_) => failed.push((id, "addon_storage_unavailable")),
        }
    }
    if failed.is_empty() { Ok(out) } else { Err(ReviewError::AddonVerification { failed }) }
}


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
pub(super) fn identities(items: &[Value]) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    items
        .iter()
        .filter_map(|item| {
            let (kind, id) = (item["type"].as_str()?, item["_id"].as_str()?);
            seen.insert((kind, id)).then(|| (kind.to_owned(), id.to_owned()))
        })
        .collect()
}
pub(super) fn metadata_budget(app: &App) -> Duration {
    #[cfg(test)]
    if let Some(duration) = app.stremio_import.metadata_deadline {
        return duration;
    }
    let _ = app;
    Duration::from_secs(30)
}
async fn collect<F, Fut>(
    identities: Vec<(String, String)>,
    deadline: tokio::time::Instant,
    found: &mut HashMap<(String, String), Value>,
    remaining: &mut usize,
    lookup: F,
) where
    F: Fn(String, String) -> Fut,
    Fut: std::future::Future<Output = Option<Value>>,
{
    use futures::{stream, StreamExt};
    let mut pending = stream::iter(identities)
        .map(|(kind, id)| {
            let request = lookup(kind.clone(), id.clone());
            async move { request.await.map(|meta| ((kind, id), meta)) }
        })
        .buffer_unordered(8);
    while let Ok(Some(result)) = tokio::time::timeout_at(deadline, pending.next()).await {
        if let Some((identity, meta)) = result {
            if !retain_metadata(found, remaining, identity, meta) {
                break;
            }
        }
    }
}
async fn baseline_meta(app: &App, kind: &str, id: &str) -> Option<Value> {
    let owned = app
        .addons
        .clone()
        .for_account(app.identity().account_id().unwrap_or_default())
        .with_protected_fetch()
        .meta(kind, id)
        .await
        .ok()
        .and_then(|r| checked_meta(r, kind, id));
    match owned {
        Some(meta) => Some(meta),
        None => cinemeta(app, kind, id).await,
    }
}
pub(super) async fn baseline_metadata(
    app: &App,
    items: &[Value],
) -> (HashMap<(String, String), Value>, usize) {
    let mut found = HashMap::new();
    let mut remaining = MAX_RETAINED_METADATA_BYTES;
    let deadline = tokio::time::Instant::now() + metadata_budget(app);
    collect(identities(items), deadline, &mut found, &mut remaining, |kind, id| async move {
        baseline_meta(app, &kind, &id).await
    })
    .await;
    (found, MAX_RETAINED_METADATA_BYTES - remaining)
}

pub(super) struct BaselineMetadata {
    found: HashMap<(String, String), Value>,
    attempted: std::collections::HashSet<(String, String)>,
    retried: std::collections::HashSet<(String, String)>,
    remaining: usize,
}
impl Default for BaselineMetadata {
    fn default() -> Self {
        Self {
            found: HashMap::new(),
            attempted: Default::default(),
            retried: Default::default(),
            remaining: MAX_RETAINED_METADATA_BYTES,
        }
    }
}
pub(super) struct BaselineSnapshot {
    pub found: HashMap<(String, String), Value>,
    pub attempted: std::collections::HashSet<(String, String)>,
    pub retained: usize,
}
impl BaselineMetadata {
    pub(super) fn snapshot(&self) -> BaselineSnapshot {
        BaselineSnapshot {
            found: self.found.clone(),
            attempted: self.attempted.clone(),
            retained: MAX_RETAINED_METADATA_BYTES - self.remaining,
        }
    }
}
pub(super) fn background_budget(app: &App) -> Duration {
    #[cfg(test)]
    if let Some(duration) = app.stremio_import.metadata_deadline {
        return duration;
    }
    let _ = app;
    Duration::from_secs(120)
}
async fn collect_shared(
    app: &App,
    identities: Vec<(String, String)>,
    deadline: tokio::time::Instant,
    state: &Mutex<BaselineMetadata>,
    retry: bool,
) {
    use futures::{stream, StreamExt};
    let mut pending = stream::iter(identities)
        .map(|(kind, id)| async move {
            let meta = baseline_meta(app, &kind, &id).await;
            ((kind, id), meta)
        })
        .buffer_unordered(8);
    while let Ok(Some((identity, meta))) = tokio::time::timeout_at(deadline, pending.next()).await {
        let Ok(mut state) = state.lock() else { break };
        state.attempted.insert(identity.clone());
        if retry {
            state.retried.insert(identity.clone());
        }
        if let Some(meta) = meta {
            let BaselineMetadata { found, remaining, .. } = &mut *state;
            if !retain_metadata(found, remaining, identity, meta) {
                break;
            }
        }
    }
}
pub(super) async fn fill_baseline(
    app: &App,
    items: &[Value],
    state: &Mutex<BaselineMetadata>,
    deadline: tokio::time::Instant,
) {
    let wanted: Vec<_> = {
        let Ok(state) = state.lock() else { return };
        identities(items).into_iter().filter(|i| !state.attempted.contains(i)).collect()
    };
    collect_shared(app, wanted, deadline, state, false).await;
}
pub(super) async fn retry_baseline(app: &App, state: &Mutex<BaselineMetadata>, deadline: tokio::time::Instant) {
    let wanted: Vec<_> = {
        let Ok(mut state) = state.lock() else { return };
        let missing: Vec<_> = state
            .attempted
            .iter()
            .filter(|i| !state.found.contains_key(*i) && !state.retried.contains(*i) && imdb(&i.1))
            .cloned()
            .collect();
        missing
    };
    if !wanted.is_empty() {
        collect_shared(app, wanted, deadline, state, true).await;
    }
}

const MAX_CACHED_ADDON_METADATA_BYTES: usize = 8 * 1024 * 1024;
#[derive(Default)]
pub(super) struct AddonMetadataCache {
    entries: HashMap<(String, String, String), Option<Value>>,
    bytes: usize,
}
impl AddonMetadataCache {
    fn get(&self, key: &(String, String, String)) -> Option<Option<Value>> {
        self.entries.get(key).cloned()
    }
    fn insert(&mut self, key: (String, String, String), meta: Option<Value>) {
        let size = meta.as_ref().map_or(0, |m| serde_json::to_vec(m).map_or(usize::MAX, |v| v.len()));
        if self.bytes.saturating_add(size) <= MAX_CACHED_ADDON_METADATA_BYTES && self.entries.len() < 65_536 {
            self.bytes += size;
            self.entries.insert(key, meta);
        }
    }
}
async fn selected_meta(
    app: &App,
    selected: &[&VerifiedAddon],
    cache: &Mutex<AddonMetadataCache>,
    kind: &str,
    id: &str,
) -> Option<Value> {
    for addon in selected
        .iter()
        .filter(|a| crate::addon::supports(&a.manifest, "meta", kind, id))
        .take(8)
    {
        let key = (addon.url.clone(), kind.to_owned(), id.to_owned());
        let cached = cache.lock().ok().and_then(|c| c.get(&key));
        let meta = match cached {
            Some(meta) => meta,
            None => match app.addons.metadata_from(&addon.url, kind, id).await {
                Ok(value) => {
                    let meta = checked_meta(value, kind, id);
                    if let Ok(mut cache) = cache.lock() {
                        cache.insert(key, meta.clone());
                    }
                    meta
                }
                Err(_) => None,
            },
        };
        if meta.is_some() {
            return meta;
        }
    }
    None
}
pub(super) async fn selected_metadata(
    app: &App,
    unresolved: Vec<(String, String)>,
    selected: &[&VerifiedAddon],
    cache: &Mutex<AddonMetadataCache>,
    deadline: tokio::time::Instant,
    retained: usize,
) -> HashMap<(String, String), Value> {
    let mut found = HashMap::new();
    if selected.is_empty() || unresolved.is_empty() {
        return found;
    }
    let mut remaining = MAX_RETAINED_METADATA_BYTES.saturating_sub(retained);
    collect(unresolved, deadline, &mut found, &mut remaining, |kind, id| async move {
        selected_meta(app, selected, cache, &kind, &id).await
    })
    .await;
    found
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn addon_metadata_cache_has_a_byte_bound() {
        let mut cache = AddonMetadataCache::default();
        for index in 0..100 {
            let meta = json!({"id":index,"description":"x".repeat(240_000)});
            cache.insert(("https://a.invalid".into(), "series".into(), format!("id{index}")), Some(meta));
        }
        assert!(cache.bytes <= MAX_CACHED_ADDON_METADATA_BYTES);
        assert!(cache.entries.len() < 100, "later answers are dropped, not retained without bound");
        let key = ("https://a.invalid".to_owned(), "series".to_owned(), "id0".to_owned());
        assert!(cache.get(&key).is_some());
    }

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
