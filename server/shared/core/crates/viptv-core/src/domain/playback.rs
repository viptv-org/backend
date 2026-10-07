use super::*;

/// V2 is a closed lease envelope. Never promote arbitrary delivery fields into
/// transport authority, and never expose a usable URL from a terminal lease.
pub(super) fn playback_v2(v: &Value, origin: &str) -> Result<Value> {
    let id = string(v, "id")?;
    if id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(invalid());
    }
    let status = string(v, "status")?;
    if !matches!(
        status.as_str(),
        "starting" | "ready" | "failed" | "expired" | "released"
    ) {
        return Err(invalid());
    }
    let expires = v["expires_at"]
        .as_u64()
        .filter(|n| *n <= 9_007_199_254_740)
        .ok_or_else(invalid)?;
    let renew = v["renew_after_seconds"]
        .as_u64()
        .filter(|n| (1..=300).contains(n))
        .ok_or_else(invalid)?;
    let mut out = json!({"id":id,"status":status,"expiresAt":expires * 1000,"renewAfterSeconds":renew,"session":null,"errorCode":null,"error":null});
    if status != "ready" {
        if matches!(status.as_str(), "failed" | "expired") {
            let code = v["error_code"].as_str().unwrap_or(if status == "expired" {
                "playback_expired"
            } else {
                "playback_failed"
            });
            let error = api_error(&json!({"status":502,"error_code":code}));
            out["errorCode"] = error["code"].clone();
            out["error"] = error["message"].clone();
        }
        return Ok(out);
    }
    let delivery = obj(&v["delivery"])?;
    let value = &v["delivery"];
    let kind = string(value, "kind")?;
    if !matches!(kind.as_str(), "direct" | "gateway") {
        return Err(invalid());
    }
    let raw = string(value, "url")?;
    let url = url::Url::parse(&raw).map_err(|_| invalid())?;
    if raw.len() > 16384
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (kind == "gateway" && url.scheme() != "https")
    {
        return Err(invalid());
    }
    let position = number(value, "position")?;
    if !position.is_finite() || position < 0.0 {
        return Err(invalid());
    }
    let live = boolean(value, "live")?;
    let mut safe = json!({"id":id,"url":url.as_str(),"position":if live { 0.0 } else { position },"live":live});
    if kind == "direct" {
        if value["format"] != "original" {
            return Err(invalid());
        }
        let headers = obj(&value["headers"])?;
        if headers.len() > 32 {
            return Err(invalid());
        }
        for (name, value) in headers {
            if name.is_empty()
                || name.len() > 128
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                || matches!(
                    name.to_ascii_lowercase().as_str(),
                    "host"
                        | "connection"
                        | "content-length"
                        | "transfer-encoding"
                        | "upgrade"
                        | "keep-alive"
                        | "te"
                        | "trailer"
                        | "proxy-authorization"
                )
                || !value
                    .as_str()
                    .is_some_and(|s| s.len() <= 8192 && !s.contains(['\r', '\n', '\0']))
            {
                return Err(invalid());
            }
        }
        safe["headers"] = json!(headers);
        safe["authorization"] = json!({"headers":headers});
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("cookie") {
                safe["authorization"]["cookie"] = value.clone();
            }
            if name.eq_ignore_ascii_case("user-agent") {
                safe["authorization"]["user_agent"] = value.clone();
            }
        }
        safe["format"] = json!("original");
        safe["mode"] = json!("direct");
    } else {
        if delivery
            .get("headers")
            .is_some_and(|h| !h.is_null() && h != &json!({}))
            || delivery.get("authorization").is_some_and(|a| !a.is_null())
        {
            return Err(invalid());
        }
        for (field, allowed) in [
            ("format", &["hls"][..]),
            ("mode", &["direct", "remux", "transcode"][..]),
            ("video_mode", &["copy", "encode"][..]),
            ("audio_mode", &["copy", "encode", "none"][..]),
        ] {
            if !value[field].as_str().is_some_and(|s| allowed.contains(&s)) {
                return Err(invalid());
            }
            safe[field] = value[field].clone();
        }
        let duration = number(value, "duration")?;
        if !duration.is_finite() || duration < 0.0 {
            return Err(invalid());
        }
        safe["duration"] = json!(duration);
        safe["subtitles_supported"] = json!(boolean(value, "subtitles_supported")?);
        for field in ["audio_tracks", "subtitle_tracks"] {
            let tracks = array(value, field)?;
            if tracks.len() > 256
                || tracks
                    .iter()
                    .any(|track| !track["input_index"].as_u64().is_some_and(|n| n <= 65535))
            {
                return Err(invalid());
            }
            safe[field] = json!(tracks);
        }
    }
    let mut session = playback(&safe, origin)?;
    // Direct native players consume language preferences locally. Do not copy
    // the retired quality preference or any unrecognized transport metadata.
    let preferences = &value["preferences"];
    for (field, output) in [
        ("audio_language", "preferredAudioLanguage"),
        ("subtitle_language", "preferredSubtitleLanguage"),
    ] {
        if field == "subtitle_language" && preferences["subtitles_enabled"].as_bool() != Some(true)
        {
            continue;
        }
        if let Some(language) = preferences[field].as_str() {
            if language.is_empty()
                || language.len() > 35
                || !language
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Err(invalid());
            }
            session[output] = json!(language);
        }
    }
    session["deliveryKind"] = json!(kind);
    out["session"] = session;
    Ok(out)
}

