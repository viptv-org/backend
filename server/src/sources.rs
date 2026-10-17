use super::*;
mod input;

impl App {
    pub(crate) fn register(
        &self,
        source: &str,
        raw: Vec<Value>,
        kind: &str,
    ) -> (Vec<Value>, Option<String>) {
        self.register_with_configuration(source, raw, kind, None)
    }

    /// A previously resolved URL must not acquire a newer credential proof.
    pub(crate) fn register_with_configuration(
        &self,
        source: &str,
        raw: Vec<Value>,
        kind: &str,
        expected: Option<[u8; 32]>,
    ) -> (Vec<Value>, Option<String>) {
        let configuration = match source_configuration(&self.db.lock().unwrap(), source) {
            Ok(configuration) => configuration,
            Err(error) => return (vec![], Some(error.1)),
        };
        if expected.is_some() && configuration != expected {
            return (vec![], Some("source_configuration_changed".into()));
        }
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
        let mut unsupported_headers = false;
        let mut entries = self.streams.lock().unwrap();
        for r in raw.into_iter().take(100) {
            // Only the host SDK producer authors this requirement. An add-on
            // cannot use a display field to acquire a different playback policy.
            let required_audio_index = if source.starts_with("vsources:") {
                match r
                    .get("required_audio_index")
                    .filter(|value| !value.is_null())
                {
                    Some(value) => match value.as_u64().filter(|index| *index <= 65535) {
                        Some(index) => Some(index as u32),
                        None => {
                            unsupported += 1;
                            continue;
                        }
                    },
                    None => None,
                }
            } else {
                None
            };
            let Some(input) = input::Input::parse(&r, source.starts_with("addon:"), kind == "live")
            else {
                unsupported += 1;
                continue;
            };
            let url = input.url.as_str();
            if entries.len() >= 20000 {
                break;
            }
            let id = Uuid::new_v4().to_string();
            let mut headers = HashMap::new();
            if let Some(raw_headers) = r
                .pointer("/behaviorHints/proxyHeaders/request")
                .filter(|value| !value.is_null())
            {
                let Some(h) = raw_headers.as_object() else {
                    unsupported_headers = true;
                    continue;
                };
                let mut invalid = false;
                for (k, v) in h {
                    let key = k.to_ascii_lowercase();
                    if ![
                        "user-agent",
                        "cookie",
                        "referer",
                        "origin",
                        "authorization",
                        "accept",
                        "accept-language",
                        "x-requested-with",
                        "x-csrf-token",
                        "x-api-key",
                    ]
                    .contains(&key.as_str())
                        || headers.contains_key(&key)
                    {
                        invalid = true;
                        break;
                    }
                    let Some(value) = v.as_str().filter(|value| {
                        value.len() <= 4096 && !value.chars().any(char::is_control)
                    }) else {
                        invalid = true;
                        break;
                    };
                    headers.insert(key, value.into());
                }
                // Explicit request headers are required source context. Never
                // publish an opaque handle after silently discarding one.
                if invalid {
                    unsupported_headers = true;
                    continue;
                }
            }
            headers.extend(routing.clone());
            if url.starts_with("magnet:") && !headers.is_empty() {
                unsupported_headers = true;
                continue;
            }
            let mut public = source_card(&r, source, &id, url, &headers, &input.private_values);
            // IDs/ownership are server-authored; upstream metadata cannot replace them.
            public["id"] = json!(id);
            public["source"] = json!(source);
            // Stable across rediscovery, without persisting expiring playback URLs.
            let mut identity = if public["filename"].as_str().is_some_and(|v| !v.is_empty()) {
                json!([source, public["name"], public["filename"]])
            } else {
                json!([source, public["name"], public["title"]])
            };
            if input.requires_gateway {
                // Public continuation identity is opaque and derived from
                // private payload context, never an add-on's display label.
                // Configuration includes producer account ownership/revision.
                let canonical_headers: std::collections::BTreeMap<_, _> = headers.iter().collect();
                identity = json!([
                    source,
                    configuration,
                    url,
                    canonical_headers,
                    kind == "live",
                    input.file_index
                ]);
            }
            if source.starts_with("addon:")
                || source.starts_with("iptv:")
                || source.starts_with("vsources:")
            {
                public["source_addon_id"] = json!(source);
            }
            if let Some(index) = required_audio_index {
                identity = json!([identity, index]);
            }
            public["source_fingerprint"] = json!(format!(
                "{:x}",
                Sha256::digest(identity.to_string().as_bytes())
            ));
            public["source_name"] = json!(source_display_text(
                &source_name,
                256,
                url,
                &headers,
                &input.private_values
            ));
            if source.starts_with("addon:") {
                public["presentation"]["provider_name"] = public["source_name"].clone();
            }
            entries.insert(
                id.clone(),
                StreamEntry {
                    metadata_proof:None,
                    profile_revision: None,
                    media_id: None,
                    exact_vod: None,
                    live_channel_id: None,
                    producer: source.to_owned(),
                    configuration,
                    provider_id: source.strip_prefix("iptv:").and_then(|s| s.parse().ok()),
                    kind: kind.to_owned(),
                    live: kind == "live",
                    url: url.into(),
                    file_index: input.file_index,
                    discovery_trackers: input.trackers.clone(),
                    info_hash: input.info_hash.clone(),
                    requires_torrent_gateway: input.requires_gateway,
                    headers,
                    required_audio_index,
                    created: Instant::now(),
                },
            );
            self.own_resource("stream", &id);
            out.push(public);
        }
        let error = if unsupported_headers {
            Some("source_headers_unsupported".into())
        } else {
            (unsupported > 0).then(|| "source_format_unsupported".into())
        };
        (out, error)
    }
}

