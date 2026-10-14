use super::*;
pub(crate) async fn list(
    State(app): State<App>,
    Extension(lease): Extension<ResourceLease>,
    Path(profile): Path<i64>,
    Query(page): Query<Page>,
) -> ApiResult {
    use viptv_engine::{capabilities::Availability, library::*};
    page.validate()
        .map_err(|_| ApiError::from("Invalid library query"))?;
    let app = app.with_lease(lease.clone());
    let (configuration, connections) = super::configuration::read(&app, &lease, profile)?;
    let linked = connections
        .iter()
        .any(|c| c.plugin_id == "simkl" && c.revision > 0);
    let assigned = configuration.assignments.iter().any(|a| {
        a.connection_id == format!("simkl:{profile}")
            && a.enabled
                .contains(&viptv_engine::capabilities::Capability::Authorization)
    });
    let (simkl_availability, simkl_reason) =
        if !app.simkl.configured() || app.secret_vault.is_none() {
            (
                Availability::NeedsConfiguration,
                Some("Simkl is not configured on this server".to_owned()),
            )
        } else if !assigned {
            (
                Availability::Unsupported,
                Some("Simkl authorization is disabled for this profile".to_owned()),
            )
        } else if !linked {
            (
                Availability::NeedsAuthorization,
                Some("Link Simkl for this profile".to_owned()),
            )
        } else {
            (Availability::Available, None)
        };
    // The old selector remains a request alias; all imported and saved titles share one collection.
    let legacy = page.source == "simkl";
    let unavailable = legacy && simkl_availability != Availability::Available;
    let sources = vec![LibrarySource { id:"favorites".into(), label:"Library".into(), availability:Availability::Available, reason:None }];
    let import = if simkl_availability==Availability::Available {crate::simkl::imports::schedule(&app,&lease,profile)}else{None};
    let (mut known_genres,mut genre_total,mut genre_running)=(0,0,false);
    if !unavailable && simkl_availability==Availability::Available {
        let cached = if legacy {Some(crate::simkl::watchlists::load(&app,profile,&lease).await?)}else{crate::simkl::watchlists::cached(&app,profile,&lease).await?};
        if let Some((remote,known,total,running))=cached {
            known_genres=known;genre_total=total;genre_running=running;
            crate::simkl::imports::publish_membership(&app,&lease,profile,&remote)?;
        }
    }
    let mut entries=if unavailable{vec![]}else{super::library_stats::entries(&app,&lease,profile,configuration.scope.revision)?};
    for entry in &mut entries {
        super::source_integrations::merge_cached(
            &app,
            &lease,
            configuration.scope.revision,
            std::slice::from_mut(&mut entry.card),
        );
    }
    let revision = configuration
        .scope
        .revision
        .saturating_add(super::history::revision(
            &*app
                .db
                .lock()
                .map_err(|_| ApiError::from("Database unavailable"))?,
            profile,
        )?);
    let now = util::now_millis().max(0) as u64;
    let lower = added_lower_bound(&page, now);
    let filtering_genre = page.genre.is_some();
    let mut page=page;
    if linked && assigned {page.source="simkl".into();}
    let mut snapshot = project(
        configuration.scope.clone(),
        revision,
        page,
        entries,
        sources,
        now,
        lower,
    )
    .map_err(|_| ApiError::from("Invalid or unavailable library filter"))?;
    snapshot.query.source="favorites".into();
    if import.as_ref().is_some_and(|s|s.running) {snapshot.refresh_after_millis=Some(4000);}
    if unavailable {
        snapshot.availability = simkl_availability;
        snapshot.reason = simkl_reason;
    }
    if !unavailable && known_genres < genre_total {
        snapshot.refresh_after_millis = (genre_running || import.as_ref().is_some_and(|s|s.running)).then_some(4000);
        snapshot.reason = Some(format!(
            "Genre metadata is {} ({known_genres} of {genre_total} titles verified); genre matches are partial",
            if genre_running {
                "loading"
            } else {
                "incomplete"
            }
        ));
        if filtering_genre {
            snapshot.availability = Availability::TemporarilyUnavailable;
        }
    }
    super::configuration::validate_revision(&app, profile, configuration.scope.revision)?;
    lease.validate(
        &*app
            .db
            .lock()
            .map_err(|_| ApiError::from("Database unavailable"))?,
    )?;
    Ok(axum::Json(
        serde_json::to_value(snapshot).map_err(|_| ApiError::from("Library unavailable"))?,
    ))
}