pub(super) fn source(v: &Value) -> Result<Value> {
    let mut out =
        json!({"id":string(v,"id")?,"name":fallback(v,&["name","title"],"Source"),"raw":clean(v)});
    for (a, b) in [
        ("provider", "provider"),
        ("description", "description"),
        ("source_fingerprint", "sourceFingerprint"),
        ("title", "title"),
        ("filename", "filename"),
        ("source_addon_id", "sourceAddonId"),
        ("source_name", "sourceName"),
        ("source_audio", "audio"),
    ] {
        optional_string(v, &mut out, a, b);
    }
    if let Some(q) = v["source_quality"]
        .as_str()
        .or_else(|| v["quality"].as_str())
    {
        out["quality"] = json!(q);
    }
    Ok(out)
}
fn track(v: &Value) -> Result<Value> {
    let mut out = json!({"inputIndex":number(v,"input_index")?,"languageStatus":fallback(v,&["language_status"],"unknown"),"title":fallback(v,&["title"],""),"selected":v["selected"].as_bool().unwrap_or(false),"supported":v["supported"].as_bool().unwrap_or(false),"selectable":v["selectable"].as_bool().unwrap_or(false)});
    optional_string(v, &mut out, "codec", "codec");
    optional_string(v, &mut out, "language", "language");
    Ok(out)
}
pub fn playback(v: &Value, origin: &str) -> Result<Value> {
    let base = url::Url::parse(origin).map_err(|_| invalid())?;
    // Proxy sessions serve root-relative /media/ capability URLs on the API
    // origin. A direct-URL session hands the ORIGINAL absolute source URL to a
    // client that declared direct_urls, so an absolute http(s) URL without
    // embedded credentials passes through unchanged.
    let raw = string(v, "url")?;
    let url = if raw.starts_with('/') {
        let joined = base.join(&raw).map_err(|_| invalid())?;
        if !matches!(base.scheme(), "https" | "http")
            || joined.origin() != base.origin()
            || !joined.path().starts_with("/media/")
            || !joined.username().is_empty()
            || joined.password().is_some()
        {
            return Err(invalid());
        }
        joined
    } else {
        let parsed = url::Url::parse(&raw).map_err(|_| invalid())?;
        if !matches!(parsed.scheme(), "https" | "http")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(invalid());
        }
        parsed
    };
    let mut out = json!({"headers":v["headers"].as_object().map(|h|h.iter().filter(|(_,v)|v.is_string()).map(|(k,v)|(k.clone(),v.clone())).collect::<Map<String,Value>>()).unwrap_or_default(),"id":string(v,"id")?,"url":url.as_str(),"format":fallback(v,&["format"],"hls"),"mode":fallback(v,&["mode"],"direct"),"videoMode":fallback(v,&["video_mode"],"copy"),"audioMode":fallback(v,&["audio_mode"],"copy"),"position":v["position"].as_f64().unwrap_or(0.0),"live":v["live"].as_bool().unwrap_or(false),"duration":v["duration"].as_f64().unwrap_or(0.0),"audioTracks":v["audio_tracks"].as_array().into_iter().flatten().filter(|v|v.is_object()).map(track).collect::<Result<Vec<_>>>()?,"subtitleTracks":v["subtitle_tracks"].as_array().into_iter().flatten().filter(|v|v.is_object()).map(track).collect::<Result<Vec<_>>>()?,"subtitlesSupported":v["subtitles_supported"].as_bool().unwrap_or(false)});
    // Source authorization is present only when the session carries one;
    // absent fields are omitted, matching the generated wire types.
    if let Some(authorization) = authorization(v) {
        out["authorization"] = authorization;
    }
    optional_string(
        &v["preferences"],
        &mut out,
        "audio_language",
        "preferredAudioLanguage",
    );
    if v["preferences"]["subtitles_enabled"].as_bool() == Some(true) {
        optional_string(
            &v["preferences"],
            &mut out,
            "subtitle_language",
            "preferredSubtitleLanguage",
        );
    }
    Ok(out)
}

/// Source credentials for a direct-URL session: the server hands over the
/// provider's Cookie/User-Agent so a native engine fetches the original
/// stream itself. Proxy sessions carry none; absent fields are omitted.
fn authorization(v: &Value) -> Option<Value> {
    let source = v["authorization"].as_object()?;
    let mut out = Map::new();
    if let Some(cookie) = source.get("cookie").and_then(Value::as_str) {
        out.insert("cookie".into(), json!(cookie));
    }
    if let Some(user_agent) = source.get("user_agent").and_then(Value::as_str) {
        out.insert("userAgent".into(), json!(user_agent));
    }
    if let Some(headers) = source.get("headers").and_then(Value::as_object) {
        let headers: Map<String, Value> = headers
            .iter()
            .filter_map(|(name, value)| {
                let value = value.as_str()?;
                (!name.is_empty()
                    && name.len() <= 128
                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && !matches!(
                        name.to_ascii_lowercase().as_str(),
                        "host" | "connection" | "content-length"
                    )
                    && value.len() <= 8192
                    && !value.contains(['\r', '\n']))
                .then(|| (name.clone(), json!(value)))
            })
            .take(32)
            .collect();
        if !headers.is_empty() {
            out.insert("headers".into(), Value::Object(headers));
        }
    }
    Some(Value::Object(out))
}