/// A private fingerprint, never a wire credential. Changes to ownership, source
/// credentials, enabled scopes or routing invalidate a cached source at use time.
pub(crate) fn source_configuration(
    db: &Connection,
    producer: &str,
) -> Result<Option<[u8; 32]>, ApiError> {
    let Some((kind, id)) = producer.split_once(':') else {
        return Ok(None);
    };
    let Ok(id) = id.parse::<i64>() else {
        return Ok(None);
    };
    let table = match kind {
        "iptv" => Some("providers"),
        "addon" => Some("addons"),
        _ => None,
    };
    if let Some(table) = table {
        let version: Option<i64> = db
            .query_row(
                &format!("SELECT credentials_version FROM {table} WHERE id=?1"),
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if version == Some(0) {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "source_credentials_migration_required".into(),
            ));
        }
    }
    let value:Option<String>=match kind {
        "iptv"=>{
            let (route,join)=if provider::egress::table_exists(db).map_err(ApiError::from)? {("COALESCE(r.warp,0)","LEFT JOIN provider_routes r ON r.provider_id=p.id")}else{("0","")};
            db.query_row(&format!("SELECT json_array(p.url,p.username,p.password,p.enabled,p.enable_live,p.enable_movies,p.enable_series,{route},o.account_id,p.credentials_version,c.account_id,c.secret) FROM providers p {join} LEFT JOIN provider_ownership o ON o.provider_id=p.id LEFT JOIN provider_credentials_v2 c ON c.provider_id=p.id WHERE p.id=?1"),[id],|row|row.get(0)).optional().map_err(db_error)?
        },
        "xtream"=>db.query_row("SELECT json_array(c.account_id,c.profile_id,c.revision,c.enabled,c.secret,p.revision,p.value) FROM engine_xtream_connections c LEFT JOIN engine_profile_configuration p ON p.profile_id=c.profile_id WHERE c.id=?1",[id],|r|r.get(0)).optional().map_err(db_error)?,
        "m3u"=>db.query_row("SELECT json_array(c.account_id,c.profile_id,c.revision,c.enabled,c.secret,p.revision,p.value) FROM engine_m3u_connections c LEFT JOIN engine_profile_configuration p ON p.profile_id=c.profile_id WHERE c.id=?1",[id],|r|r.get(0)).optional().map_err(db_error)?,
        "vsources"=>db.query_row("SELECT json_array(s.account_id,s.profile_id,s.revision,s.enabled,s.secret,p.revision,p.value) FROM engine_source_integrations s LEFT JOIN engine_profile_configuration p ON p.profile_id=s.profile_id WHERE s.id=?1",[id],|row|row.get(0)).optional().map_err(db_error)?,
        "addon"=>db.query_row("SELECT json_array(a.manifest_url,a.enabled,a.account_id,a.credentials_version,a.credentials_revision,a.manifest,i.incarnation) FROM addons a JOIN engine_addon_incarnations i ON i.addon_id=a.id WHERE a.id=?1",[id],|row|row.get(0)).optional().map_err(db_error)?,
        _=>None,
    };
    Ok(value.map(|value| Sha256::digest(value.as_bytes()).into()))
}

