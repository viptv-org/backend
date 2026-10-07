use serde_json::json;
use viptv_core::Identity;
use viptv_core::policy::shell_lifecycle::*;

fn identity() -> Identity {
    serde_json::from_value(json!({
        "account": {"id":"a", "username":"fixture", "name":"Fixture", "role":"member"},
        "profiles":[{"id":"p", "name":"P", "kid":false, "setupComplete":true}],
        "profileId":"p", "restricted":false, "profileSetupRequired":false
    }))
    .unwrap()
}

#[test]
fn authority_checks_account_role_scope_and_true_only_profile_flags() {
    let mut v = ForegroundAuthorityInput {
        expected: identity(),
        current: identity(),
        profile_id: Some("p".into()),
    };
    assert_eq!(foreground_authority(&v), ForegroundAuthorityDecision::Valid);
    v.current.account.id = "replacement".into();
    assert_eq!(
        foreground_authority(&v),
        ForegroundAuthorityDecision::Revoked
    );
    v.current = identity();
    v.current.account.role = "owner".into();
    assert_eq!(
        foreground_authority(&v),
        ForegroundAuthorityDecision::ProfileUnavailable
    );
    v.current = identity();
    v.current.restricted = true;
    assert_eq!(
        foreground_authority(&v),
        ForegroundAuthorityDecision::ProfileUnavailable
    );
    v.current = identity();
    v.current.profile_setup_required = true;
    assert_eq!(
        foreground_authority(&v),
        ForegroundAuthorityDecision::ProfileUnavailable
    );
    v.current = identity();
    v.current.profile_id = Some("another".into());
    assert_eq!(
        foreground_authority(&v),
        ForegroundAuthorityDecision::ProfileUnavailable
    );
    v.current = identity();
    v.current.profiles.clear();
    assert_eq!(
        foreground_authority(&v),
        ForegroundAuthorityDecision::ProfileUnavailable
    );
    v.current = identity();
    v.current.profiles[0].kid = None;
    assert_eq!(foreground_authority(&v), ForegroundAuthorityDecision::Valid);
    v.current.profiles[0].kid = Some(true);
    assert_eq!(
        foreground_authority(&v),
        ForegroundAuthorityDecision::ProfileUnavailable
    );
    v.current = identity();
    v.expected.profiles[0].setup_complete = Some(false);
    v.current.profiles[0].setup_complete = None;
    assert_eq!(foreground_authority(&v), ForegroundAuthorityDecision::Valid);
    v.current.profiles[0].setup_complete = Some(true);
    assert_eq!(
        foreground_authority(&v),
        ForegroundAuthorityDecision::ProfileUnavailable
    );
    v.profile_id = None;
    assert_eq!(foreground_authority(&v), ForegroundAuthorityDecision::Valid);
}

#[test]
fn home_compares_post_load_acknowledgement_and_retries_failed_refresh() {
    let mut v = HomeRevisionInput {
        scope_valid: true,
        observed_revision: Some("r2".into()),
        rendered_revision: Some("r1".into()),
        refresh_succeeded: None,
    };
    assert_eq!(home_revision(&v), HomeRevisionDecision::Refresh);
    v.refresh_succeeded = Some(false);
    assert_eq!(home_revision(&v), HomeRevisionDecision::RetryLater);
    // Failure does not acknowledge the observed revision; the next check still refreshes.
    v.refresh_succeeded = None;
    assert_eq!(home_revision(&v), HomeRevisionDecision::Refresh);
    v.rendered_revision = Some("r2".into());
    v.refresh_succeeded = Some(true);
    assert_eq!(home_revision(&v), HomeRevisionDecision::Refreshed);
    v.refresh_succeeded = None;
    assert_eq!(home_revision(&v), HomeRevisionDecision::Unchanged);
    // A revision committed while refresh was loading requires another refresh.
    v.observed_revision = Some("r3".into());
    assert_eq!(home_revision(&v), HomeRevisionDecision::Refresh);
    v.scope_valid = false;
    assert_eq!(home_revision(&v), HomeRevisionDecision::ScopeLost);
    v.refresh_succeeded = Some(true);
    assert_eq!(home_revision(&v), HomeRevisionDecision::ScopeLost);
    v.refresh_succeeded = None;
    v.scope_valid = true;
    v.observed_revision = None;
    assert_eq!(home_revision(&v), HomeRevisionDecision::Unsupported);
}

fn preview_input(action: PreviewAction) -> PreviewInput {
    PreviewInput {
        action,
        requested_key: "p\0movie\0m".into(),
        active_key: None,
        running: false,
        has_sources: false,
        done: false,
        failed: false,
        owner_matches: true,
    }
}

