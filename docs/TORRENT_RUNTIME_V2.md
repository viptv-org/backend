# Shared torrent runtime native protocol

The refactor follows the gateway's `docs/TORRENT_PERFORMANCE_FOLLOWUP.md`;
production rollout remains separately coordinated.

GET `/api/torrent-runtime-protocol` is authenticated, admitted-profile scoped,
bodyless and `no-store`. It returns the closed version-2 support object.
The existing native-v1 route and response remain unchanged. Negotiation records
are separate, so negotiating one protocol does not authorize the other.

Android, Android TV and desktop may request native transport version 2 with
`network_policy: public_discovery_verified_v2`. Authoritative source, account,
profile, session, exact-VOD and cancellation proofs remain required. Native v2
torrent requests do not substitute gateway delivery when negotiation, selection
or transport requirements fail. Web retains gateway delivery.

The private version-2 grant preserves canonical hash identity, metainfo/magnet
input, validated tracker hints and optional file/archive indices. Missing file
selection delegates largest-file resolution to the local shared runtime. Grant
identity remains immutable during twenty-second heartbeat renewal; authority
lasts at most sixty seconds, and poll never extends it. Release/cancellation and
source/account/profile revocation retain the existing durable authority rules.

Metainfo grants use canonical info bytes while preserving vetted outer trackers
separately. Runtime validation permits payloads larger than the aggregate cache;
the local runtime enforces its bounded piece storage. Native v1's validation and
exact-file requirements remain separate. Backend v2 native preparation uses the
120-second startup budget; on-device acquisition/player opening share the
remaining budget through their adapters.

Local evidence: full backend tests pass, including phone/TV/desktop v2 grants,
automatic file selection, tracker retention, immutable renewal/release and
no gateway substitution. Strict Clippy exposed pre-existing type-complexity and
cloned-reference warnings in unchanged files; checking with those two baseline
lints allowed passes. Native holder and platform adapters are integrated in the
coordinated consumer branches; physical-device qualification and installed
production delivery remain separately coordinated.

The exported core includes the private v2 client holder and canonical
stage/format failures.

## Gateway decoder acknowledgement

POST `/api/playback-decoder-start` takes the existing closed playback start
envelope. Its route-selected mode is part of idempotency identity; it is not an
extra JSON field. The original start route and native v1 negotiation stay
compatible. Ordinary HTTP delivery retains its existing admission rules.

For torrent gateway inputs, selection requires the gateway's separate v2
decoder-acknowledgement protocol, including when considering an existing input's
affinity. Unsupported gateways are refused before source creation. The backend
uses `/v2/torrent-sessions` and retains the original gateway viewer identity.

POST `/api/playback/:id/first-frame` accepts exactly `{}`. It checks the
current account/profile/session, source ownership/configuration, ready lease
and gateway configuration before brokering the scoped acknowledgement. A
gateway still reporting `session_not_ready` is polled for a bounded wait; it
never ends the playback entry, and an unanswered wait returns the retryable
`playback_starting`. It does not update backend touch time or gateway expiry.
Ordinary direct/legacy gateway
sessions require no runtime acknowledgement; native sessions report directly to
their local worker. Stop and replaced authority still revoke access.

Coverage includes v2 gateway selection/refusal, unchanged authority expiry,
route-mode idempotency conflicts, closed bodies and source revocation before
acknowledgement.
