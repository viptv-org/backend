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

pub(super) async fn metadata(
    app: &App,
    items: &[Value],
    selected: &[VerifiedAddon],
) -> HashMap<(String, String), Value> {
    let mut found = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    for item in items {
        let (Some(kind), Some(id)) = (item["type"].as_str(), item["_id"].as_str()) else {
            continue;
        };
        if !seen.insert((kind, id)) {
            continue;
        }
        let from_owned = app
            .addons
            .clone()
            .for_account(app.identity().account_id().unwrap_or_default())
            .with_protected_fetch()
            .meta(kind, id)
            .await
            .ok();
        let mut response = from_owned;
        if response.as_ref().is_none_or(|r| r["meta"]["id"] != id) {
            response = None;
            for addon in selected
                .iter()
                .filter(|a| crate::addon::supports(&a.manifest, "meta", kind, id))
                .take(8)
            {
                if let Ok(value) = app.addons.metadata_from(&addon.url, kind, id).await {
                    if value["meta"]["id"] == id {
                        response = Some(value);
                        break;
                    }
                }
            }
        }
        if let Some(meta) = response.and_then(|r| r.get("meta").cloned()) {
            if meta["id"] == id
                && meta["type"] == kind
                && serde_json::to_vec(&meta).is_ok_and(|v| v.len() <= 256_000)
                && meta["videos"].as_array().is_none_or(|v| v.len() <= 2000)
            {
                found.insert((kind.to_owned(), id.to_owned()), meta);
            }
        }
    }
    found
}