#[test]
fn preview_settles_once_adopts_running_and_partial_rows_and_retries_empty_results() {
    let mut v = preview_input(PreviewAction::Start);
    assert_eq!(preview(&v), PreviewDecision::BeginSettled);
    v.active_key = Some(v.requested_key.clone());
    assert_eq!(preview(&v), PreviewDecision::Retain);
    v.action = PreviewAction::Adopt;
    assert_eq!(preview(&v), PreviewDecision::BeginImmediate);
    v.running = true;
    assert_eq!(preview(&v), PreviewDecision::Retain);
    v.running = false;
    v.has_sources = true;
    v.done = true;
    v.failed = true;
    assert_eq!(preview(&v), PreviewDecision::Retain);
    v.action = PreviewAction::Result;
    assert_eq!(preview(&v), PreviewDecision::Ready);
    v.has_sources = false;
    assert_eq!(preview(&v), PreviewDecision::Failed);
    v.action = PreviewAction::Adopt;
    assert_eq!(preview(&v), PreviewDecision::BeginImmediate);
    v.action = PreviewAction::Result;
    v.failed = false;
    assert_eq!(preview(&v), PreviewDecision::Ready);
    v.done = false;
    assert_eq!(preview(&v), PreviewDecision::Cancelled);
}

#[test]
fn stale_preview_entry_rejected_even_when_target_key_is_identical() {
    let mut v = preview_input(PreviewAction::Update);
    v.active_key = Some(v.requested_key.clone());
    assert_eq!(preview(&v), PreviewDecision::Accept);
    v.owner_matches = false;
    assert_eq!(preview(&v), PreviewDecision::Reject);
    v.action = PreviewAction::Adopt;
    v.running = true;
    v.active_key = Some("other".into());
    assert_eq!(preview(&v), PreviewDecision::BeginImmediate);
}

#[test]
fn preview_scope_preserves_native_outgoing_details_frame_but_not_disposed_title() {
    let mut v = PreviewScopeInput {
        profile_id: Some("p".into()),
        media_type: "movie".into(),
        media_id: "m".into(),
        has_episode: false,
        active_key: Some("p\0movie\0m".into()),
        route: PreviewRoute::Details,
        releasing: false,
    };
    assert_eq!(preview_scope(&v).key, v.active_key);
    assert!(preview_scope(&v).keep);
    v.media_id = "replacement".into();
    assert!(preview_scope(&v).keep);
    v.releasing = true;
    assert!(!preview_scope(&v).keep);
    v.route = PreviewRoute::Sources;
    assert!(!preview_scope(&v).keep);
    v.media_id = "m".into();
    assert!(preview_scope(&v).keep);
    v.route = PreviewRoute::Player;
    assert!(preview_scope(&v).keep);
    v.profile_id = Some("other".into());
    assert!(!preview_scope(&v).keep);
    v.route = PreviewRoute::Other;
    assert!(!preview_scope(&v).keep);
    v.media_type = "live".into();
    assert_eq!(preview_scope(&v).key, None);
    v.media_type = "series".into();
    assert_eq!(preview_scope(&v).key, None);
    v.has_episode = true;
    assert!(preview_scope(&v).key.is_some());
    v.profile_id = None;
    assert_eq!(preview_scope(&v).key, None);
}

fn countdown_input() -> CountdownInput {
    CountdownInput {
        action: CountdownAction::Begin,
        remaining_millis: 0,
        active: false,
        elapsed_millis: 0,
        progressing: true,
        scope_matches: true,
    }
}

#[test]
fn countdown_freezes_nonprogressing_time_and_rejects_replaced_scope() {
    let mut v = countdown_input();
    let initial = countdown(&v);
    assert_eq!(initial.remaining_millis, 10_000);
    assert_eq!(initial.seconds, 10);
    v.action = CountdownAction::Advance;
    v.active = true;
    v.remaining_millis = initial.remaining_millis;
    v.elapsed_millis = 1;
    let advanced = countdown(&v);
    assert_eq!(advanced.remaining_millis, 9_999);
    assert_eq!(advanced.seconds, 10);
    v.remaining_millis = advanced.remaining_millis;
    v.progressing = false;
    v.elapsed_millis = 30_000;
    assert_eq!(countdown(&v).remaining_millis, 9_999);
    v.progressing = true;
    v.elapsed_millis = -500;
    assert_eq!(countdown(&v).remaining_millis, 9_999);
    v.elapsed_millis = 30_000;
    v.scope_matches = false;
    assert_eq!(countdown(&v).remaining_millis, 9_999);
    assert!(!countdown(&v).done);
    v.scope_matches = true;
    assert!(countdown(&v).done);
    assert_eq!(countdown(&v).seconds, 0);
    v.action = CountdownAction::Cancel;
    assert!(!countdown(&v).active);
    v.active = false;
    v.action = CountdownAction::Advance;
    assert_eq!(countdown(&v).remaining_millis, 9_999);
    assert!(!countdown(&v).done);
    v.action = CountdownAction::Begin;
    assert_eq!(countdown(&v).remaining_millis, 10_000);
}

