use super::*;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use viptv_engine::preferences::{
    self, PreferredSourceDecision, ProfilePreferences, RankingContext, RememberedSource,
};
use viptv_engine::source_integration::VsourcesConfiguration;

type SdkSource = (String, Value, VsourcesConfiguration, [u8; 32], u64);
#[derive(Clone)]
pub(crate) struct Context {
    memory: RememberedSource,
    preferences: ProfilePreferences,
    ranking: RankingContext,
}
#[derive(Default)]
pub(crate) struct Attempt {
    pub started: bool,
    pub sdk_excluded: std::collections::HashMap<String, Vec<String>>,
    pub iptv_excluded: Option<i64>,
}
pub(crate) fn validate(value: &Value) -> Result<(), ApiError> {
    for field in ["remembered_source", "cross_title_source"] {
        if let Some(raw) = value.get(field).filter(|v| !v.is_null()) {
            let memory: RememberedSource = serde_json::from_value(raw.clone())
                .map_err(|_| ApiError::from("Invalid source selection"))?;
            memory
                .validate()
                .map_err(|_| ApiError::from("Invalid source selection"))?;
        }
    }
    if let Some(height) = value.get("device_max_height") {
        if !height.as_u64().is_some_and(|h| (144..=4320).contains(&h)) {
            return Err("Invalid source selection".into());
        }
    }
    if value
        .get("exclude_preferred_source")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err("Invalid source selection".into());
    }
    Ok(())
}
pub(crate) async fn context(app: &App, value: &Value) -> Result<Option<Context>, ApiError> {
    let read = |key| {
        value
            .get(key)
            .filter(|v| !v.is_null())
            .map(|raw| {
                serde_json::from_value::<RememberedSource>(raw.clone())
                    .map_err(|_| ApiError::from("Invalid source selection"))
            })
            .transpose()
    };
    let title = read("remembered_source")?;
    let across = read("cross_title_source")?;
    if title.is_none() && across.is_none() {
        return Ok(None);
    }
    let lease = app.request_lease();
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = lease.principal
    else {
        return Err(auth::forbidden());
    };
    let snapshot = super::preferences::get(State(app.clone()), Extension(lease), Path(profile))
        .await?
        .0;
    let snapshot: viptv_engine::preferences::PreferencesSnapshot =
        serde_json::from_value(snapshot).map_err(|_| ApiError::from("Preferences unavailable"))?;
    let memory = preferences::preferred_source_memory(
        title.as_ref(),
        across.as_ref(),
        &snapshot.preferences,
    )
    .map_err(|_| ApiError::from("Invalid source selection"))?;
    Ok(memory.map(|memory| Context {
        memory,
        preferences: snapshot.preferences,
        ranking: RankingContext::for_device_height(
            value["device_max_height"].as_u64().map(|h| h as u32),
        ),
    }))
}
pub(crate) fn authorized(app: &App, job: &Job) -> bool {
    !job.cancelled.load(Ordering::Acquire)
        && app
            .request_lease()
            .validate(&app.db.lock().unwrap())
            .is_ok()
        && job
            .configuration_revision
            .is_none_or(|(profile, revision)| {
                configuration::validate_revision(app, profile, revision).is_ok()
            })
}
pub(crate) fn phase(job: &Job, value: &str) {
    let mut state = job.state.lock().unwrap();
    let seq = state.events.len() + 1;
    state
        .events
        .push(json!({"seq":seq,"phase":value,"streams":[]}));
    drop(state);
    job.notify.notify_waiters();
}
fn ready(context: &Context, producer: &str, streams: &[Value]) -> bool {
    let mut rows = streams
        .iter()
        .enumerate()
        .map(|(i, card)| super::source_ranking::candidate_from_card(i.to_string(), producer, card))
        .collect::<Vec<_>>();
    preferences::rank_sources(&mut rows, &context.preferences, &context.ranking);
    preferences::reusable_source_context(
        &rows,
        Some(&context.memory),
        None,
        &context.preferences,
        &context.ranking,
    )
    .is_ok_and(|result| result.ready && result.preferred)
}
#[cfg(test)]
fn published(job: &Job, producer: &str) -> bool {
    job.state
        .lock()
        .unwrap()
        .events
        .iter()
        .rev()
        .find(|v| v["source"] == producer)
        .is_some_and(|v| v["streams"].as_array().is_some_and(|s| !s.is_empty()))
}
fn ready_published(context: &Context, job: &Job, producer: &str) -> bool {
    let cards = job
        .state
        .lock()
        .unwrap()
        .events
        .iter()
        .rev()
        .find(|event| event["source"] == producer)
        .and_then(|event| event["streams"].as_array())
        .cloned()
        .unwrap_or_default();
    ready(context, producer, &cards)
}
pub(crate) async fn attempt(
    app: &App,
    job: &Arc<Job>,
    value: &Value,
    sdk: &[SdkSource],
    addons: &mut Vec<(i64, String, Value)>,
    context: Context,
) -> Attempt {
    let mut outcome = Attempt::default();
    let exclude_only = value["exclude_preferred_source"].as_bool() == Some(true);
    if !exclude_only {
        phase(job, "preferred");
    }
    let source_proof =
        crate::sources::source_configuration(&app.db.lock().unwrap(), &context.memory.provider_id)
            .ok()
            .flatten();
    let metadata_revision = app.addons.revision().ok();
    let valid_attempt = || {
        authorized(app, job)
            && crate::sources::source_configuration(
                &app.db.lock().unwrap(),
                &context.memory.provider_id,
            )
            .ok()
            .flatten()
                == source_proof
            && app.addons.revision().ok() == metadata_revision
    };
    let begun = Instant::now();
    let ready_flag = Arc::new(AtomicBool::new(false));
    let ready_at = AtomicU64::new(u64::MAX);
    let notify = Arc::new(tokio::sync::Notify::new());
    let budget = Duration::from_millis(preferences::PREFERRED_SOURCE_BUDGET_MS);
    let mut consumed_addon = false;
    {
        let run = async {
            if let Some((source, _, config, proof, revision)) = sdk
                .iter()
                .find(|(source, ..)| source == &context.memory.provider_id)
            {
                let Ok(Some(supplier)) =
                    viptv_engine::vsources_runtime::preferred_supplier(config, &context.memory)
                else {
                    return;
                };
                outcome
                    .sdk_excluded
                    .insert(source.clone(), vec![supplier.clone()]);
                if exclude_only {
                    return;
                }
                let base = source_integrations::authorization(app, job, source, *proof, *revision);
                let active = Arc::new(AtomicBool::new(true));
                struct Fence(Arc<AtomicBool>);
                impl Drop for Fence {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let _fence = Fence(active.clone());
                let authorize: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
                    active.load(Ordering::Acquire) && begun.elapsed() < budget && base()
                });
                let Some(card) = source_integrations::canonical_card(
                    app,
                    value,
                    &app.request_lease(),
                    *revision,
                    job.metadata_proof.as_ref(),
                )
                .await
                else {
                    return;
                };
                let Ok(media) = viptv_engine::vsources_runtime::media_ref(
                    &card.identity,
                    card.parent_identity.as_ref(),
                    &[],
                ) else {
                    return;
                };
                let metadata = source_integrations::sdk_metadata(app, &card, &media, *revision);
                tracing::info!(elapsed_ms = begun.elapsed().as_millis() as u64, "Preferred source metadata verified");
                let mut selected = context.clone();
                selected.memory.source_id = Some(supplier.clone());
                let _ = viptv_engine::vsources_runtime::resolve_preferred_progressive(
                    config.clone(),
                    media,
                    supplier,
                    context.memory.audio_languages.first().cloned(),
                    metadata,
                    authorize.clone(),
                    app.cloudflare.clone(),
                    |streams| {
                        if !authorize() {
                            return;
                        }
                        tracing::info!(elapsed_ms = begun.elapsed().as_millis() as u64, streams = streams.len(), "Preferred supplier batch received");
                        let usable = streams
                            .into_iter()
                            .filter(|stream| {
                                let candidate = viptv_engine::vsources_runtime::candidate(
                                    "preferred".into(),
                                    source,
                                    stream,
                                );
                                preferences::reusable_source_context(
                                    &[candidate],
                                    Some(&selected.memory),
                                    None,
                                    &selected.preferences,
                                    &selected.ranking,
                                )
                                .is_ok_and(|result| result.ready && result.preferred)
                            })
                            .collect::<Vec<_>>();
                        if usable.is_empty() {
                            return;
                        }
                        source_integrations::publish_batch(
                            app, job, source, *proof, &authorize, usable,
                        );
                        if ready_published(&selected, job, source) && begun.elapsed() < budget {
                            ready_at.store(begun.elapsed().as_millis() as u64, Ordering::Release);
                            ready_flag.store(true, Ordering::Release);
                            tracing::info!(elapsed_ms = begun.elapsed().as_millis() as u64, "Preferred source ready");
                            notify.notify_one();
                        }
                    },
                )
                .await;
            } else if let Some(index) = addons
                .iter()
                .position(|(id, ..)| format!("addon:{id}") == context.memory.provider_id)
            {
                let (id, url, _) = addons.remove(index);
                consumed_addon = true;
                if exclude_only {
                    return;
                }
                let result = app
                    .addons
                    .streams(
                        &url,
                        &value["type"].as_str().unwrap_or("movie"),
                        value["id"].as_str().unwrap_or(""),
                    )
                    .await;
                if !valid_attempt() || begun.elapsed() >= budget {
                    return;
                }
                if let Ok(streams) = result {
                    let producer = format!("addon:{id}");
                    crate::handlers_media::emit_batch_expected(
                        app,
                        job,
                        &producer,
                        Ok(streams),
                        false,
                        source_proof,
                    );
                    if ready_published(&context, job, &producer) && begun.elapsed() < budget {
                        ready_at.store(begun.elapsed().as_millis() as u64, Ordering::Release);
                        ready_flag.store(true, Ordering::Release);
                    }
                }
            } else if let Some(id) = context
                .memory
                .provider_id
                .strip_prefix("iptv:")
                .and_then(|id| id.parse::<i64>().ok())
            {
                outcome.iptv_excluded = Some(id);
                if exclude_only {
                    return;
                }
                let mut request = value.clone();
                request["only_provider_id"] = json!(id);
                let _ = app
                    .providers
                    .stream_batches(request, |producer, result| {
                        if !valid_attempt() || begun.elapsed() >= budget {
                            return;
                        }
                        crate::handlers_media::emit_batch_expected(
                            app,
                            job,
                            &producer,
                            result,
                            false,
                            source_proof,
                        );
                        if ready_published(&context, job, &producer) && begun.elapsed() < budget {
                            ready_at.store(begun.elapsed().as_millis() as u64, Ordering::Release);
                            ready_flag.store(true, Ordering::Release);
                            notify.notify_one();
                        }
                    })
                    .await;
            }
        };
        tokio::pin!(run);
        let cancelled = async {
            loop {
                if !valid_attempt() {
                    job.cancelled.store(true, Ordering::Release);
                    return;
                }
                tokio::select! {_=job.notify.notified()=>{},_=tokio::time::sleep(Duration::from_millis(250))=>{}}
            }
        };
        let _ = tokio::time::timeout(budget, async {
            tokio::select! { _=&mut run=>{}, _=notify.notified()=>{}, _=cancelled=>{} }
        })
        .await;
    }
    if consumed_addon {
        let mut state = job.state.lock().unwrap();
        state.pending = state.pending.saturating_sub(1);
    }
    let elapsed = if ready_flag.load(Ordering::Acquire) {
        ready_at.load(Ordering::Acquire)
    } else {
        begun.elapsed().as_millis() as u64
    };
    if !valid_attempt() {
        job.cancelled.store(true, Ordering::Release);
    }
    tracing::info!(
        elapsed_ms = begun.elapsed().as_millis() as u64,
        ready = ready_flag.load(Ordering::Acquire),
        "Preferred discovery finished"
    );
    outcome.started = preferences::preferred_source_decision(
        ready_flag.load(Ordering::Acquire),
        true,
        elapsed,
        !valid_attempt(),
    ) == PreferredSourceDecision::Start;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_integration_tests::fixture;
    async fn setup(
        slow: bool,
    ) -> (
        App,
        Arc<Job>,
        Vec<(i64, String, Value)>,
        Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let others = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let routes=Router::new().route("/a/:provider/stream/:kind/:id",get({let others=others.clone();move |Path((provider,_,_)):Path<(i64,String,String)>|{let others=others.clone();async move{
            if provider!=201{others.fetch_add(1,Ordering::SeqCst);tokio::time::sleep(Duration::from_secs(30)).await;}
            if slow{tokio::time::sleep(Duration::from_secs(30)).await;}
            axum::Json(json!({"streams":[{"url":"https://media.example/preferred.mp4","name":"Provider A","quality_height":720,"audio_languages":["en"],"presentation":{"provider_name":"Provider A"}}]}))
        }}})).fallback(||async{StatusCode::OK});
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        let mut app = fixture();
        app.addons.allow_test_loopback = true;
        let mut addons = Vec::new();
        for id in 201..231 {
            let manifest = json!({"id":format!("source{id}"),"name":"Provider A","resources":["stream"],"types":["movie"]});
            let url = format!("http://{address}/a/{id}/manifest.json");
            app.db.lock().unwrap().execute("INSERT INTO addons(id,name,manifest_url,manifest,account_id,enabled) VALUES(?1,'Provider A',?2,?3,1,1)",params![id,url,manifest.to_string()]).unwrap();
            addons.push((id, url, manifest));
        }
        crate::test_support::encrypt_fixture_sources(&app);
        let app = app.with_lease(ResourceLease {
            policy_revision: 0,
            principal: auth::Principal::Account {
                account_id: 1,
                role: "owner".into(),
                profile_id: Some(1),
                session_id: Some("s1".into()),
            },
            session_id: Some("s1".into()),
        });
        let job = Arc::new(Job {
            metadata_proof: None,
            source_slots: std::sync::atomic::AtomicUsize::new(0),
            cancelled: AtomicBool::new(false),
            configuration_revision: None,
            media_id: Some("tt123".into()),
            exact_vod: None,
            kind: "movie".into(),
            created: Instant::now(),
            state: Mutex::new(JobState {
                events: vec![],
                pending: 30,
            }),
            notify: Notify::new(),
        });
        (app, job, addons, others, task)
    }
    fn context() -> Context {
        Context {
            memory: RememberedSource {
                provider_id: "addon:201".into(),
                source_id: None,
                provider_name: Some("Provider A".into()),
                audio_languages: vec!["en".into()],
            },
            preferences: ProfilePreferences::default(),
            ranking: RankingContext::for_device_height(Some(1080)),
        }
    }
    #[tokio::test]
    async fn preferred_provider_starts_without_requesting_the_other_twenty_nine() {
        let (app, job, mut addons, others, server) = setup(false).await;
        let outcome = attempt(
            &app,
            &job,
            &json!({"type":"movie","id":"tt123"}),
            &[],
            &mut addons,
            context(),
        )
        .await;
        assert!(outcome.started);
        assert_eq!(others.load(Ordering::SeqCst), 0);
        assert_eq!(addons.len(), 29);
        assert!(!addons.iter().any(|(id, ..)| *id == 201));
        assert!(published(&job, "addon:201"));
        server.abort();
    }
    #[tokio::test]
    async fn cancellation_drops_preferred_request_without_fallback_or_late_publication() {
        let (app, job, mut addons, others, server) = setup(true).await;
        let cancel = job.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancelled.store(true, Ordering::Release);
            cancel.notify.notify_waiters();
        });
        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            attempt(
                &app,
                &job,
                &json!({"type":"movie","id":"tt123"}),
                &[],
                &mut addons,
                context(),
            ),
        )
        .await
        .unwrap();
        assert!(!outcome.started);
        assert_eq!(others.load(Ordering::SeqCst), 0);
        assert!(!published(&job, "addon:201"));
        server.abort();
    }
    #[tokio::test]
    async fn changed_source_configuration_cancels_preferred_work() {
        let (app, job, mut addons, others, server) = setup(true).await;
        let changed = app.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            changed
                .db
                .lock()
                .unwrap()
                .execute("UPDATE addons SET enabled=0 WHERE id=201", [])
                .unwrap();
        });
        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            attempt(
                &app,
                &job,
                &json!({"type":"movie","id":"tt123"}),
                &[],
                &mut addons,
                context(),
            ),
        )
        .await
        .unwrap();
        assert!(!outcome.started);
        assert!(job.cancelled.load(Ordering::Acquire));
        assert_eq!(others.load(Ordering::SeqCst), 0);
        assert!(!published(&job, "addon:201"));
        server.abort();
    }
    #[tokio::test]
    async fn preferred_request_deadline_does_not_dispatch_fallback_early() {
        let (app, job, mut addons, others, server) = setup(true).await;
        let start = Instant::now();
        let outcome = attempt(
            &app,
            &job,
            &json!({"type":"movie","id":"tt123"}),
            &[],
            &mut addons,
            context(),
        )
        .await;
        assert!(!outcome.started);
        assert!(start.elapsed() >= Duration::from_millis(4900));
        assert!(start.elapsed() < Duration::from_secs(10));
        assert_eq!(others.load(Ordering::SeqCst), 0);
        assert!(!addons.iter().any(|(id, ..)| *id == 201));
        server.abort();
    }
}
