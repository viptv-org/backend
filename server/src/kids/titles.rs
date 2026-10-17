//! Parent-only typed title selection. Opaque provider identities remain unsupported.
use super::*;

pub(crate) fn canonical(provider: &str, kind: &str, id: &str) -> bool {
    if !matches!(kind, "movie" | "series") {
        return false;
    }
    if provider == "stremio" {
        return id.strip_prefix("tt").is_some_and(|digits| {
            (1..=16).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
        });
    }
    if provider != "simkl" {
        return false;
    }
    let parts = id.split(':').collect::<Vec<_>>();
    matches!(parts.as_slice(),["simkl",category,number] if matches!(*category,"movie"|"tv"|"anime") && number.parse::<u64>().is_ok_and(|n|n>0 && n.to_string()==*number) && (*category!="movie" || kind=="movie") && (*category!="tv" || kind=="series"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Approval {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    approved: bool,
    metadata_provider_id: Option<String>,
    metadata_addon_id: Option<i64>,
    metadata_origin: Option<viptv_engine::origin::MetadataOrigin>,
    card: Option<viptv_engine::presentation::Card>,
}

pub(crate) async fn approve(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    axum::Json(request): axum::Json<Approval>,
) -> ApiResult {
    let app = app.with_lease(lease.clone());
    let parent_scope = super::parent_scope::ParentScope::new(&app, profile)?;
    if !matches!(request.kind.as_str(), "movie" | "series")
        || request.id.is_empty()
        || request.id.len() > 512
        || request.id.chars().any(char::is_control)
    {
        return Err("Invalid title approval".into());
    }
    let expected = request.card.as_ref();
    if let Some(card) = expected {
        card.validate_library()
            .map_err(|_| ApiError::from("Invalid approval expectation"))?;
    }
    if expected
        .is_some_and(|card| card.identity.kind != request.kind || card.identity.id != request.id)
    {
        return Err("Approval expectation does not match title".into());
    }
    let origin = request
        .metadata_origin
        .as_ref()
        .or_else(|| expected.and_then(|card| card.metadata_origin.as_ref()))
        .cloned();
    if request
        .metadata_origin
        .as_ref()
        .zip(expected.and_then(|card| card.metadata_origin.as_ref()))
        .is_some_and(|(a, b)| a != b)
    {
        return Err("Conflicting metadata origin".into());
    }
    let (provider, addon) = if !request.approved && origin.is_some() {
        let origin = origin.as_ref().expect("scoped revoke");
        origin
            .validate()
            .map_err(|_| ApiError::from("Invalid metadata origin"))?;
        (
            Some(origin.provider_id.clone()),
            origin
                .connection_id
                .as_deref()
                .and_then(|id| id.strip_prefix("addon:"))
                .and_then(|id| id.parse::<i64>().ok()),
        )
    } else {
        crate::engine_v3::origins::selectors(
            &app,
            request.metadata_provider_id.as_deref(),
            request.metadata_addon_id,
            origin.as_ref(),
        )?
    };
    let private = !canonical(
        provider.as_deref().unwrap_or(""),
        &request.kind,
        &request.id,
    );
    if private && request.approved && (origin.is_none() || expected.is_none()) {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "A scoped title selection is required".into(),
        ));
    }
    if origin.as_ref().is_some_and(|o| o.provider_id == "stremio")
        && request.id.starts_with("simkl:")
    {
        return Err(forbidden());
    }
    let (config, _) = crate::engine_v3::configuration::read_parent(&app, &parent_scope)?;
    let addon_revision = app.addons.revision()?;
    let mut simkl_revision = None;
    let meta = if request.approved {
        let provider = provider.as_deref().ok_or("Metadata provider is required")?;
        if !canonical(provider, &request.kind, &request.id) && origin.is_none() {
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "This provider's private title IDs do not support parent approval yet".into(),
            ));
        }
        let (scoped, _, _) = crate::engine_v3::search::metadata_scope_parent(
            app.clone(),
            &parent_scope,
            provider,
            addon,
        )?;
        let fetched = tokio::time::timeout(Duration::from_secs(15), async {
            if provider == "simkl" {
                {
                    let (meta, revision) = crate::simkl::parent_metadata(
                        &scoped,
                        &parent_scope,
                        &request.kind,
                        &request.id,
                    )
                    .await?;
                    simkl_revision = Some(revision);
                    Ok(meta)
                }
            } else {
                let value = scoped.addons.meta(&request.kind, &request.id).await?;
                Ok::<_, ApiError>(value["meta"].clone())
            }
        })
        .await
        .map_err(|_| ApiError::from("Metadata timed out"))??;
        if fetched["id"].as_str() != Some(request.id.as_str())
            || !fetched["name"].is_string()
            || fetched["type"]
                .as_str()
                .is_some_and(|kind| kind != request.kind)
        {
            return Err("Metadata identity did not match the requested title".into());
        }
        Some(fetched)
    } else {
        None
    };
    let scoped_document = if let (Some(origin), Some(meta)) = (&origin, &meta) {
        let mut detail = viptv_engine::stremio::detail_page(
            &json!({"meta":meta}).to_string(),
            origin.connection_id.as_deref().unwrap_or("simkl"),
            &request.kind,
            &request.id,
            0,
            128,
        )
        .map_err(|_| ApiError::from("Invalid approval metadata"))?;
        detail.item.metadata_origin = Some(origin.clone());
        parent_scope.recheck(&app)?;
        if app.addons.revision()? != addon_revision {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Metadata integrations changed. Try again.".into(),
            ));
        }
        crate::engine_v3::origins::check_expected_at(&app, expected, &detail.item)?;
        let mut episodes = viptv_engine::stremio::episode_cards(
            meta,
            origin.connection_id.as_deref().unwrap_or("simkl"),
            &detail.item,
        )
        .map_err(|_| ApiError::from("Invalid approval episode metadata"))?;
        for episode in &mut episodes {
            episode.metadata_origin = Some(origin.clone());
        }
        Some((detail.item, episodes))
    } else {
        None
    };
    parent_scope.recheck(&app)?;
    // A provider result may never be reauthorized under a newer connector revision.
    if app.addons.revision()? != addon_revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Metadata integrations changed. Try again.".into(),
        ));
    }
    let mut db = app
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    let tx = db.transaction().map_err(db_error)?;
    parent_scope.validate(&tx)?;
    let current: u64 = tx
        .query_row(
            "SELECT revision FROM engine_profile_configuration WHERE profile_id=?1",
            [profile],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or(0);
    if current != config.scope.revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Profile configuration changed".into(),
        ));
    }
    if app.addons.revision_db(&tx)? != addon_revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Metadata integrations changed. Try again.".into(),
        ));
    }
    if let Some(expected) = simkl_revision {
        let current:Option<u64>=tx.query_row("SELECT revision FROM simkl_links WHERE profile_id=?1 AND account_id=?2 AND credential IS NOT NULL",params![profile,lease.principal.account_id()],|r|r.get(0)).optional().map_err(db_error)?;
        if current != expected {
            return Err(ApiError(StatusCode::CONFLICT, "Simkl link changed".into()));
        }
    }
    let value = if let Some(origin) = &origin {
        if request.approved {
            let (card, episodes) = scoped_document.as_ref().ok_or("Metadata unavailable")?;
            let meta = meta.as_ref().ok_or("Metadata unavailable")?;
            super::scoped::observe_document(
                &tx,
                lease.principal.account_id().ok_or_else(auth::forbidden)?,
                card,
                episodes,
                meta,
            )?;
            if canonical(
                provider.as_deref().unwrap_or(""),
                &request.kind,
                &request.id,
            ) {
                schema::apply_approval(
                    &tx,
                    &app,
                    profile,
                    &request.kind,
                    &request.id,
                    true,
                    Some(meta),
                )?
            } else {
                super::scoped::approve_db(&tx, &app, profile, card, true)?
            }
        } else {
            let card = expected
                .filter(|card| card.metadata_origin.as_ref() == Some(origin))
                .ok_or("A stored scoped title selection is required")?;
            if canonical(
                provider.as_deref().unwrap_or(""),
                &request.kind,
                &request.id,
            ) {
                schema::apply_approval(&tx, &app, profile, &request.kind, &request.id, false, None)?
            } else {
                super::scoped::approve_db(&tx, &app, profile, card, false)?
            }
        }
    } else {
        schema::apply_approval(
            &tx,
            &app,
            profile,
            &request.kind,
            &request.id,
            request.approved,
            meta.as_ref(),
        )?
    };
    tx.commit().map_err(db_error)?;
    Ok(axum::Json(value))
}

pub(crate) async fn list(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    Query(page): Query<schema::ApprovalPage>,
) -> ApiResult {
    let scoped = app.clone().with_lease(lease.clone());
    let parent_scope = super::parent_scope::ParentScope::new(&scoped, profile)?;
    let axum::Json(items) =
        schema::approvals(State(app), Extension(lease), Path(profile), Query(page)).await?;
    let mut items = items
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| {
            let mut item = item.clone();
            let kind = item["type"].as_str().unwrap_or("");
            let id = item["id"].as_str().unwrap_or("");
            let provider = if canonical("simkl", kind, id) {
                Some("simkl")
            } else if canonical("stremio", kind, id) {
                Some("stremio")
            } else {
                None
            };
            item["metadata_provider_id"] = json!(provider);
            item["metadata_addon_id"] = Value::Null;
            item["approval_supported"] = json!(provider.is_some());
            item
        })
        .collect::<Vec<_>>();
    parent_scope.recheck(&scoped)?;
    let db = scoped
        .db
        .lock()
        .map_err(|_| ApiError::from("Database unavailable"))?;
    parent_scope.validate(&db)?;
    items.extend(super::scoped::list_db(&db, profile)?);
    Ok(axum::Json(json!({"items":items})))
}
