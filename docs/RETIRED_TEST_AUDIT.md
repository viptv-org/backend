# Retired backend fixture audit — 2026-09-30

The original `server/tests/api.rs` is retained in Git history at the cleanup
parent revision. Its tests mixed embedded media execution, family organization
and shared account/catalog behavior. Removing the file does not retire shared
behavior. The following names identify the retained/replacement checks; they
are not a claim that host fixtures qualify hardware or production.

## Still-active cases from the removed file

| Original test | Retained/replacement fixture |
| --- | --- |
| `database_waiters_do_not_starve_runtime` | `retired::tests::database_waiters_do_not_starve_runtime_after_engine_removal` holds the DB mutex while concurrent authenticated requests wait and the async timer still advances. |
| `iptv_batches_survive_stalled_provider_and_keep_cursor` | `discovery_error_tests::iptv_discovery_and_guide_explain_failures_without_losing_healthy_sources`, `provider::v2_http_tests::v2_discovery_uses_three_owned_providers_not_live_default_or_foreign_sources`, and incremental `contract_tests::discover` polling; no obsolete SSE claim. |
| `addon_patch_lists_disabled_and_filters_catalogs` | `addon::tests::patch_validation_and_order`, `addon::tests::ordered_catalogs_raw_pagination_search_constraints_and_meta`, `addon::http_v2_tests::account_management_is_encrypted_paged_redacted_and_preserves_identity`. |
| `provider_credentials_redacted_and_url_rejected` | `provider::connections_v2_tests::rejected_checks_never_store_credentials_or_echo_upstream_diagnostics`, `connection_crud_encrypts_immediately_and_preserves_account_defaults`, and protected transport address/redirect tests. |
| `addon_mock_discovery_cursor_and_unsupported_sources` | `addon::tests::catalog_cache_and_protocol_without_skip`, `catalogs_preserve_bounded_normalized_extra_capabilities`, `discovery_error_tests::addon_errors_and_invalid_success_bodies_are_not_silent_empty_results`, and v2 source-job tests. |
| `credential_renewal_preserves_provider_identity_scopes_and_imported_channels` | `provider::connections_v2_tests::connection_crud_encrypts_immediately_and_preserves_account_defaults` renews credentials without replacing IDs/scopes/catalogs. |
| `renewed_credentials_discard_obsolete_catalog_and_guide_fetches` | `provider::refresh_v2::tests::expired_claims_and_configuration_changes_invalidate_old_workers`, `provider::v2_http_tests::live_registration_does_not_stamp_old_urls_with_updated_credentials`, `v2_discovery_revocation_during_series_fetch_does_not_publish_or_cache`. |
| `cancelled_catalog_job_does_not_publish_late_metadata`, `timed_out_catalog_transaction_rolls_back_before_publication` | `provider::refresh_v2::tests::cancelled_inflight_refresh_cannot_publish_late_catalog`, `expired_claims_and_configuration_changes_invalidate_old_workers`, `initial_background_refresh_is_atomic_and_failed_refresh_retains_snapshot`. |
| `catalog_restart_resumes_pending_accounts_and_honors_persisted_due_time` | Persisted refresh claims, expired-claim recovery and `queue_bounds_claims_and_gives_another_account_the_next_slot`; the retired family schedule policy is not retained. |
| `empty_automated_catalog_retains_last_good_scope`, `broken_category_catalog_retains_last_good_identity_evidence` | Authenticated valid empty v2 arrays intentionally replace a catalog (BE-002); malformed/failed refresh keeps the prior snapshot in `initial_background_refresh_is_atomic_and_failed_refresh_retains_snapshot`. This is an approved semantic change, not equivalent legacy empty-array filtering. |
| `live_session_keeps_capacity_until_stop_and_invalidates_old_media`, `cancelled_live_startup_releases_capacity_without_publishing_a_session`, `startup_cancellation_is_scoped_to_the_requesting_auth_session`, `owned_startup_cancel_arriving_before_playback_prevents_any_probe`, `playback_rejects_provider_over_capacity_before_probe` | `gateway::playback_tests::gateway_selection_affinity_renewal_and_grant_revocation_are_scoped`, `stopping_during_gateway_start_releases_the_late_viewer`, `cancelled_or_removed_live_admission_does_not_record_history`, `fresh_playback_skips_full_gateways_and_unassigned_provider_sources_fail_closed`, plus scoped native admission. Media capabilities are owned by the independent gateway. |
| `shared_family_viewers_use_one_input_and_keep_independent_capabilities` | Independent gateway input/output/viewer accounting owns sharing. Backend checks independent logical release/renewal; the isolated real-media fixture verifies gateway delivery. Five compatible viewers / full multi-output stress remains a separate gateway acceptance gate. |

