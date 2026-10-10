# Shared torrent runtime native protocol

The owner-authorized refactor follows design
`16277caf2c7bf16296e25da40f143666465f0d2b`. Performance work is deferred in the
gateway's `docs/TORRENT_PERFORMANCE_FOLLOWUP.md`; production rollout remains
separately coordinated.

GET `/api/v2/torrent-runtime-protocol` is authenticated, admitted-profile scoped,
bodyless and `no-store`. It returns the closed version-2 support object.
The existing native-v1 route and response remain unchanged. Negotiation records
are separate, so negotiating one protocol does not authorize the other.

Android, Android TV and desktop may request native transport version 2 with
`network_policy: public_discovery_verified_v2`. Authoritative source, account,
profile, session, exact-VOD and cancellation proofs remain required. Native v2
torrent requests do not substitute gateway delivery when negotiation, selection
or transport requirements fail. Web/Roku retain gateway delivery.

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
lints allowed passes. Installed apps have not yet adopted this protocol; native
holder and platform adapter integration remain separate work.

The exported core is `0f3d9f78e6df7e9550a9335b835dbebef454a3b5`, matching
the Android and shared viewing client pins. It includes the private v2 client
holder and canonical stage/format failures. The full backend suite passes with
one test thread (289 tests, 4 ignored), plus its CLI integration tests. A parallel
run lost the existing diagnostic-capture assertion's span fields; the serial run
passes. Strict Clippy passes with the same two pre-existing lint classes allowed
(`type_complexity` and `cloned_ref_to_slice_refs`); no new allowances were added.

## Gateway decoder acknowledgement

POST `/api/v2/playback-decoder-start` takes the existing closed playback start
envelope. Its route-selected mode is part of idempotency identity; it is not an
extra JSON field. The original start route and native v1 negotiation stay
compatible. Ordinary HTTP delivery retains its existing admission rules.

For torrent gateway inputs, selection requires the gateway's separate v2
decoder-acknowledgement protocol, including when considering an existing input's
affinity. Unsupported gateways are refused before source creation. The backend
uses `/v2/torrent-sessions` and retains the original gateway viewer identity.

POST `/api/v2/playback/:id/first-frame` accepts exactly `{}`. It checks the
current account/profile/session, source ownership/configuration, ready lease
and gateway configuration before brokering the scoped acknowledgement. It does
not update backend touch time or gateway expiry. Ordinary direct/legacy gateway
sessions require no runtime acknowledgement; native sessions report directly to
their local worker. Stop and replaced authority still revoke access.

The full backend suite passes 292 library tests with four explicit ignored
fixtures, plus all CLI/integration tests. New regressions cover v2 gateway
selection/refusal, unchanged authority expiry, route-mode idempotency conflicts,
closed bodies and source revocation before acknowledgement.
