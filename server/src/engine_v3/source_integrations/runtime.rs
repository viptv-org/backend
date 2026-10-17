use super::*;
use std::sync::Arc;
use viptv_engine::presentation::{Card, DetailSnapshot};
#[path = "runtime_projection.rs"]
mod projection;
pub(crate) async fn start(
    app: &App,
    value: &Value,
) -> Vec<(String, Value, VsourcesConfiguration, [u8; 32], u64)> {
    let lease = app.request_lease();
    let auth::Principal::Account {
        profile_id: Some(profile),
        ..
    } = lease.principal
    else {
        return vec![];
    };
    let Ok((configuration, connections)) = configuration::read(app, &lease, profile) else {
        return vec![];
    };
    let Ok(allowed) = viptv_engine::registry::Registry::builtin().connections_for(
        &configuration,
        &connections,
        Capability::Sources,
    ) else {
        return vec![];
    };
    let Some(connection) = allowed.iter().find(|c| c.plugin_id == "vsources") else {
        return vec![];
    };
    let source = connection.id.clone();
    let Some(id) = source
        .strip_prefix("vsources:")
        .and_then(|id| id.parse::<i64>().ok())
    else {
        return vec![];
    };
    let Some(vault) = &app.secret_vault else {
        return vec![];
    };
    let db = app.db.lock().unwrap();
    if lease.validate(&db).is_err() {
        return vec![];
    }
    let Ok(envelope)=db.query_row("SELECT secret FROM engine_source_integrations WHERE id=?1 AND account_id=?2 AND profile_id=?3 AND enabled=1",params![id,lease.principal.account_id(),profile],|r|r.get::<_,String>(0))else{return vec![]};
    let Ok(secret) = vault.open(
        lease.principal.account_id().unwrap(),
        "vsources",
        &format!("{profile}:{id}"),
        &envelope,
    ) else {
        return vec![];
    };
    let Ok(config) = serde_json::from_slice::<VsourcesConfiguration>(secret.expose()) else {
        return vec![];
    };
    let Ok(Some(proof)) = crate::sources::source_configuration(&db, &source) else {
        return vec![];
    };
    vec![(
        source,
        value.clone(),
        config,
        proof,
        configuration.scope.revision,
    )]
}
pub(crate) async fn canonical_card(
    app: &App,
    value: &Value,
    lease: &ResourceLease,
    revision: u64,
) -> Option<Card> {
    if value.get("metadata_origin").is_some() {
        let expected:Card=serde_json::from_value(value["card"].clone()).ok()?;
        return crate::engine_v3::origins::verified_card(app,&expected).await.ok();
    }
    // IDs supplied by a client are lookup hints only; fetch current selected-provider metadata.
    let Some(kind) = value["type"]
        .as_str()
        .filter(|k| matches!(*k, "movie" | "series"))
    else {
        return None;
    };
    let Some(request_id) = value["id"].as_str() else {
        return None;
    };
    let hint: Option<Card> = serde_json::from_value(value["card"].clone()).ok();
    let title_id = if kind == "series" {
        hint.as_ref()
            .and_then(|c| c.parent_identity.as_ref())
            .map(|p| p.id.as_str())
            .or_else(|| value["series_id"].as_str())
            .unwrap_or(request_id)
    } else {
        request_id
    };
    let explicit = crate::engine_v3::search::source_origin(app, value).ok()?;
    let origin = if let Some((_, _, origin)) = &explicit {
        origin.clone()
    } else {
        {
            let auth::Principal::Account {
                profile_id: Some(profile),
                ..
            } = lease.principal
            else {
                return None;
            };
            configuration::read(app, lease, profile)
                .ok()?
                .0
                .browsing_plugin
        }
    };
    crate::kids::consumers::require(
        app,
        kind,
        request_id,
        if origin == "simkl" {
            "simkl"
        } else {
            "stremio"
        },
        explicit.is_some(),
    )
    .ok()?;
    let addon_revision = app.addons.revision().ok()?;
    let cached = app
        .verified_cards
        .lock()
        .ok()?
        .get(&(
            lease.principal.key(),
            revision,
            origin,
            kind.to_string(),
            request_id.to_string(),
            addon_revision,
        ))
        .cloned();
    if let Some(card) = cached {
        return Some(card);
    }
    let metadata = async {
        for offset in (0..2400).step_by(128) {
            let page = crate::engine_v3::catalog::DetailPage {
                offset,
                limit: Some(128),
                metadata_origin: None,
                card: None,
                provider_id: explicit.as_ref().map(|(provider, _, _)| provider.clone()),
                addon_id: explicit.as_ref().and_then(|(_, addon, _)| *addon),
            };
            let axum::Json(detail) = crate::engine_v3::catalog::detail(
                State(app.clone()),
                Extension(lease.clone()),
                Path((kind.into(), title_id.into())),
                Query(page),
            )
            .await
            .ok()?;
            let detail: DetailSnapshot = serde_json::from_value(detail).ok()?;
            if kind == "movie" && detail.item.identity.id == request_id {
                return Some(detail.item);
            }
            if let Some(card) = detail
                .episodes
                .into_iter()
                .find(|card| card.identity.id == request_id)
            {
                return Some(card);
            }
            if detail.next_cursor.is_none() {
                return None;
            }
        }
        None
    };
    let Ok(Some(card)) = tokio::time::timeout(Duration::from_secs(10), metadata).await else {
        return None;
    };
    if viptv_engine::vsources_runtime::media_ref(&card.identity, card.parent_identity.as_ref(), &[])
        .is_err()
    {
        return None;
    }

    Some(card)
}
pub(crate) fn spawn(
    app: App,
    job: Arc<Job>,
    source: String,
    value: Value,
    config: VsourcesConfiguration,
    proof: [u8; 32],
    revision: u64,
) {
    tokio::spawn(async move {
        let id = source
            .strip_prefix("vsources:")
            .and_then(|id| id.parse::<i64>().ok())
            .unwrap_or(0);
        let auth_app = app.clone();
        let auth_source = source.clone();
        let lease = app.request_lease();
        let auth::Principal::Account {
            profile_id: Some(profile),
            ..
        } = lease.principal
        else {
            return;
        };
        let auth_job = job.clone();
        let metadata_revision = app.addons.revision().ok();
        let authorize: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            if auth_job
                .cancelled
                .load(std::sync::atomic::Ordering::Acquire)
                || configuration::validate_revision(&auth_app, profile, revision).is_err()
                || metadata_revision.as_ref().is_none_or(|expected| {
                    auth_app.addons.revision().as_ref().ok() != Some(expected)
                })
            {
                return false;
            }
            let db = auth_app.db.lock().unwrap();
            lease.validate(&db).is_ok()
                && available_db(&db, &lease, id)
                && crate::sources::source_configuration(&db, &auth_source)
                    .is_ok_and(|current| current == Some(proof))
        });
        let Some(card) = canonical_card(&app, &value, &app.request_lease(), revision).await else {
            crate::handlers_media::emit(&app, &job, &source, Err("source_unresolved".into()));
            return;
        };
        let media = viptv_engine::vsources_runtime::media_ref(
            &card.identity,
            card.parent_identity.as_ref(),
            &[],
        );
        let result = if let Ok(media) = media {
            viptv_engine::vsources_runtime::resolve_progressive(config,media,authorize.clone(),|streams|{
            if !authorize(){return;}let ttls:std::collections::HashMap<_,_>=streams.iter().map(|s|((s.url.to_string(),s.meta.audio_selection.as_ref().map(|audio|audio.audio_index)),s.ttl.min(Duration::from_secs(1800)))).collect();let limit=job.reserve_sources(streams.len());let raw=streams.iter().take(limit).map(projection::sdk_source).collect::<Vec<_>>();
            // Registration carries the exact credential/assignment proof used by this producer.
            let (cards,error)=app.register_with_configuration(&source,raw,&job.kind,Some(proof));
            if !authorize(){return;}let mut entries=app.streams.lock().unwrap();for card in &cards{if let Some(entry)=card["id"].as_str().and_then(|id|entries.get_mut(id)){entry.profile_revision=job.configuration_revision;entry.metadata_proof=job.metadata_proof.clone();entry.exact_vod=job.exact_vod.clone();entry.media_id=job.media_id.clone();if let Some(ttl)=ttls.get(&(entry.url.clone(),entry.required_audio_index)){entry.created=Instant::now().checked_sub(Duration::from_secs(1800).saturating_sub(*ttl)).unwrap_or_else(Instant::now);}}}drop(entries);
            let mut state=job.state.lock().unwrap();let seq=state.events.len()+1;state.events.push(json!({"seq":seq,"source":source,"streams":cards,"error_code":error.map(|_|"source_format_unsupported")}));drop(state);job.notify.notify_waiters();
        }).await
        } else {
            Err(viptv_engine::Error::Unresolved)
        };
        crate::handlers_media::emit(
            &app,
            &job,
            &source,
            result
                .map(|_| vec![])
                .map_err(|_| "source_unavailable".into()),
        );
    });
}
