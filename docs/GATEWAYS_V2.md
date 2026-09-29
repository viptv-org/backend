# Account gateway configuration and encrypted keys

This is development-branch functionality, not a completed playback cutover.
Gateway registration, validation and grants are implemented; playback selection,
affinity and media-session forwarding are not connected yet. Legacy embedded
playback remains until the coordinated client/server cutover. Do not deploy this
checkpoint with production secrets or infer public multi-tenant readiness.

## Operator-managed encryption

VIPTV_SECRETS_KEYRING is a JSON object with an active key ID and up to eight
named keys. Each value is the standard-Base64 encoding of 32 random bytes:

```json
{"active":"master_2026","keys":{"master_2026":"BASE64_ENCODED_32_BYTE_KEY"}}
```

The example is a placeholder, not a usable key. Supply real material through
private deployment-secret configuration, never a repository or public response.
Malformed configuration stops backend startup. Leaving it unset does not create
an insecure default: saving gateway keys fails with secret_store_not_configured.
Keep the keyring separately from database backups. Losing it makes saved
credentials unrecoverable unless the correct keys are restored.

Secrets use AES-256-GCM with a fresh OS-random 96-bit nonce. Authenticated context
includes format version, key ID, account ID, purpose and record ID. Moving or
altering ciphertext across records/accounts fails authentication. The database
contains a versioned key-ID/nonce/ciphertext envelope, not the integration key.
Debug output is redacted, and temporary plaintext buffers are zeroized.

New writes use the active key; older retained keys can still decrypt their
envelopes. Automatic bulk re-encryption is not implemented. Do not remove an old
key until every affected record has been deliberately re-encrypted. This change
does not yet migrate existing provider/addon secrets out of their legacy storage.

## Management API

All endpoints require a full account session and parent authorization when a
selected kids profile is locked. Paired-device sessions cannot manage settings.

- GET /api/v2/gateways: permitted gateway metadata and encryption-configuration
  status. No integration keys or ciphertext are returned.
- POST /api/v2/gateways: name, endpoint, namespace, integration_key, optional
  priority (default 100). The endpoint/key/scopes are checked before saving.
- PATCH /api/v2/gateways/:id: change name, priority and/or enabled. Cosmetic or
  ranking changes do not change the credential revision.
- PUT /api/v2/gateways/:id: replace connection configuration with an explicitly
  supplied key, preserving the gateway ID and incrementing its revision. An
  existing hidden key is never silently sent to a newly entered endpoint.
- POST /api/v2/gateways/:id/check: verify the saved credential and namespace.
- DELETE /api/v2/gateways/:id: idempotent owner-only removal.
- PUT /api/v2/gateways/:id/grants: account_id plus enabled, for a server operator
  explicitly granting/revoking access to its own gateway.

A registered gateway is private to its owning account. There is no implicit
global/family default and no public grant. A grant permits use/checking, not
editing another account's connection. Operator role does not bypass ownership.
Recipients never receive the integration key. Users may also register their own
connection using a URL and scoped key supplied to them directly.

Use an integration key beginning with pgk_, not the gateway's bootstrap API_KEY.
The key must authorize the selected namespace and capabilities/create/read/renew/
release operations. The gateway's capability response must report these scopes.
Wrong gateway credentials return a configuration error, not a backend-login 401.

Endpoint URLs must be public HTTPS base URLs and cannot contain userinfo, query
parameters or fragments. A reverse-proxy path prefix is supported. This rule is
for gateway control endpoints; IPTV source URLs may still use HTTP. Private
gateway destinations and operator allowlists are not implemented in this slice.

Each control request resolves, validates and pins destination addresses while
retaining the original TLS hostname. Redirects and inherited proxies are
disabled. DNS/connect/body time and response size are bounded; at most four
gateway checks run concurrently. Registration rejects missing scopes, unready
services, incompatible responses and duplicate endpoint/namespace records.

Limits are 16 owned and 64 total authorized connections per account. Authenticated
API responses use no-store. Existing sessions and grants are revalidated before
the result of an in-flight connection check is returned.

## Evidence and remaining work

Tests cover ciphertext randomization, tampering, account/purpose/record binding,
old-key decryption, no plaintext fallback, redacted output, explicit grants,
operator-role boundaries, missing/wrong keyrings, private/reserved destinations,
redirect rejection, scope failures and bounded capability responses. Network
fixtures use a test-only loopback override, never a production configuration flag.

An opt-in interoperability test starts the independent gateway executable, issues
a real scoped integration key through its API, then registers and checks it through
the backend router. Run it with VIPTV_TEST_GATEWAY_BINARY, VIPTV_TEST_FFMPEG and
VIPTV_TEST_FFPROBE set. This verifies the HTTP credential/capability contract,
not playback-session forwarding or a public HTTPS deployment.

These checks do not establish complete DNS-rebinding/TLS deployment qualification,
playback selection/affinity, session forwarding, provider/addon encryption,
live grant revocation of media leases, or process isolation after legacy removal.
Those remain acceptance work before production deployment.
