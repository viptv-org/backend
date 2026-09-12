//! One viewer-owned live identity across bounded input generations. The media
//! engine retains responsibility for processes and connection reservations.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) struct LiveSession {
    cancelled: AtomicBool,
    finished: AtomicBool,
    state: Mutex<LiveState>,
}
struct LiveState {
    engine: String,
    generation: u64,
    status: &'static str,
    playback: Value,
    touched: Instant,
    requested: bool,
    reason: Option<&'static str>,
}
pub(super) fn register(
    a: App,
    options: PlaybackRequest,
    channel: String,
    inputs: FamilyInputs,
    response: &mut Value,
) {
    let id = response["id"].as_str().unwrap().to_owned();
    response["managed_live"] = json!(true);
    response["generation"] = json!(1);
    response["channel_id"] = json!(channel);
    let live = Arc::new(LiveSession {
        cancelled: AtomicBool::new(false),
        finished: AtomicBool::new(false),
        state: Mutex::new(LiveState {
            engine: id.clone(),
            generation: 1,
            status: "playing",
            playback: response.clone(),
            touched: Instant::now(),
            requested: false,
            reason: None,
        }),
    });
    a.live_sessions
        .lock()
        .unwrap()
        .insert(id.clone(), live.clone());
    tokio::spawn(run(a, live, id, options, channel, inputs));
}
pub(super) async fn heartbeat(a: &App, id: &str, recover: Option<u64>) -> Option<ApiResult> {
    let live = a.live_sessions.lock().unwrap().get(id).cloned()?;
    if let Err(error) = validate_request(a).await {
        return Some(Err(error));
    }
    let (engine, response) = {
        let mut state = live.state.lock().unwrap();
        state.touched = Instant::now();
        if recover == Some(state.generation) && state.status == "playing" {
            state.requested = true;
        }
        (
            state.engine.clone(),
            json!({"ok":true,"managed_live":true,"state":state.status,"reason":state.reason,"generation":state.generation,"playback":if state.status=="playing"{state.playback.clone()}else{Value::Null}}),
        )
    };
    a.playback.heartbeat(&engine).await;
    Some(Ok(axum::Json(response)))
}
pub(super) async fn stop(a: &App, id: &str) -> bool {
    let Some(live) = a.live_sessions.lock().unwrap().get(id).cloned() else {
        return false;
    };
    live.cancelled.store(true, Ordering::Release);
    while !live.finished.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    true
}
async fn cancelled(live: &LiveSession) {
    while !live.cancelled.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
struct Completion(Arc<LiveSession>);
impl Drop for Completion {
    fn drop(&mut self) {
        self.0.finished.store(true, Ordering::Release);
    }
}
async fn run(
    a: App,
    live: Arc<LiveSession>,
    id: String,
    options: PlaybackRequest,
    channel: String,
    mut inputs: FamilyInputs,
) {
    let _completion = Completion(live.clone());
    let mut a = a;
    let mut progress = None;
    let mut advanced = Instant::now();
    let mut recoveries = 0;
    loop {
        tokio::select! {_=tokio::time::sleep(Duration::from_millis(250))=>{},_=cancelled(&live)=>break}
        let (engine, requested, expired, failed) = {
            let state = live.state.lock().unwrap();
            (
                state.engine.clone(),
                state.requested,
                state.touched.elapsed() > a.playback.session_ttl(),
                state.status == "failed",
            )
        };
        if shared::worker_known(&a, &id) {
            if let Some(owner) = shared::worker_access(&a, &id) {
                a = owner;
            } else {
                break;
            }
        }
        let authorized =
            tokio::select! { result=validate_request(&a)=>result, _=cancelled(&live)=>break };
        if expired || a.playback.is_shutting_down() || authorized.is_err() {
            break;
        }
        if failed {
            continue;
        }
        let running = a.playback.input_running(&engine).await;
        let current = a.playback.live_progress(&engine).await;
        if current.is_some() && current != progress {
            progress = current;
            advanced = Instant::now();
        }
        let stalled = advanced.elapsed() >= Duration::from_secs(inputs.policy.stall_seconds);
        if !requested && running && !stalled {
            continue;
        }
        {
            let mut state = live.state.lock().unwrap();
            state.status = "recovering";
            state.requested = false;
            state.reason = Some(if requested {
                "player_failed"
            } else if stalled {
                "media_stalled"
            } else {
                "input_ended"
            });
        }
        crate::activity::playback_event(
            &a,
            &channel,
            if requested {
                "player_failed"
            } else if stalled {
                "media_stalled"
            } else {
                "input_ended"
            },
            live.state.lock().unwrap().generation,
        );
        if let Some(observation) = inputs.current.take() {
            let _ = a.providers.record_family_input(&observation, false, 0);
        }
        // Always finish the failed input before using its account allowance.
        a.playback.stop(&engine).await;
        if engine != id {
            a.resource_owners
                .lock()
                .unwrap()
                .remove(&format!("playback:{engine}"));
        }
        if recoveries >= inputs.policy.max_recoveries {
            let mut state = live.state.lock().unwrap();
            state.status = "failed";
            state.reason = Some("recovery_budget_exhausted");
            continue;
        }
        recoveries += 1;
        let deadline = Duration::from_secs(inputs.policy.deadline_seconds);
        let preparation = async {
            let response =
                prepare_family(a.clone(), &options, channel.clone(), &mut inputs).await?;
            let published = publish(a.clone(), response, None, true).await?;
            Ok::<_, ApiError>(published.0)
        };
        let result = tokio::select! {
            biased;
            _=cancelled(&live)=>break,
            error=revoked(&a)=>{let _=error;break;},
            result=tokio::time::timeout(deadline,preparation)=>result,
        };
        match result {
            Ok(Ok(mut response)) => {
                let engine = response["id"].as_str().unwrap().to_owned();
                if live.cancelled.load(Ordering::Acquire) {
                    a.playback.stop(&engine).await;
                    break;
                }
                let mut state = live.state.lock().unwrap();
                state.generation += 1;
                response["id"] = json!(id);
                response["managed_live"] = json!(true);
                response["generation"] = json!(state.generation);
                response["channel_id"] = json!(channel);
                response["candidate_id"] =
                    json!(inputs.current.as_ref().map(|key| key.candidate.clone()));
                response["_source_key"] =
                    json!(inputs.current.as_ref().map(|key| key.source.clone()));
                state.engine = engine;
                state.playback = response;
                state.status = "playing";
                crate::activity::playback_event(
                    &a,
                    &channel,
                    "recovery_succeeded",
                    state.generation,
                );
                state.reason = None;
                progress = None;
                advanced = Instant::now();
            }
            result => {
                let reason = match result {
                    Err(_) => "recovery_deadline_exceeded",
                    Ok(Err(ApiError(StatusCode::TOO_MANY_REQUESTS, _))) => "connections_busy",
                    _ => "no_playable_backup",
                };
                a.playback.settle_cancelled_inputs().await;
                let mut state = live.state.lock().unwrap();
                state.status = "failed";
                state.reason = Some(reason);
            }
        }
    }
    let engine = live.state.lock().unwrap().engine.clone();
    a.playback.stop(&engine).await;
    a.playback.settle_cancelled_inputs().await;
    a.resource_owners
        .lock()
        .unwrap()
        .remove(&format!("playback:{engine}"));
    a.resource_owners
        .lock()
        .unwrap()
        .remove(&format!("playback:{id}"));
    a.live_sessions.lock().unwrap().remove(&id);
}

pub(super) fn snapshot(a: &App, id: &str) -> Option<Value> {
    let live = a.live_sessions.lock().unwrap().get(id).cloned()?;
    let state = live.state.lock().unwrap();
    let mut value = state.playback.clone();
    value["worker_state"] = json!(state.status);
    Some(value)
}
