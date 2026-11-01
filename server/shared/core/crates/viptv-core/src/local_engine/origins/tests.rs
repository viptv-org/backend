use super::*;
use serde_json::{Value, json};
use viptv_engine::presentation::{Card, DetailSnapshot};
#[derive(Default)]
struct Secrets(Mutex<std::collections::HashMap<String, String>>);
impl SecretStore for Secrets {
    fn get(&self, key: String) -> Result<Option<String>, CoreError> {
        Ok(self.0.lock().unwrap().get(&key).cloned())
    }
    fn put(&self, key: String, value: String) -> Result<(), CoreError> {
        self.0.lock().unwrap().insert(key, value);
        Ok(())
    }
    fn remove(&self, key: String) -> Result<(), CoreError> {
        self.0.lock().unwrap().remove(&key);
        Ok(())
    }
}
struct Fixture {
    engine: LocalEngine,
    dir: std::path::PathBuf,
    providers: Vec<String>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn fixture() -> Fixture {
    let dir = std::env::temp_dir().join(format!("viptv-native-origin-{}", uuid::Uuid::new_v4()));
    let engine = LocalEngine::new(
        dir.join("local.sqlite").to_string_lossy().into(),
        Arc::new(Secrets::default()),
    )
    .unwrap();
    let mut providers = vec![];
    for name in ["first", "second"] {
        let installed:Value=serde_json::from_str(&engine.install_addon("default".into(),format!("https://{name}.example/manifest.json"),json!({"id":name,"name":name,"version":"1","resources":["catalog","meta","stream","subtitles"],"types":["movie","series"],"catalogs":[{"id":"all","type":"movie","extra":[{"name":"search"}]}]}).to_string()).unwrap()).unwrap();
        providers.push(installed["id"].as_str().unwrap().into());
    }
    Fixture {
        engine,
        dir,
        providers,
    }
}
fn scope(engine: &LocalEngine) -> Scope {
    engine
        .store
        .lock()
        .unwrap()
        .profile(&local_scope("default"))
        .unwrap()
        .unwrap()
        .scope
}
fn hint(engine: &LocalEngine, provider: &str, kind: &str, id: &str) -> Card {
    let current = scope(engine);
    let origin = engine
        .store
        .lock()
        .unwrap()
        .metadata_origin(&current, provider)
        .unwrap();
    serde_json::from_value(json!({"identity":{"id":id,"kind":kind,"ids":[]},"metadata_origin":origin,"provider_id":provider,"title":"Expected title","badges":[]})).unwrap()
}
fn detail(engine: &LocalEngine, expected: &Card, imdb: Option<&str>) -> Card {
    let id = &expected.identity.id;
    let plan: Value = serde_json::from_str(
        &engine
            .plan_detail_for_origin(
                "default".into(),
                expected.identity.kind.clone(),
                id.clone(),
                serde_json::to_string(expected).unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    let body=json!({"meta":{"id":id,"name":"Actual title","type":expected.identity.kind,"imdb_id":imdb}}).to_string();
    serde_json::from_str::<DetailSnapshot>(
        &engine
            .project_detail(plan["operation"].as_str().unwrap().into(), body, 0)
            .unwrap(),
    )
    .unwrap()
    .item
}
fn source(engine: &LocalEngine, card: &Card, url: &str) -> String {
    let plan: Value = serde_json::from_str(
        &engine
            .plan_sources_for_origin(
                "default".into(),
                card.identity.kind.clone(),
                card.identity.id.clone(),
                serde_json::to_string(card).unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        plan["requests"].as_array().unwrap().len(),
        1,
        "Opaque lookup must stay with its issuer"
    );
    assert_eq!(plan["requests"][0]["provider"], card.provider_id);
    let candidates:Value=serde_json::from_str(&engine.project_sources_partial(plan["operation"].as_str().unwrap().into(),json!([{"provider":card.provider_id,"body":json!({"streams":[{"url":url}]}).to_string()}]).to_string(),true).unwrap()).unwrap();
    candidates[0]["id"].as_str().unwrap().into()
}
#[test]
fn identical_opaque_ids_keep_library_history_streams_and_subtitles_with_their_issuer() {
    let f = fixture();
    let a = detail(
        &f.engine,
        &hint(&f.engine, &f.providers[0], "movie", "same-id"),
        None,
    );
    let b = detail(
        &f.engine,
        &hint(&f.engine, &f.providers[1], "movie", "same-id"),
        None,
    );
    for (card, position) in [(&a, 1000), (&b, 2000)] {
        f.engine
            .save_favorite(
                "default".into(),
                scope(&f.engine).revision,
                serde_json::to_string(card).unwrap(),
            )
            .unwrap();
        f.engine
            .record_progress(
                "default".into(),
                scope(&f.engine).revision,
                serde_json::to_string(card).unwrap(),
                position,
                100000,
            )
            .unwrap();
    }
    let membership: Value =
        serde_json::from_str(&f.engine.library_membership("default".into()).unwrap()).unwrap();
    assert_eq!(membership["scoped_identities"].as_array().unwrap().len(), 2);
    assert!(membership["identities"].as_array().unwrap().is_empty());
    let history: Value =
        serde_json::from_str(&f.engine.history("default".into()).unwrap()).unwrap();
    assert_eq!(history.as_array().unwrap().len(), 2);
    let grant = source(&f.engine, &a, "https://media.example/first.mp4");
    let open: Value = serde_json::from_str(&f.engine.open_source(grant.clone()).unwrap()).unwrap();
    assert_eq!(open["position_millis"], 1000);
    assert_eq!(
        open["metadata_origin"],
        serde_json::to_value(&a.metadata_origin).unwrap()
    );
    let subtitles: Value = serde_json::from_str(
        &f.engine
            .plan_subtitles_for_origin(
                "default".into(),
                "movie".into(),
                "same-id".into(),
                "{}".into(),
                serde_json::to_string(&a).unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(subtitles["requests"].as_array().unwrap().len(), 1);
    assert_eq!(subtitles["requests"][0]["provider"], f.providers[0]);
    let projected:Value=serde_json::from_str(&f.engine.project_subtitles(subtitles["operation"].as_str().unwrap().into(),json!([{"provider":f.providers[0],"body":json!({"subtitles":[{"id":"one","lang":"en","url":"https://subtitles.example/first.vtt"}]}).to_string()}]).to_string()).unwrap()).unwrap();
    let subtitle = projected["candidates"][0]["id"].as_str().unwrap();
    let opened: Value =
        serde_json::from_str(&f.engine.open_subtitle(subtitle.into()).unwrap()).unwrap();
    assert_eq!(
        opened["metadata_origin"],
        serde_json::to_value(&a.metadata_origin).unwrap()
    );
    assert_eq!(
        opened["media_context"], open["media_context"],
        "Same title source/subtitle must carry exact host identity context"
    );
    let mut forged = a.clone();
    forged.metadata_origin = b.metadata_origin.clone();
    assert!(
        f.engine
            .plan_sources_for_origin(
                "default".into(),
                "movie".into(),
                "same-id".into(),
                serde_json::to_string(&forged).unwrap()
            )
            .is_err()
    );
    f.engine
        .set_addon_enabled("default".into(), f.providers[0].clone(), false)
        .unwrap();
    assert!(f.engine.open_source(grant).is_err());
    assert!(f.engine.open_subtitle(subtitle.into()).is_err());
    f.engine
        .remove_favorite_for_origin(
            "default".into(),
            scope(&f.engine).revision,
            serde_json::to_string(&a).unwrap(),
        )
        .unwrap();
    let membership: Value =
        serde_json::from_str(&f.engine.library_membership("default".into()).unwrap()).unwrap();
    assert_eq!(membership["scoped_identities"].as_array().unwrap().len(), 1);
}
#[test]
fn remapped_lookup_rejects_old_expectations_and_removes_only_the_selected_stored_alias() {
    let f = fixture();
    let blank = hint(&f.engine, &f.providers[0], "movie", "opaque-lookup");
    let a = detail(&f.engine, &blank, Some("tt1111111"));
    f.engine
        .save_favorite(
            "default".into(),
            scope(&f.engine).revision,
            serde_json::to_string(&a).unwrap(),
        )
        .unwrap();
    f.engine
        .record_progress(
            "default".into(),
            scope(&f.engine).revision,
            serde_json::to_string(&a).unwrap(),
            4000,
            100000,
        )
        .unwrap();
    let plan: Value = serde_json::from_str(
        &f.engine
            .plan_detail_for_origin(
                "default".into(),
                "movie".into(),
                "opaque-lookup".into(),
                serde_json::to_string(&a).unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    assert!(
        f.engine
            .project_detail(
                plan["operation"].as_str().unwrap().into(),
                json!({"meta":{"id":"opaque-lookup","name":"Remapped","imdb_id":"tt2222222"}})
                    .to_string(),
                0
            )
            .is_err()
    );
    assert!(
        f.engine
            .plan_sources_for_origin(
                "default".into(),
                "movie".into(),
                "opaque-lookup".into(),
                serde_json::to_string(&a).unwrap()
            )
            .is_err(),
        "Known stale proof must be invalidated after mismatch"
    );
    let b = detail(&f.engine, &blank, Some("tt2222222"));
    // Existing playback A stays attached to A while new discovery rejects its
    // changed opaque lookup; updating A cannot overwrite the B record.
    f.engine
        .record_progress(
            "default".into(),
            scope(&f.engine).revision,
            serde_json::to_string(&a).unwrap(),
            5000,
            100000,
        )
        .unwrap();
    let current_scope = scope(&f.engine);
    assert_eq!(
        f.engine
            .store
            .lock()
            .unwrap()
            .playback_for_origin(&current_scope, &a)
            .unwrap()
            .unwrap()
            .position_millis,
        5000
    );

    f.engine
        .save_favorite(
            "default".into(),
            scope(&f.engine).revision,
            serde_json::to_string(&b).unwrap(),
        )
        .unwrap();
    f.engine
        .record_progress(
            "default".into(),
            scope(&f.engine).revision,
            serde_json::to_string(&b).unwrap(),
            8000,
            100000,
        )
        .unwrap();
    let membership: Value =
        serde_json::from_str(&f.engine.library_membership("default".into()).unwrap()).unwrap();
    assert_eq!(membership["identities"].as_array().unwrap().len(), 2);
    let grant = source(&f.engine, &b, "https://media.example/b.mp4");
    let opened: Value = serde_json::from_str(&f.engine.open_source(grant).unwrap()).unwrap();
    assert_eq!(opened["position_millis"], 8000);
    f.engine
        .remove_favorite_for_origin(
            "default".into(),
            scope(&f.engine).revision,
            serde_json::to_string(&a).unwrap(),
        )
        .unwrap();
    let membership: Value =
        serde_json::from_str(&f.engine.library_membership("default".into()).unwrap()).unwrap();
    assert_eq!(membership["identities"].as_array().unwrap().len(), 1);
    assert_eq!(membership["identities"][0]["id"], "tt2222222");
}
#[test]
fn unverified_external_claims_cannot_mint_shared_membership_or_authorize_sources() {
    let f = fixture();
    let mut fake = hint(&f.engine, &f.providers[0], "movie", "unverified");
    fake.identity.ids.push(viptv_engine::identity::ExternalId {
        namespace: "imdb".into(),
        value: "tt9999999".into(),
    });
    f.engine
        .save_favorite(
            "default".into(),
            scope(&f.engine).revision,
            serde_json::to_string(&fake).unwrap(),
        )
        .unwrap();
    let membership: Value =
        serde_json::from_str(&f.engine.library_membership("default".into()).unwrap()).unwrap();
    assert!(membership["identities"].as_array().unwrap().is_empty());
    assert_eq!(membership["scoped_identities"].as_array().unwrap().len(), 1);
    assert!(
        f.engine
            .plan_sources_for_origin(
                "default".into(),
                "movie".into(),
                "unverified".into(),
                serde_json::to_string(&fake).unwrap()
            )
            .is_err()
    );
    assert!(
        f.engine
            .plan_sources("default".into(), "movie".into(), "unverified".into())
            .is_err()
    );
}
#[test]
fn stremio_cannot_impersonate_reserved_simkl_namespace_and_padded_episode_ids_are_not_shared() {
    let f = fixture();
    assert!(!shared_lookup("movie", "simkl:movie:42"));
    assert!(!shared_lookup("series", "tt123:01:1"));
    assert!(!shared_lookup("series", "tt123:1:01"));
    assert!(shared_lookup("series", "tt123:1:1"));
    assert!(
        f.engine
            .plan_detail(
                "default".into(),
                "movie".into(),
                "simkl:movie:42".into(),
                f.providers[0].clone()
            )
            .is_err()
    );
    assert!(
        f.engine
            .plan_sources("default".into(), "movie".into(), "simkl:movie:42".into())
            .is_err()
    );
    let plan: Value = serde_json::from_str(
        &f.engine
            .plan_search("default".into(), "fixture".into())
            .unwrap(),
    )
    .unwrap();
    let responses=plan["requests"].as_array().unwrap().iter().map(|request|json!({"id":request["id"],"body":json!({"metas":[{"id":"simkl:movie:42","type":"movie","name":"Forged"},{"id":"allowed","type":"movie","name":"Allowed"}]}).to_string()})).collect::<Vec<_>>();
    let projected: Value = serde_json::from_str(
        &f.engine
            .project_home(
                plan["id"].as_str().unwrap().into(),
                serde_json::to_string(&responses).unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    for row in projected["rows"].as_array().unwrap() {
        assert_eq!(row["cards"].as_array().unwrap().len(), 1);
        assert_eq!(row["cards"][0]["identity"]["id"], "allowed");
        assert!(
            row["cards"][0]["metadata_origin"]["incarnation"]
                .as_str()
                .is_some()
        );
    }
}

#[test]
fn late_episode_proof_uses_the_complete_cached_document_and_rejects_changed_parent_mapping() {
    let f = fixture();
    let expected = hint(&f.engine, &f.providers[0], "series", "private-series");
    let plan: Value = serde_json::from_str(
        &f.engine
            .plan_detail_for_origin(
                "default".into(),
                "series".into(),
                "private-series".into(),
                serde_json::to_string(&expected).unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    let body = |imdb: &str| {
        json!({"meta":{"id":"private-series","name":"Series","imdb_id":imdb,"videos":(1..=1500).map(|episode|json!({"id":format!("private-episode-{episode}"),"title":format!("Episode {episode}"),"season":1,"episode":episode})).collect::<Vec<_>>()}}).to_string()
    };
    let first: DetailSnapshot = serde_json::from_str(
        &f.engine
            .project_detail(
                plan["operation"].as_str().unwrap().into(),
                body("tt1111111"),
                0,
            )
            .unwrap(),
    )
    .unwrap();
    let episode:Card=serde_json::from_value(json!({"identity":{"id":"private-episode-1499","kind":"series","ids":[],"episode":{"order":"aired","season":1,"episode":1499}},"parent_identity":first.item.identity,"metadata_origin":first.item.metadata_origin,"provider_id":f.providers[0],"title":"Saved episode","badges":[]})).unwrap();
    let cached: DetailSnapshot = serde_json::from_str(
        &f.engine
            .detail_page_for_origin(
                "default".into(),
                "series".into(),
                "private-series".into(),
                serde_json::to_string(&episode).unwrap(),
                0,
                24,
            )
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(cached.episodes.len(), 24);
    let (_, proof) = f.engine.verified_source_card("default", &episode).unwrap();
    assert_eq!(proof.identity.episode.as_ref().unwrap().episode, 1499);
    f.engine
        .record_progress(
            "default".into(),
            scope(&f.engine).revision,
            serde_json::to_string(&episode).unwrap(),
            46000,
            100000,
        )
        .unwrap();
    let grant = source(&f.engine, &episode, "https://media.example/1499.mp4");
    let opened: Value = serde_json::from_str(&f.engine.open_source(grant).unwrap()).unwrap();
    assert_eq!(opened["position_millis"], 46000);
    let plan: Value = serde_json::from_str(
        &f.engine
            .plan_detail_for_origin(
                "default".into(),
                "series".into(),
                "private-series".into(),
                serde_json::to_string(&episode).unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    assert!(
        f.engine
            .project_detail(
                plan["operation"].as_str().unwrap().into(),
                body("tt2222222"),
                0
            )
            .is_err()
    );
    assert!(f.engine.verified_source_card("default", &episode).is_err());
}

#[test]
fn same_connector_id_reinstallation_keeps_old_owned_rows_but_cannot_inherit_their_authority() {
    let f = fixture();
    let id = f.providers[0].clone();
    let old = detail(&f.engine, &hint(&f.engine, &id, "movie", "same-id"), None);
    f.engine
        .save_favorite(
            "default".into(),
            scope(&f.engine).revision,
            serde_json::to_string(&old).unwrap(),
        )
        .unwrap();
    let grant = source(&f.engine, &old, "https://media.example/old.mp4");
    let connection = f
        .engine
        .store
        .lock()
        .unwrap()
        .connections("local")
        .unwrap()
        .into_iter()
        .find(|connection| connection.id == id)
        .unwrap();
    let addon = f.engine.addon(&id).unwrap();
    f.engine.remove_addon(id.clone()).unwrap();
    f.engine
        .secrets
        .put(
            format!("{}:{id}", f.engine.namespace),
            serde_json::to_string(&addon).unwrap(),
        )
        .unwrap();
    f.engine
        .store
        .lock()
        .unwrap()
        .install("default", &connection)
        .unwrap();
    let current = hint(&f.engine, &id, "movie", "same-id");
    assert_ne!(old.metadata_origin, current.metadata_origin);
    assert!(
        f.engine
            .plan_detail_for_origin(
                "default".into(),
                "movie".into(),
                "same-id".into(),
                serde_json::to_string(&old).unwrap()
            )
            .is_err()
    );
    assert!(f.engine.open_source(grant).is_err());
    let new = detail(&f.engine, &current, None);
    f.engine
        .save_favorite(
            "default".into(),
            scope(&f.engine).revision,
            serde_json::to_string(&new).unwrap(),
        )
        .unwrap();
    let membership: Value =
        serde_json::from_str(&f.engine.library_membership("default".into()).unwrap()).unwrap();
    assert_eq!(membership["scoped_identities"].as_array().unwrap().len(), 2);
    f.engine
        .remove_favorite_for_origin(
            "default".into(),
            scope(&f.engine).revision,
            serde_json::to_string(&old).unwrap(),
        )
        .unwrap();
    let membership: Value =
        serde_json::from_str(&f.engine.library_membership("default".into()).unwrap()).unwrap();
    assert_eq!(membership["scoped_identities"].as_array().unwrap().len(), 1);
    assert_eq!(
        membership["scoped_identities"][0]["metadata_origin"],
        serde_json::to_value(&new.metadata_origin).unwrap()
    );
}

#[test]
fn verified_alias_resumes_shared_public_legacy_progress_without_accepting_client_aliases() {
    let f = fixture();
    let legacy = viptv_engine::stremio::cards(
        r#"{"metas":[{"id":"tt1111111","type":"movie","name":"Shared legacy"}]}"#,
        "stremio",
    )
    .unwrap()
    .remove(0);
    f.engine
        .record_progress(
            "default".into(),
            scope(&f.engine).revision,
            serde_json::to_string(&legacy).unwrap(),
            7000,
            100000,
        )
        .unwrap();
    let card = detail(
        &f.engine,
        &hint(&f.engine, &f.providers[0], "movie", "private-alias"),
        Some("tt1111111"),
    );
    let grant = source(&f.engine, &card, "https://media.example/shared-a.mp4");
    let opened: Value = serde_json::from_str(&f.engine.open_source(grant).unwrap()).unwrap();
    assert_eq!(opened["position_millis"], 7000);
}

#[test]
fn older_popped_metadata_reply_cannot_replace_or_invalidate_newer_target_proof() {
    for old_kind in ["movie", "series"] {
        let f = fixture();
        let blank = hint(&f.engine, &f.providers[0], "movie", "reused");
        let old: Value = serde_json::from_str(
            &f.engine
                .plan_detail_for_origin(
                    "default".into(),
                    "movie".into(),
                    "reused".into(),
                    serde_json::to_string(&blank).unwrap(),
                )
                .unwrap(),
        )
        .unwrap();
        let old_id = old["operation"].as_str().unwrap().to_owned();
        // Model the first projector having taken work out of its queue before a
        // newer request is issued; final publication must still check ownership.
        let popped = f.engine.details.lock().unwrap().remove(&old_id).unwrap();
        let current = detail(&f.engine, &blank, Some("tt2222222"));
        f.engine
            .details
            .lock()
            .unwrap()
            .insert(old_id.clone(), popped);
        assert!(f.engine.project_detail(old_id,json!({"meta":{"id":"reused","type":old_kind,"name":"Old","imdb_id":"tt1111111"}}).to_string(),0).is_err());
        let (_, proof) = f.engine.verified_source_card("default", &current).unwrap();
        assert_eq!(proof.identity.ids, current.identity.ids);
        source(&f.engine, &current, "https://media.example/current.mp4");
    }
}
#[test]
fn wrong_identity_or_kind_invalidates_known_proof_but_malformed_transient_data_keeps_cache() {
    let f = fixture();
    let blank = hint(&f.engine, &f.providers[0], "movie", "target");
    for body in [
        json!({"meta":{"id":"foreign","type":"movie","name":"Wrong ID"}}).to_string(),
        json!({"meta":{"id":"target","type":"series","name":"Wrong kind"}}).to_string(),
        "{".into(),
    ] {
        let card = detail(&f.engine, &blank, Some("tt1111111"));
        let plan: Value = serde_json::from_str(
            &f.engine
                .plan_detail_for_origin(
                    "default".into(),
                    "movie".into(),
                    "target".into(),
                    serde_json::to_string(&card).unwrap(),
                )
                .unwrap(),
        )
        .unwrap();
        assert!(
            f.engine
                .project_detail(plan["operation"].as_str().unwrap().into(), body.clone(), 0)
                .is_err()
        );
        assert_eq!(
            f.engine.verified_source_card("default", &card).is_ok(),
            body == "{",
            "Only a known identity mismatch invalidates prior proof"
        );
    }
}

#[test]
fn verified_enrichment_carries_external_assertions_without_rewriting_lookup_or_episode_identity() {
    let f = fixture();
    let preview = hint(&f.engine, &f.providers[0], "movie", "opaque-preview");
    let home = viptv_engine::presentation::HomeSnapshot {
        scope: scope(&f.engine),
        revision: 0,
        hero: Some(preview.clone()),
        featured: vec![preview.clone()],
        rows: vec![],
        complete: true,
    };
    let plan: Value = serde_json::from_str(
        &f.engine
            .plan_home_enrichment("default".into(), serde_json::to_string(&home).unwrap())
            .unwrap(),
    )
    .unwrap();
    let request = &plan["requests"][0];
    let enriched:viptv_engine::presentation::HomeSnapshot=serde_json::from_str(&f.engine.project_home_enrichment(plan["operation"].as_str().unwrap().into(),json!([{"operation":request["operation"],"body":json!({"meta":{"id":"opaque-preview","type":"movie","name":"Verified","imdb_id":"tt1111111","tmdb_id":123}}).to_string()}]).to_string()).unwrap()).unwrap();
    let card = &enriched.featured[0];
    assert_eq!(card.identity.id, "opaque-preview");
    assert_eq!(card.metadata_origin, preview.metadata_origin);
    assert!(
        card.identity
            .ids
            .iter()
            .any(|id| id.namespace == "imdb" && id.value == "tt1111111")
    );
    let mut metadata = card.clone();
    metadata.identity.kind = "series".into();
    let mut episode = preview.clone();
    episode.identity.id = "opaque-child".into();
    episode.identity.kind = "series".into();
    episode.identity.episode = Some(EpisodeCoordinates {
        order: "aired".into(),
        season: Some(2),
        episode: 3,
    });
    episode.parent_identity = Some(MediaIdentity {
        id: "opaque-preview".into(),
        kind: "series".into(),
        ids: vec![],
        episode: None,
        edition: None,
    });
    episode.progress = Some(0.25);
    let enriched = enrich_verified_card(&episode, &metadata);
    assert_eq!(enriched.identity, episode.identity);
    assert_eq!(enriched.progress, Some(0.25));
    assert_eq!(
        enriched.parent_identity.as_ref().unwrap().ids,
        metadata.identity.ids
    );
    let mut stale = card.clone();
    stale.identity.ids = vec![viptv_engine::identity::ExternalId {
        namespace: "imdb".into(),
        value: "tt2222222".into(),
    }];
    let unchanged = enrich_verified_card(&stale, card);
    assert_eq!(unchanged.identity, stale.identity);
}

#[test]
fn playback_media_context_distinguishes_same_connector_titles_remaps_and_episode_editions() {
    let f = fixture();
    let blank = hint(&f.engine, &f.providers[0], "movie", "same-id");
    let a = detail(&f.engine, &blank, Some("tt1111111"));
    let grant = source(&f.engine, &a, "https://media.example/old.mp4");
    let old: Value = serde_json::from_str(&f.engine.open_source(grant.clone()).unwrap()).unwrap();
    let b = detail(&f.engine, &blank, Some("tt2222222"));
    let planned: Value = serde_json::from_str(
        &f.engine
            .plan_subtitles_for_origin(
                "default".into(),
                "movie".into(),
                "same-id".into(),
                "{}".into(),
                serde_json::to_string(&b).unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    let projected: Value = serde_json::from_str(&f.engine.project_subtitles(planned["operation"].as_str().unwrap().into(), json!([{"provider":f.providers[0],"body":json!({"subtitles":[{"id":"one","lang":"en","url":"https://subtitles.example/new.vtt"}]}).to_string()}]).to_string()).unwrap()).unwrap();
    let caption: Value = serde_json::from_str(
        &f.engine
            .open_subtitle(projected["candidates"][0]["id"].as_str().unwrap().into())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(old["metadata_origin"], caption["metadata_origin"]);
    assert_ne!(
        old["media_context"], caption["media_context"],
        "Same raw lookup remapped to another verified title must differ"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&f.engine.open_source(grant).unwrap()).unwrap()["media_context"],
        old["media_context"],
        "Old playable retains its captured identity"
    );
    let context = playback_media_context("movie", "same-id", Some(&a));
    let mut other = a.clone();
    other.identity.id = "other-id".into();
    assert_ne!(
        context,
        playback_media_context("movie", "other-id", Some(&other))
    );
    other = a.clone();
    other.identity.edition = Some("Director cut".into());
    assert_ne!(
        context,
        playback_media_context("movie", "same-id", Some(&other))
    );
    other = a.clone();
    other.parent_identity = Some(a.identity.clone());
    assert_ne!(
        context,
        playback_media_context("movie", "same-id", Some(&other))
    );
    other = a.clone();
    other.identity.ids.reverse();
    other.identity.ids.extend(a.identity.ids.clone());
    assert_eq!(
        context,
        playback_media_context("movie", "same-id", Some(&other)),
        "Assertion order and duplicates are immaterial"
    );
}

#[path = "delegation_tests.rs"]
mod delegation;