#[test]
fn up_next_cancel_suppression_survives_ticks_and_resume_waits_for_completion() {
    let mut v = UpNextGateInput {
        media_key: "series.e1".into(),
        attempted_key: None,
        resume_awaiting_key: Some("series.e1".into()),
        ended: false,
        eligible: true,
        continuation_busy: false,
        blocked: false,
    };
    assert!(!up_next_gate(&v).start);
    v.ended = true;
    v.blocked = true;
    assert_eq!(up_next_gate(&v).resume_awaiting_key, v.resume_awaiting_key);
    v.blocked = false;
    let begin = up_next_gate(&v);
    assert!(begin.start);
    assert_eq!(begin.resume_awaiting_key, None);
    // Cancel clears only the native job/prompt, not the attempted key.
    v.attempted_key = begin.attempted_key;
    v.resume_awaiting_key = begin.resume_awaiting_key;
    assert!(!up_next_gate(&v).start);
    v.media_key = "series.e2".into();
    assert!(up_next_gate(&v).start);
    v.continuation_busy = true;
    assert!(!up_next_gate(&v).start);
    v.continuation_busy = false;
    v.eligible = false;
    assert!(!up_next_gate(&v).start);
}

#[test]
fn normalizer_uses_typed_operation_outputs_and_rejects_invalid_requests() {
    let output = shell_lifecycle(&json!({"operation":"countdown", "action":"Begin", "remainingMillis":0, "active":false, "elapsedMillis":0, "progressing":false, "scopeMatches":true})).unwrap();
    assert_eq!(
        output,
        json!({"remainingMillis":10_000,"seconds":10,"active":true,"done":false})
    );
    assert!(shell_lifecycle(&json!({"operation":"unknown"})).is_err());
    assert!(shell_lifecycle(&json!({"operation":"preview"})).is_err());
    let home = shell_lifecycle(&json!({"operation":"homeRevision", "scopeValid":true, "observedRevision":"r2", "renderedRevision":"r1", "refreshSucceeded":null})).unwrap();
    assert_eq!(home, json!("Refresh"));
}

#[test]
fn playback_replacement_resets_attempt_but_exact_resume_arms_only_near_end() {
    let mut v = UpNextPlaybackInput {
        media_key: "series.e2".into(),
        previous_key: Some("series.e1".into()),
        attempted_key: Some("series.e1".into()),
        resume_awaiting_key: None,
        explicit_resume: true,
        position_millis: 49_999,
        duration_millis: Some(60_000),
    };
    assert_eq!(up_next_playback(&v).attempted_key, None);
    assert_eq!(up_next_playback(&v).resume_awaiting_key, None);
    v.position_millis = 50_000;
    assert_eq!(
        up_next_playback(&v).resume_awaiting_key,
        Some("series.e2".into())
    );
    v.explicit_resume = false;
    assert_eq!(up_next_playback(&v).resume_awaiting_key, None);
    v.explicit_resume = true;
    v.duration_millis = None;
    assert_eq!(up_next_playback(&v).resume_awaiting_key, None);
    v.previous_key = Some("series.e2".into());
    v.attempted_key = Some("series.e2".into());
    assert_eq!(up_next_playback(&v).attempted_key, Some("series.e2".into()));
    v.resume_awaiting_key = Some("series.e1".into());
    assert_eq!(
        up_next_playback(&v).resume_awaiting_key,
        Some("series.e1".into())
    );
}

#[test]
fn public_normalize_dispatches_all_lifecycle_operations() {
    let foreground = serde_json::to_value(ForegroundAuthorityInput {
        expected: identity(),
        current: identity(),
        profile_id: Some("p".into()),
    })
    .unwrap();
    let vectors = [
        ("foregroundAuthority", foreground, json!("Valid")),
        (
            "homeRevision",
            json!({"scopeValid":true,"observedRevision":"r2","renderedRevision":"r1","refreshSucceeded":null}),
            json!("Refresh"),
        ),
        (
            "previewScope",
            json!({"profileId":"p","mediaType":"movie","mediaId":"m","hasEpisode":false,"activeKey":null,"route":"Other","releasing":false}),
            json!({"key":"p\0movie\0m","keep":false}),
        ),
        (
            "preview",
            serde_json::to_value(preview_input(PreviewAction::Start)).unwrap(),
            json!("BeginSettled"),
        ),
        (
            "upNextPlayback",
            json!({"mediaKey":"series.e2","previousKey":"series.e1","attemptedKey":"series.e1","resumeAwaitingKey":null,"explicitResume":true,"positionMillis":50000,"durationMillis":60000}),
            json!({"attemptedKey":null,"resumeAwaitingKey":"series.e2"}),
        ),
        (
            "upNextGate",
            json!({"mediaKey":"series.e1","attemptedKey":null,"resumeAwaitingKey":null,"ended":false,"eligible":true,"continuationBusy":false,"blocked":false}),
            json!({"attemptedKey":"series.e1","resumeAwaitingKey":null,"start":true}),
        ),
        (
            "countdown",
            serde_json::to_value(countdown_input()).unwrap(),
            json!({"remainingMillis":10000,"seconds":10,"active":true,"done":false}),
        ),
    ];
    for (operation, mut input, expected) in vectors {
        input["operation"] = json!(operation);
        let output =
            viptv_core::normalize("shellLifecycle".into(), input.to_string(), "".into()).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&output).unwrap(),
            expected,
            "{operation}"
        );
    }
}