fn source_display_text(
    text: &str,
    limit: usize,
    url: &str,
    headers: &HashMap<String, String>,
    private_values: &[String],
) -> String {
    let mut text = text.replace(url, "[link omitted]");
    for value in private_values {
        text = redact_private_value(&text, value);
    }
    // Ordinary negotiation/client-identification values are display text too:
    // e.g. Accept-Language: en must not erase en/eng or letters inside French.
    // Origin/Referer links are handled by the URL redaction below.
    for (_, value) in headers.iter().filter(|(key, value)| {
        (key.eq_ignore_ascii_case("authorization")
            || key.eq_ignore_ascii_case("x-csrf-token")
            || key.eq_ignore_ascii_case("x-api-key"))
            && !value.is_empty()
    }) {
        text = redact_private_value(&text, value);
        if let Some((scheme, credential)) = value.split_once(' ') {
            if (scheme.eq_ignore_ascii_case("bearer") || scheme.eq_ignore_ascii_case("basic"))
                && !credential.is_empty()
            {
                text = redact_private_value(&text, credential);
            }
        }
    }
    if let Some(cookie) = headers.get("cookie") {
        text = redact_private_value(&text, cookie);
        for pair in cookie.split(';').map(str::trim) {
            text = redact_private_value(&text, pair);
            if let Some((_, credential)) = pair.split_once('=') {
                text = redact_private_value(&text, credential.trim().trim_matches('"'));
            }
        }
    }
    let text = text
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(|word| {
                    if word.contains("://") || word.to_ascii_lowercase().contains("magnet:") {
                        "[link omitted]".to_owned()
                    } else {
                        word.chars().filter(|c| !c.is_control()).collect::<String>()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim();
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn redact_private_value(text: &str, value: &str) -> String {
    if value.is_empty() {
        return text.to_owned();
    }
    // Opaque values remain private even when reflected inside filenames or
    // longer text. Very short values use token boundaries so e.g. cookie en
    // does not tear letters out of French/eng or client-identification words.
    if value.chars().count() >= 4 {
        return text.replace(value, "[private value omitted]");
    }
    let boundary =
        |character: Option<char>| character.is_none_or(|c| !c.is_alphanumeric() && c != '_');
    let mut out = String::new();
    let mut copied = 0;
    for (start, _) in text.match_indices(value) {
        let end = start + value.len();
        if boundary(text[..start].chars().next_back()) && boundary(text[end..].chars().next()) {
            out.push_str(&text[copied..start]);
            out.push_str("[private value omitted]");
            copied = end;
        }
    }
    out.push_str(&text[copied..]);
    out
}

fn source_card(
    raw: &Value,
    source: &str,
    id: &str,
    url: &str,
    headers: &HashMap<String, String>,
    private_values: &[String],
) -> Value {
    let clean = |text: &str, limit| source_display_text(text, limit, url, headers, private_values);
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
    if let Some(presentation) =
        viptv_engine::stremio::SourcePresentation::read(&raw["presentation"])
    {
        let mut value = serde_json::to_value(presentation).unwrap_or(Value::Null);
        if let Some(fields) = value.as_object_mut() {
            for field in fields.values_mut() {
                if let Some(text) = field.as_str() {
                    *field = json!(clean(text, 128));
                } else if let Some(values) = field.as_array_mut() {
                    for item in values {
                        if let Some(text) = item.as_str() {
                            *item = json!(clean(text, 64));
                        }
                    }
                }
            }
        }
        card["presentation"] = value;
    }
    for key in ["quality", "codec"] {
        if let Some(value) = raw[key].as_str() {
            let value = clean(value, 128);
            if !value.is_empty() {
                card[key] = json!(value);
            }
        }
    }
    if let Some(height) = raw["quality_height"]
        .as_u64()
        .filter(|h| (144..=4320).contains(h))
    {
        card["quality_height"] = json!(height);
    }
    let explicit_language_present = [
        "audio_languages",
        "reported_languages",
        "language",
        "languages",
    ]
    .iter()
    .any(|key| match &raw[*key] {
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(values) => values
            .iter()
            .any(|value| value.as_str().is_some_and(|s| !s.trim().is_empty())),
        _ => false,
    });
    let explicit_audio_present = raw["audio_languages"].as_array().is_some_and(|languages| {
        languages
            .iter()
            .any(|v| v.as_str().is_some_and(|s| !s.trim().is_empty()))
    });
    let audio = raw["audio_languages"]
        .as_array()
        .map(|languages| {
            languages
                .iter()
                .take(16)
                .filter_map(Value::as_str)
                .map(|tag| clean(tag, 32))
                .filter(|tag| viptv_engine::preferences::language_tag(tag))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !audio.is_empty() {
        card["audio_languages"] = json!(audio);
    }
    if let Some(filename) = raw.get("filename").and_then(Value::as_str).or_else(|| {
        raw.pointer("/behaviorHints/filename")
            .and_then(Value::as_str)
    }) {
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
        .get("size_bytes")
        .and_then(Value::as_u64)
        .or_else(|| {
            raw.pointer("/behaviorHints/videoSize")
                .and_then(Value::as_u64)
        })
        .filter(|n| *n > 0 && *n <= (1_u64 << 50))
    {
        card["size_bytes"] = json!(size);
        card["presentation"]["size_bytes"] = json!(size);
    }
    let mut languages = Vec::new();
    for key in ["reported_languages", "language", "languages"] {
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
    // Parse only the already redacted public fields. Hints are informational;
    // opaque grants and private playback context remain host-authored above.
    if let Some(presentation) = viptv_engine::stremio::SourcePresentation::from_stream(&card) {
        card["presentation"] = json!(presentation);
    }
    if let Some(height) = viptv_engine::stremio::source_quality_height(&card) {
        card["quality_height"] = json!(height);
    }
    let languages = if (explicit_audio_present && audio.is_empty())
        || (explicit_language_present
            && audio.is_empty()
            && card["reported_languages"]
                .as_array()
                .is_none_or(Vec::is_empty))
    {
        Vec::new()
    } else {
        viptv_engine::stremio::source_audio_languages(&card)
    };
    card["audio_languages"] = json!(languages);
    if !languages.is_empty() {
        card["audio_language_status"] = json!("unverified");
    }
    if let Some(detail) = viptv_engine::stremio::source_detail(&card) {
        card["detail"] = json!(detail);
    }
    card
}

#[cfg(test)]
mod metadata_tests {
    use super::*;

    #[test]
    fn sanitized_source_metadata_batch_matches_shared_parser() {
        let corpus: Value = serde_json::from_str(include_str!(
            "../shared/engine/source-metadata-vectors.json"
        ))
        .unwrap();
        let captured: Value = serde_json::from_str(include_str!(
            "../shared/engine/source-metadata-captured.json"
        ))
        .unwrap();
        let started = Instant::now();
        let cases: Vec<_> = corpus["cases"]
            .as_array()
            .unwrap()
            .iter()
            .chain(captured["cases"].as_array().unwrap())
            .collect();
        for case in &cases {
            let card = source_card(
                &case["source"],
                "addon:fixture",
                "opaque-grant",
                "https://media.example/opaque",
                &HashMap::new(),
                &[],
            );
            assert_eq!(card["id"], "opaque-grant");
            assert!(card.get("url").is_none());
            for (field, expected) in case["expected"].as_object().unwrap() {
                let actual = match field.as_str() {
                    "height" => &card["quality_height"],
                    "languages" => {
                        let value = &card["audio_languages"];
                        if value.is_null() && expected == &json!([]) {
                            continue;
                        }
                        value
                    }
                    // Backend detail includes a stable fallback when all upstream text is absent.
                    "detail" => &card["detail"],
                    _ => &card["presentation"][field],
                };
                assert_eq!(actual, expected, "{} field {field}", case["id"]);
            }
        }
        eprintln!(
            "SANITIZED_SOURCE_METADATA_BATCH cases={} elapsed_ms={}",
            cases.len(),
            started.elapsed().as_millis()
        );
        assert!(started.elapsed().as_secs() < 60);
    }

    #[test]
    fn metadata_hints_are_parsed_after_private_reflections_are_redacted() {
        let raw = json!({"name":"Provider", "title":"Secret4K HEVC https://private.example/2160p.AVC.mkv", "description":"AAC key-secret-ENG", "audio_languages":["key-secret-ENG"], "behaviorHints":{"filename":"Film.1080p.mkv"}});
        let card = source_card(
            &raw,
            "addon:fixture",
            "grant",
            "https://media.example/opaque",
            &HashMap::new(),
            &["Secret4K".into(), "key-secret-ENG".into()],
        );
        assert_eq!(card["quality_height"], 1080);
        assert_eq!(card["presentation"]["video_codec"], "HEVC");
        assert_eq!(card["presentation"]["container"], "mkv");
        assert_eq!(card["audio_languages"], json!([]));
        let public = card.to_string();
        assert!(
            !public.contains("Secret4K")
                && !public.contains("key-secret")
                && !public.contains("private.example")
        );
    }
}