## Retained shared invariants outside the removed file

- History/favorites/Continue Watching: `auth_integration_tests::favorites_and_progress_require_the_exact_selected_profile`, `library_pages_and_atomic_toggle_are_profile_scoped`, `library_corrections_update_history_and_continue_without_losing_source`, `episode_history_is_scoped_to_the_open_series`, `manual_watched_does_not_invent_an_episode_duration`, `home_favorites_page_does_not_let_live_channels_hide_saved_movies`, `playback_after_a_correction_becomes_the_current_episode_immediately`; `contract_tests::viewing_queue_keeps_completed_series_hides_without_erasing_and_pages_titles`, `opaque_episode_progress_roundtrip_rediscovers_without_metadata`, `next_queue_preserves_resume_only_in_final_seconds`, and `next_api_reads_metadata_envelope_then_discovers_actual_iptv_episode`.
- Access/auth/CSRF: complete `auth::tests`, retained `auth_integration_tests`, `tests/auth_http_flow.rs`, `auth_migration.rs`, `auth_surface.rs`. Legacy-only endpoint expectations now require `client_update_required`; active v2 source-job requests retain account/profile/session denial checks. Old bearerless backend-media and SSE tests are retired, not relabeled as gateway evidence.
- Manual VOD matches/defaults: `provider::v2_http_tests::account_matches_are_paged_and_cursors_cannot_cross_accounts`, `match_edits_and_defaults_are_account_owned_not_owner_role_global`, and conservative `provider::tests::matching`. Family matching/quality ordering is separately retired.
- `/catalogs`, `/discover`, `/meta`: their handlers remain. `addon::tests::ordered_catalogs_raw_pagination_search_constraints_and_meta`, custom/required filter and fallback tests, kids metadata/episode membership tests, and source/header/privacy contract fixtures remain.
- Basic account playback/safe failures: all `gateway::playback_tests`, `service_errors::tests`, `discovery_error_tests`, source-cookie/header/privacy fixtures, mandatory gateway/no-family-fallback tests, revocation and delayed-start cleanup tests. No backend engine idle counter is asserted after the engine has been removed.
- Preservation: existing owner/addon encryption CLI fixtures plus four offline retirement fixtures prove export/backup durability, ownership/encryption refusal, wrong-key/dependency rollback, unchanged IDs/history/manual matches and no schema resurrection on boot. `retired::tests` additionally preserves exact raw/composite favorite IDs and historical quality JSON with the new quality-free writer.

## Intentionally retired only

All original `family_*`, `automatic_family_*`, matching alias/fuzzy/coast/feed/
quality-order/grouping/repair fixtures target the retired organizer. So do
`bulk_xtream_import_*`, `bulk_accounts_bound_workers_and_metadata_and_enforce_owner_access`,
shared account-pool observations/external reserves and stale/late pool reports;
`scheduled_catalog_job_refreshes_twenty_accounts_with_partial_failure_and_matching`;
`health_exclusion_and_activity_undo_preserve_manual_intent`,
`guide_refresh_persists_last_valid_and_owner_repair_uses_exact_feed`,
`health_jobs_publish_media_observations_and_scheduler_recovers_a_failure`,
`real_health_decodes_media_and_rejects_http_success_with_invalid_content`,
`oversized_provider_guide_uses_selected_feeds_and_retains_last_good`,
and `us_live_view_filters_sections_and_searches_only_current_programmes`.
Their configuration is privately exported before offline deletion; raw Xtream
guide/catalog behavior and personal history remain under the retained v2 tests.
Embedded/family failover and recovery are not falsely claimed as v2 gateway
migration or hardware acceptance.
