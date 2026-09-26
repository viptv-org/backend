use super::*;

impl App {
    pub(crate) fn register(
        &self,
        source: &str,
        raw: Vec<Value>,
        kind: &str,
    ) -> (Vec<Value>, Option<String>) {
        // This label comes from the configured producer, never upstream release metadata.
        let source_name = source
            .split_once(':')
            .and_then(|(kind, id)| {
                let table = match kind {
                    "addon" => "addons",
                    "iptv" => "providers",
                    _ => return None,
                };
                let id = id.parse::<i64>().ok()?;
                self.db
                    .lock()
                    .unwrap()
                    .query_row(
                        &format!("SELECT name FROM {table} WHERE id=?1"),
                        [id],
                        |row| row.get::<_, String>(0),
                    )
                    .ok()
            })
            .unwrap_or_else(|| source.to_owned());
        let routing = match source
            .strip_prefix("iptv:")
            .and_then(|v| v.parse::<i64>().ok())
        {
            Some(id) => match provider::egress::headers(&self.db.lock().unwrap(), id) {
                Ok(h) => h,
                Err(_) => return (vec![], Some("Provider WARP route unavailable".into())),
            },
            None => HashMap::new(),
        };
        let mut out = vec![];
        let mut unsupported = 0;
        let mut entries = self.streams.lock().unwrap();
        for r in raw.into_iter().take(100) {
            let Some(url) = r["url"].as_str() else {
                unsupported += 1;
                continue;
            };
            if util::validate_url(url).is_err() {
                unsupported += 1;
                continue;
            }
            if entries.len() >= 20000 {
                break;
            }
            let id = Uuid::new_v4().to_string();
            let mut headers = HashMap::new();
            if let Some(h) = r
                .pointer("/behaviorHints/proxyHeaders/request")
                .and_then(Value::as_object)
            {
                for (k, v) in h {
                    let key = k.to_ascii_lowercase();
                    if [
                        "user-agent",
                        "cookie",
                        "referer",
                        "origin",
                        "authorization",
                        "accept",
                        "accept-language",
                        "x-requested-with",
                        "x-csrf-token",
                    ]
                    .contains(&key.as_str())
                    {
                        if let Some(v) = v.as_str() {
                            if v.len() <= 4096 && !v.chars().any(char::is_control) {
                                headers.insert(key, v.into());
                            }
                        }
                    }
                }
            }
            headers.extend(routing.clone());
            let mut public = source_card(&r, source, &id, url, &headers);
            // IDs/ownership are server-authored; upstream metadata cannot replace them.
            public["id"] = json!(id);
            public["source"] = json!(source);
            // Stable across rediscovery, without persisting expiring playback URLs.
            let identity = if public["filename"].as_str().is_some_and(|v| !v.is_empty()) {
                json!([source, public["name"], public["filename"]])
            } else {
                json!([source, public["name"], public["title"]])
            };
            if source.starts_with("addon:") || source.starts_with("iptv:") {
                public["source_addon_id"] = json!(source);
            }
            public["source_fingerprint"] = json!(format!(
                "{:x}",
                Sha256::digest(identity.to_string().as_bytes())
            ));
            public["source_name"] = json!(source_display_text(&source_name, 256, url, &headers));
            entries.insert(
                id.clone(),
                StreamEntry {
                    provider_id: source.strip_prefix("iptv:").and_then(|s| s.parse().ok()),
                    kind: kind.to_owned(),
                    live: kind == "live",
                    url: url.into(),
                    headers,
                    created: Instant::now(),
                },
            );
            self.own_resource("stream", &id);
            out.push(public);
        }
        let error=(unsupported>0).then(||format!("{unsupported} source(s) unsupported: torrent, external-player, or non-HTTP streams require an external resolver"));
        (out, error)
    }
}

use viptv_playback_engine::source_display_text;

fn source_card(
    raw: &Value,
    source: &str,
    id: &str,
    url: &str,
    headers: &HashMap<String, String>,
) -> Value {
    let clean = |text: &str, limit| source_display_text(text, limit, url, headers);
    let description = clean(raw["description"].as_str().unwrap_or(""), 2048);
    let title = ["title", "description"]
        .into_iter()
        .filter_map(|key| raw[key].as_str().map(str::trim))
        .find(|text| !text.is_empty())
        .unwrap_or("HTTP stream");
    let name = raw["name"]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(source);
    let mut card = json!({"id":id,"source":source,"name":clean(name,256),"title":clean(title,1024),"description":description,"audio_language_status":"unknown"});
    if let Some(filename) = raw
        .pointer("/behaviorHints/filename")
        .and_then(Value::as_str)
    {
        if !filename.contains("://") && !filename.contains(url) {
            let filename = filename
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("")
                .split(['?', '#'])
                .next()
                .unwrap_or("");
            let filename = clean(filename, 512);
            if !filename.is_empty() {
                if let Some(group) = continuation::release_group(&filename) {
                    card["source_release_group"] = json!(group);
                }
                card["filename"] = json!(filename);
            }
        }
    }
    if let Some(group) = raw
        .pointer("/behaviorHints/bingeGroup")
        .and_then(Value::as_str)
    {
        let group = clean(group, 128);
        if !group.is_empty() {
            card["source_binge_group"] = json!(group);
        }
    }
    if let Some(size) = raw
        .pointer("/behaviorHints/videoSize")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0 && *n <= (1_u64 << 50))
    {
        card["size_bytes"] = json!(size);
    }
    let mut languages = Vec::new();
    for key in ["language", "languages"] {
        let values: Vec<&Value> = match &raw[key] {
            Value::String(_) => vec![&raw[key]],
            Value::Array(values) => values.iter().take(16).collect(),
            _ => vec![],
        };
        for value in values {
            if let Some(language) = value.as_str() {
                let language = language.trim();
                if !language.is_empty()
                    && language.len() <= 32
                    && language
                        .chars()
                        .all(|c| c.is_alphabetic() || matches!(c, '-' | '_' | ' '))
                {
                    let language = clean(language, 32);
                    if !language.is_empty()
                        && !languages.contains(&language)
                        && languages.len() < 16
                    {
                        languages.push(language);
                    }
                }
            }
        }
    }
    if !languages.is_empty() {
        card["reported_languages"] = json!(languages);
        card["audio_language_status"] = json!("unverified");
    }
    card
}
