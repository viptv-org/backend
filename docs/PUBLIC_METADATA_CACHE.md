# Public provider cache

Only anonymous HTTPS requests to `v3-cinemeta.strem.io/meta/movie/tt*.json`
and `meta/series/tt*.json` share the persistent SQLite cache. URLs with query
parameters, credentials, alternate hosts or noncanonical identifiers use the
existing scoped provider path. Authorization and enabled-provider checks still
precede metadata lookup. The cache stores raw public documents, never composed
profile responses, watch state or provider credentials.

| Document | Refresh after | Maximum stale age |
| --- | --- | --- |
| Complete movie | 24 hours | 30 days |
| Movie without a synopsis | 15 minutes | 30 days |
| Series and episode metadata | 15 minutes | 7 days |

A stale hit returns immediately while one shared refresh runs. Refresh failure
retains the known document and backs off for one minute. Cold requests coalesce;
canceling their caller does not strand the next request. Invalid identities and
missing documents are not persisted. Storage is bounded to 10,000 entries,
256 MiB total and 8 MiB per document. Existing search and catalog caches remain
unchanged. No client metadata database is required.

# Prepared public clearlogos

`GET /api/images/clearlogo/v1/tt<digits>.png` loads only the public Metahub
clearlogo for that IMDb identifier. It accepts no upstream URL or credentials.
The existing bounded public image service coalesces requests and caches the
prepared image. PNG decoding has explicit byte, dimension and pixel limits and
runs outside the async executor. Processing trims transparent padding, removes
opaque neutral mattes and makes dark monochrome wordmarks legible; colored
artwork retains its colors. Unsupported artwork fails to the client's text title.

Successful responses have an ETag, public CORS and
`public, max-age=86400, s-maxage=604800, stale-while-revalidate=604800`.
They set no cookies and are eligible for shared image caching. Other metadata
API responses retain their private/no-store policy because they contain account,
profile and watch-state fields; no blanket Cloudflare API cache rule is suitable.
TV clients opt in through `clearlogo_image_supported`; older clients and servers
retain the existing image path.

Validation: 936 default-feature tests and 948 live-runtime tests pass, including
public/private URL isolation, persistent reconstruction, concurrent cold reads,
stale refresh, failures, cancellation, image transparency and conditional requests.
The engine snapshot check passes without changes to the shared engine.
