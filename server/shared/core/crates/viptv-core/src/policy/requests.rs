use super::*;

pub(super) fn request(v: &Value) -> Result {
    Ok({
        let op = text(v, "operation");
        let profile = enc(text(v, "profileId"));
        let base = format!("/api/profiles/{profile}");
        let mut body = item_request(&v["item"]);
        let (method, path) = match op {
            "nextEpisode" => ("POST", format!("{base}/continue/next")),
            "sources" => ("POST", "/api/streams".into()),
            "sourcesV2" => ("POST", "/api/v2/streams".into()),
            "sourcesPoll" | "sourcesPollV2" => {
                body = Value::Null;
                let id = text(v, "id");
                if id.is_empty() {
                    return Err(CoreError::InvalidInput);
                }
                let after = v["after"].as_u64().unwrap_or(0);
                let prefix = if op == "sourcesPollV2" {
                    "/api/v2/streams"
                } else {
                    "/api/streams"
                };
                ("GET", format!("{prefix}/{}?after={after}", enc(id)))
            }
            "livePageV2" | "liveCategoriesV2" => {
                body = Value::Null;
                ("GET", live_catalog_request(v, op == "liveCategoriesV2")?)
            }
            "liveGuideV2" | "liveSourceV2" => {
                body = Value::Null;
                let id = text(v, "id");
                if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
                    return Err(CoreError::InvalidInput);
                }
                if op == "liveSourceV2" {
                    ("POST", format!("/api/v2/iptv/live/{}/source", enc(id)))
                } else {
                    ("GET", format!("/api/v2/iptv/guide/{}", enc(id)))
                }
            }
            "saveProgress" => {
                body["position"] = v["position"].clone();
                body["duration"] = v["duration"].clone();
                ("PUT", format!("{base}/progress"))
            }
            "correctProgress" => {
                body["action"] = v["action"].clone();
                body["duration"] = v["duration"].clone();
                ("PUT", format!("{base}/progress/correct"))
            }
            "setQueueVisibility" => {
                body["hidden"] = v["hidden"].clone();
                ("PUT", format!("{base}/continue/visibility"))
            }
            "toggleFavorite" => ("POST", format!("{base}/favorites/toggle")),
            "setFavorite" => ("PUT", format!("{base}/favorites")),
            "playback" => {
                body = snake(&v["playback"]);
                ("POST", "/api/playback".into())
            }
            "playbackProtocolV2" => {
                body = Value::Null;
                ("GET", "/api/v2/playback-protocol".into())
            }
            "playbackV2CancelRequest" => {
                let id = text(v, "requestId");
                if !crate::domain::playback_protocol::valid_identifier(id) {
                    return Err(CoreError::InvalidInput);
                }
                body = Value::Null;
                ("DELETE", format!("/api/v2/playback-requests/{id}"))
            }
            "playbackV2" => {
                body = playback_v2_request(&v["playback"])?;
                ("POST", "/api/v2/playback".into())
            }
            "playbackV2Status" | "playbackV2Heartbeat" | "playbackV2Stop" => {
                let id = text(v, "id");
                if id.is_empty()
                    || id.len() > 128
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                {
                    return Err(CoreError::InvalidInput);
                }
                body = Value::Null;
                let path = format!("/api/v2/playback/{}", enc(id));
                match op {
                    "playbackV2Heartbeat" => {
                        body = json!({});
                        ("POST", format!("{path}/heartbeat"))
                    }
                    "playbackV2Stop" => ("DELETE", path),
                    _ => ("GET", path),
                }
            }
            "metadata" => {
                body = Value::Null;
                let item = &v["item"];
                let series_id = text(item, "seriesId");
                let (media_type, media_id) = if series_id.is_empty() {
                    (text(item, "type"), text(item, "id"))
                } else {
                    ("series", series_id)
                };
                (
                    "GET",
                    format!("/api/meta/{}/{}", enc(media_type), enc(media_id)),
                )
            }
            _ => return Err(CoreError::InvalidInput),
        };
        json!({"method":method,"path":path,"body":body})
    })
}

