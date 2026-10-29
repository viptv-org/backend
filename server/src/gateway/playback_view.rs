use super::*;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReplaceSource {
    playback_id: String,
    observed_position: f64,
}
impl ReplaceSource {
    pub(super) fn valid(&self) -> bool {
        protocol::identifier(&self.playback_id)
            && self.observed_position.is_finite()
            && (0.0..=9007199254740991.0).contains(&self.observed_position)
    }
}

fn bound(proof: &Proof) -> bool {
    proof.metadata_proof.is_some()
        || proof.exact_vod.is_some()
        || proof.media_id.as_ref().is_some_and(|id| !id.is_empty())
        || proof.live_channel_id.is_some()
}
fn same_target(a: &Proof, b: &Proof) -> bool {
    if a.kind != b.kind {
        return false;
    }
    match (&a.metadata_proof, &b.metadata_proof) {
        (Some(a), Some(b)) => {
            return a.card.identity == b.card.identity
                && a.card.parent_identity == b.card.parent_identity
                && a.card.metadata_origin == b.card.metadata_origin
        }
        (None, None) => {}
        _ => return false,
    }
    match (&a.exact_vod, &b.exact_vod) {
        (Some(a), Some(b)) => return a == b,
        (None, None) => {}
        _ => return false,
    }
    if let (Some(a), Some(b)) = (&a.live_channel_id, &b.live_channel_id) {
        return a == b;
    }
    a.producer == b.producer
        && a.media_id
            .as_ref()
            .is_some_and(|id| !id.is_empty() && Some(id) == b.media_id.as_ref())
}
pub(super) fn actions(entry: &Entry, state: &StateData) -> Value {
    let ready = state.status == "ready" && !expired(state);
    let episode = entry
        .proof
        .metadata_proof
        .as_ref()
        .is_some_and(|proof| proof.card.identity.episode.is_some())
        || entry
            .proof
            .exact_vod
            .as_ref()
            .is_some_and(|vod| vod.series.is_some() && vod.episode.is_some());
    let selectable = |key: &str| {
        state
            .delivery
            .as_ref()
            .and_then(|d| d[key].as_array())
            .into_iter()
            .flatten()
            .any(|track| {
                track["selectable"] == true
                    && track["supported"] != false
                    && track["input_index"]
                        .as_u64()
                        .is_some_and(|index| index <= 65535)
            })
    };
    let subtitles = state
        .delivery
        .as_ref()
        .is_some_and(|d| d["subtitles_supported"] == true);
    json!({
        "pause":ready && !entry.live,
        "seek":ready && !entry.live,
        "restart":ready && !entry.live,
        "change_source":ready && bound(&entry.proof),
        "episodes":ready && !entry.live && episode,
        "next_episode":ready && !entry.live && episode && entry.next_episode_button,
        "tracks":ready && bound(&entry.proof) && (selectable("audio_tracks") || selectable("subtitle_tracks") || subtitles),
        "subtitle_style":ready && bound(&entry.proof) && subtitles && entry.caption_style,
        "report_progress":!entry.live && matches!(entry.proof.kind.as_str(),"movie"|"series")
    })
}

pub(super) async fn replace(
    app: &App,
    lease: &ResourceLease,
    input: &Source,
    request: &mut Start,
) -> Result<(), Error> {
    let Some(intent) = &request.replace_source else {
        return Ok(());
    };
    if !input.live && intent.observed_position > 604800.0 {
        return Err(Error::Code("playback_replacement_invalid"));
    }
    let previous = app.gateway_playbacks.snapshot(&intent.playback_id, lease)?;
    {
        let state = previous.state.lock().unwrap();
        if !matches!(state.status, "ready" | "released")
            || state.touched.elapsed() >= LEASE
            || previous.live != input.live
            || !same_target(&previous.proof, &input.proof)
        {
            return Err(Error::Code("playback_replacement_invalid"));
        }
    }
    let db = app.db.clone();
    let current_lease = lease.clone();
    let next = input.proof.clone();
    tokio::task::spawn_blocking(move || {
        let db = db
            .lock()
            .map_err(|_| Error::Code("provider_storage_unavailable"))?;
        current_lease.validate(&db)?;
        previous.lease.validate(&db)?;
        validate_source(&db, &previous.proof)?;
        validate_source(&db, &next)?;
        Ok::<_, Error>(())
    })
    .await
    .map_err(|_| Error::Code("provider_storage_unavailable"))??;
    request.position = if input.live {
        0.0
    } else {
        intent.observed_position
    };
    request.resume = false;
    request.replace_source = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_shape_is_bounded_and_old_serialization_is_unchanged() {
        let base = json!({"request_id":"fixture","stream_id":"source","client":{"platform":"web","max_width":1920,"max_height":1080,"video_codecs":["h264"],"audio_codecs":["aac"]}});
        let request: Start = serde_json::from_value(base.clone()).unwrap();
        assert!(serde_json::to_value(request)
            .unwrap()
            .get("replace_source")
            .is_none());
        for (id, position, valid) in [
            ("lease", 0.0, true),
            ("lease", 604800.0, true),
            ("lease", -1.0, false),
            ("lease", 604801.0, true),
            ("lease", 9007199254740992.0, false),
            ("../lease", 1.0, false),
        ] {
            assert_eq!(
                ReplaceSource {
                    playback_id: id.into(),
                    observed_position: position
                }
                .valid(),
                valid
            );
        }
    }
}
