//! Test-only control peer with real FFmpeg output; not a production gateway implementation.
use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct Peer {
    source: String,
    root: PathBuf,
    key: String,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}
struct Session {
    token: String,
    status: String,
    expires: u64,
    duration: f64,
    directory: PathBuf,
}
fn now() -> u64 {
    crate::util::now() as u64
}
fn session(id: &str, s: &Session) -> Value {
    json!({"id":id,"status":s.status,"expires_at":s.expires,"playback":{
        "id":id,"url":format!("/media/{id}/{}/index.m3u8",s.token),"format":"hls",
        "mode":"remux","video_mode":"copy","audio_mode":"none","position":0,
        "duration":s.duration,"live":false,"subtitles_supported":false}})
}
pub(super) fn router(source: String, root: PathBuf, key: String) -> Router {
    Router::new().fallback(any(handle)).with_state(Peer {
        source,
        root,
        key,
        sessions: Default::default(),
    })
}
async fn handle(State(peer): State<Peer>, request: axum::extract::Request) -> Response {
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    if path.starts_with("/media/") {
        return media(&peer, &path).await;
    }
    if request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        != Some(format!("Bearer {}", peer.key).as_str())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if path == "/v1/capabilities" {
        if method != axum::http::Method::GET {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        return Json(json!({"version":1,"ready":true,"torrent":false,"protocols":["hls"],"namespaces":["browser-qa"],
            "scopes":["capabilities","create","read","renew","release"],"available":{"inputs":2,"outputs":2,"viewers":2}})).into_response();
    }
    if path == "/v1/sessions" && method == axum::http::Method::POST {
        let bytes = axum::body::to_bytes(request.into_body(), 16_384)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        // This peer accepts exactly the disposable clip, not arbitrary caller input.
        if body["namespace"] != "browser-qa"
            || body["input"]["url"] != peer.source
            || body["input"]["live"] != false
            || body["output"]["protocol"] != "hls"
            || body["position_seconds"].as_f64() != Some(0.0)
            || !body["output"]["video_codecs"]
                .as_array()
                .is_some_and(|v| v.iter().any(|c| c == "h264"))
        {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let id = uuid::Uuid::new_v4().to_string();
        let directory = peer.root.join(&id);
        std::fs::create_dir(&directory).unwrap();
        let source = peer.source.clone();
        let output = directory.clone();
        let produced = tokio::task::spawn_blocking(move || {
            let result = std::process::Command::new("/usr/bin/ffmpeg")
                .args([
                    "-nostdin",
                    "-v",
                    "error",
                    "-y",
                    "-i",
                    &source,
                    "-map",
                    "0:v:0",
                    "-an",
                    "-c:v",
                    "copy",
                    "-f",
                    "hls",
                    "-hls_time",
                    "2",
                    "-hls_list_size",
                    "0",
                    "-hls_segment_filename",
                ])
                .arg(output.join("segment-%03d.ts"))
                .arg(output.join("index.m3u8"))
                .output()
                .unwrap();
            assert!(result.status.success(), "Disposable FFmpeg remux failed");
            let probe = std::process::Command::new("/usr/bin/ffprobe")
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "format=duration",
                    "-of",
                    "default=noprint_wrappers=1:nokey=1",
                ])
                .arg(output.join("index.m3u8"))
                .output()
                .unwrap();
            assert!(probe.status.success());
            let duration: f64 = String::from_utf8(probe.stdout)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert!(duration > 20.0 && output.join("segment-000.ts").metadata().unwrap().len() > 0);
            duration
        })
        .await
        .unwrap();
        let state = Session {
            token: uuid::Uuid::new_v4().simple().to_string(),
            status: "ready".into(),
            expires: now() + 60,
            duration: produced,
            directory,
        };
        let response = session(&id, &state);
        peer.sessions.lock().unwrap().insert(id, state);
        return (StatusCode::CREATED, Json(response)).into_response();
    }
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    if (parts.len() == 3 || (parts.len() == 4 && parts[3] == "renew"))
        && parts[..2] == ["v1", "sessions"]
    {
        let mut sessions = peer.sessions.lock().unwrap();
        let Some(state) = sessions.get_mut(parts[2]) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if parts.len() == 3 && method == axum::http::Method::DELETE {
            if state.status != "released" {
                std::fs::remove_dir_all(&state.directory).unwrap();
                state.status = "released".into();
            }
            return StatusCode::NO_CONTENT.into_response();
        }
        if parts.len() == 4 {
            if method != axum::http::Method::POST {
                return StatusCode::METHOD_NOT_ALLOWED.into_response();
            }
            if state.status != "ready" || state.expires <= now() {
                return StatusCode::GONE.into_response();
            }
            state.expires = now() + 60;
        } else if method != axum::http::Method::GET {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        return Json(session(parts[2], state)).into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}
async fn media(peer: &Peer, path: &str) -> Response {
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 5
        || !parts[4]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
        || parts[4].contains("..")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let file = {
        let sessions = peer.sessions.lock().unwrap();
        let Some(state) = sessions.get(parts[2]) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if state.token != parts[3] || state.status != "ready" || state.expires <= now() {
            return StatusCode::FORBIDDEN.into_response();
        }
        state.directory.join(parts[4])
    };
    let Ok(bytes) = tokio::fs::read(file).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        "content-type",
        if parts[4].ends_with(".m3u8") {
            "application/vnd.apple.mpegurl"
        } else {
            "video/mp2t"
        }
        .parse()
        .unwrap(),
    );
    headers.insert("cache-control", "private, no-store".parse().unwrap());
    (headers, Body::from(bytes)).into_response()
}
