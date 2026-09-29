# V2 source-discovery and guide failures

Backend contract evidence only: clients have not all adopted v2, and this does
not claim complete gateway/native-player error parity or production verification.

## Response shape

Control/guide failures return JSON `error` (readable English recovery message)
and `error_code` (stable classification). Discovery request JSON/query failures
and missing/expired discovery jobs use this shape too. Authentication/profile
failures retain the existing auth response contract.

Source discovery is incremental. A successful HTTP poll may contain **failed
producer events alongside successful sources**. Each failed event carries its
existing `seq`, trusted producer identifier, `streams`, `error` and `error_code`.
`done:true` means all producers finished; it does not mean all succeeded. Clients
must inspect event errors rather than treating HTTP 200 or `done` as proof of
successful discovery. Preserve healthy sources and show failures against their
producer. A registration warning can accompany usable streams.

Empty `streams:[]` is valid. A successful addon response lacking an array-valued
`streams` field is invalid, not a silent empty result. Unsupported source URLs,
torrents and external-player entries produce `source_format_unsupported` rather
than a generic provider failure.

## Important distinctions

| Code | Evidence and recovery |
| --- | --- |
| `provider_credentials_rejected` | Provider HTTP 401/403 or rejected/inactive login; check provider access, credentials or subscription. It does not log the viewer out of VIPTV. |
| `provider_rate_limited` | Provider HTTP 429 during API work; wait before retrying. This is **not** inferred to be a stream connection limit. |
| `provider_connection_limit` | Trusted backend admission or an explicit gateway connection-limit failure; stop another stream or choose a provider. Generic 403/429/5xx is insufficient evidence for this code. |
| `provider_timeout` | Request or response-body deadline expired, including candidate deadline; retry later or choose another provider. |
| `provider_response_interrupted` | Response body ended or failed before completion; retry later. |
| `provider_protocol_invalid` | Invalid JSON or invalid episode/guide/index shape; the response is not treated as empty data. |
| `provider_discovery_failed` | Unclassified IPTV failure; safe fallback without echoing diagnostics. |
| `addon_access_denied`, `addon_rate_limited`, `addon_timeout` | Addon-specific failures, not backend account authentication or IPTV connection allowance. |
| `addon_protocol_invalid`, `addon_response_too_large`, `addon_unavailable` | Invalid/oversized addon response or safe fallback; another addon can still succeed. |

Storage/key failures, changed source configuration and unavailable source IDs
retain their own safe codes when known. Guides preserve these classifications
rather than collapsing every failure into `source_unavailable`. Unknown raw
messages never become public copy on v2 discovery, even if they contain a URL,
token, password or a phrase resembling a known failure.

Registration, refresh and discovery share the provider classification boundary.
Classification uses exact internal messages/codes, not speculative substring
matching of provider bodies. Reqwest timeout predicates are checked without
formatting its errors; those errors can contain credential-bearing URLs.

## Evidence and remaining gaps

Synthetic HTTP fixtures cover provider 401, 429, 503, invalid JSON and invalid
successful payloads across source events and guide HTTP responses. A healthy
second provider remains available. Addon fixtures cover access denial, rate
limiting, unavailable service and malformed success responses. All assert that
private fixture diagnostics, URLs and tokens do not appear in public failures.

Malformed discovery JSON, invalid polling query parameters and missing jobs have
structured-error fixtures. Socket fixtures verify request and body timeouts and
an interrupted body. These are server fixtures, not screenshots or physical
device playback tests.

Client adoption, gateway upstream-media classification, decoder/native-player
errors, remaining endpoint/extractor consistency and legacy removal remain open.
Do not claim the full cross-platform error requirement is complete from these
backend tests alone.
