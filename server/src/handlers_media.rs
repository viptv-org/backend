use super::*;

pub(crate) fn emit(a: &App, j: &Job, source: &str, result: Result<Vec<Value>, String>) {
    emit_batch(a, j, source, result, true);
}
pub(crate) fn emit_batch(
    a: &App,
    j: &Job,
    source: &str,
    result: Result<Vec<Value>, String>,
    complete: bool,
) {
    if a.request_lease().validate(&a.db.lock().unwrap()).is_err() {
        let mut state = j.state.lock().unwrap();
        state.events.clear();
        state.pending = 0;
        drop(state);
        j.notify.notify_waiters();
        return;
    }
    if let Some(id) = source
        .strip_prefix("iptv:")
        .and_then(|id| id.parse::<i64>().ok())
    {
        if a.providers
            .require_owner(&a.db.lock().unwrap(), id)
            .is_err()
        {
            // An ownership change during a queued lookup must not publish even
            // the previous provider name or source identifier.
            emit_batch(
                a,
                j,
                "iptv",
                Err("This IPTV source is no longer available in your account.".into()),
                complete,
            );
            return;
        }
    }
    if a.providers.account.is_some() {
        if let Some(id) = source
            .strip_prefix("addon:")
            .and_then(|id| id.parse::<i64>().ok())
        {
            let allowed = addon::Addons::available(
                &a.db.lock().unwrap(),
                a.identity().account_id().unwrap_or(0),
                id,
            );
            if !allowed {
                emit_batch(a, j, "addon", Err("source_not_found".into()), complete);
                return;
            }
        }
    }
    let (streams, error, registration_error) = match result {
        Ok(r) => {
            let (streams, error) = a.register(source, r, &j.kind);
            {
                let mut entries = a.streams.lock().unwrap();
                for source in &streams {
                    if let Some(entry) = source["id"].as_str().and_then(|id| entries.get_mut(id)) {
                        entry.exact_vod = j.exact_vod.clone();
                    }
                }
            }
            (streams, error, true)
        }
        Err(e) => (vec![], Some(e), false),
    };
    let mut state = j.state.lock().unwrap();
    let seq = state.events.len() + 1;
    let mut e = json!({"seq":seq,"source":source,"streams":streams});
    if let Some(error) = error {
        if a.providers.account.is_some() {
            let code = if registration_error {
                service_errors::provider(&error).unwrap_or("source_format_unsupported")
            } else {
                service_errors::discovery(source, &error)
            };
            e["error"] = json!(account_api::description(code));
            e["error_code"] = json!(code);
        } else {
            e["error"] = json!(error);
        }
    }
    state.events.push(e);
    if complete {
        state.pending = state.pending.saturating_sub(1);
    }
    drop(state);
    j.notify.notify_waiters();
}
pub(crate) fn job(a: &App, id: &str) -> Result<Arc<Job>, ApiError> {
    a.prune();
    a.jobs.lock().unwrap().get(id).cloned().ok_or(ApiError(
        StatusCode::NOT_FOUND,
        "Discovery job expired or not found".into(),
    ))
}