pub(crate) fn valid_live_cursor(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn live_catalog_request(v: &Value, categories: bool) -> std::result::Result<String, CoreError> {
    if ["offset", "view", "filter"]
        .iter()
        .any(|key| !v[*key].is_null())
    {
        return Err(CoreError::InvalidInput);
    }
    let limit = match &v["limit"] {
        Value::Null => 50,
        value => value
            .as_u64()
            .filter(|n| (1..=200).contains(n))
            .ok_or(CoreError::InvalidInput)?,
    };
    let mut query = format!("limit={limit}");
    if !v["collection"].is_null() {
        let value = v["collection"]
            .as_str()
            .filter(|value| matches!(*value, "favorites" | "recent"))
            .ok_or(CoreError::InvalidInput)?;
        if categories {
            return Err(CoreError::InvalidInput);
        }
        query += &format!("&collection={value}");
    }
    if !v["catalogId"].is_null() {
        let value = v["catalogId"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 16 && s.bytes().all(|b| b.is_ascii_digit()))
            .ok_or(CoreError::InvalidInput)?;
        let id = value
            .parse::<u64>()
            .ok()
            .filter(|id| *id > 0 && *id <= 9_007_199_254_740_991)
            .ok_or(CoreError::InvalidInput)?;
        query += &format!("&catalog_id={id}");
    }
    if !v["categoryId"].is_null() {
        let id = v["categoryId"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
            .ok_or(CoreError::InvalidInput)?;
        if categories {
            return Err(CoreError::InvalidInput);
        }
        query += &format!("&category_id={}", enc(id));
    }
    if !v["search"].is_null() {
        let search = v["search"]
            .as_str()
            .filter(|s| s.len() <= 128 && !s.chars().any(char::is_control))
            .ok_or(CoreError::InvalidInput)?
            .trim();
        if !search.is_empty() {
            query += &format!("&search={}", enc(search));
        }
    }
    if !v["cursor"].is_null() {
        let cursor = v["cursor"]
            .as_str()
            .filter(|s| valid_live_cursor(s))
            .ok_or(CoreError::InvalidInput)?;
        query += &format!("&cursor={}", enc(cursor));
    }
    Ok(format!(
        "/api/v2/iptv/live/{}?{query}",
        if categories { "categories" } else { "channels" }
    ))
}

fn playback_v2_request(input: &Value) -> Result {
    let request: crate::dto::PlaybackV2Request =
        serde_json::from_value(input.clone()).map_err(|_| CoreError::InvalidInput)?;
    validate_playback_v2(&request)?;
    serde_json::to_value(request)
        .map(|v| snake(&v))
        .map_err(|_| CoreError::InvalidInput)
}

pub(crate) fn validate_playback_v2(
    request: &crate::dto::PlaybackV2Request,
) -> std::result::Result<(), CoreError> {
    if request.client.native_torrent.as_ref().is_some_and(|cap| {
        cap.version != 1
            || cap.network_policy != "public_dht_tcp_v1"
            || !matches!(
                request.client.platform,
                crate::dto::PlaybackPlatform::Android | crate::dto::PlaybackPlatform::AndroidTv
            )
    }) || request.request_id.is_empty()
        || request.request_id.len() > 128
        || !request
            .request_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        || request.stream_id.is_empty()
        || request.stream_id.len() > 128
        || !(0.0..=604800.0).contains(&request.position)
        || !(2..=16384).contains(&request.client.max_width)
        || !(2..=16384).contains(&request.client.max_height)
        || request.audio_track.is_some_and(|n| n > 65535)
        || request.subtitle_track.is_some_and(|n| n > 65535)
        || (request.subtitles_off
            && (request.subtitle_track.is_some() || request.preferred_subtitle_language.is_some()))
        || [
            &request.audio_language,
            &request.preferred_audio_language,
            &request.preferred_subtitle_language,
        ]
        .into_iter()
        .flatten()
        .any(|language| {
            language.is_empty()
                || language.len() > 35
                || !language
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        || [&request.client.video_codecs, &request.client.audio_codecs]
            .iter()
            .any(|list| {
                list.is_empty()
                    || list.len() > 16
                    || list.iter().any(|codec| {
                        codec.is_empty()
                            || codec.len() > 32
                            || !codec
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    })
            })
    {
        return Err(CoreError::InvalidInput);
    }
    Ok(())
}

/// Shared bridge from measured player facts/options to the canonical v2 request.
/// No source URL or arbitrary capability/quality extension gains authority.
pub(super) fn playback_v2_intent(v: &Value) -> Result {
    let playback = &v["playback"];
    let caps = &playback["capabilities"];
    if !playback["channelId"].is_null() {
        return Err(CoreError::InvalidInput);
    }
    let platform = match text(v, "platform") {
        "html5" => "web",
        "tauri" => "desktop",
        other => other,
    };
    let codecs = |field: &str, flags: &[(&str, &str)]| -> Value {
        let mut values: Vec<Value> = caps[field].as_array().cloned().unwrap_or_default();
        for (flag, codec) in flags {
            let value = json!(codec);
            if caps[*flag].as_bool() == Some(true) && !values.contains(&value) {
                values.push(value);
            }
        }
        json!(values)
    };
    let conversion = if playback["forceTranscode"].as_bool() == Some(true) {
        match playback["conversionReason"].as_str() {
            Some("audio-codec") => "audio",
            Some("video-codec" | "container" | "rendering" | "performance") => "video",
            None => "audio_video",
            _ => return Err(CoreError::InvalidInput),
        }
    } else {
        "auto"
    };
    let off = playback["subtitlesOff"].as_bool().unwrap_or(false);
    let preferences = &v["preferences"];
    let value = json!({
        "requestId":v["requestId"], "streamId":playback["streamId"],
        "client":{"platform":platform,"canPlayDirect":!matches!(platform,"roku"|"vizio") && ["directPlay","directFiles","directUrls"].iter().any(|key| caps[*key].as_bool()==Some(true)),
          "maxWidth":caps["maxWidth"],"maxHeight":caps["maxHeight"],
          "videoCodecs":codecs("directVideoCodecs", &[("h264","h264"),("hevc","hevc")]),
          "audioCodecs":codecs("directAudioCodecs", &[("aac","aac")])},
        "position":playback.get("position").cloned().unwrap_or(json!(0)),
        "forceGateway":playback["managedOnly"].as_bool().unwrap_or(false) || conversion!="auto",
        "conversion":conversion,"audioTrack":playback["audioTrackIndex"],
        "subtitleTrack":if off { Value::Null } else { playback["subtitleTrackIndex"].clone() },
        "audioLanguage":playback["audioLanguage"],"preferredAudioLanguage":preferences["audioLanguage"],
        "preferredSubtitleLanguage":if !off && preferences["subtitlesEnabled"].as_bool()==Some(true) {preferences["subtitleLanguage"].clone()} else {Value::Null},
        "subtitlesOff":off
    });
    playback_v2_request(&value)?;
    Ok(value)
}
