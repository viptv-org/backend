# Public image delivery

API clients can send `x-viptv-image-delivery: direct` for native/desktop images, or `proxy` when a browser fetcher needs CORS. Opted-in JSON responses translate only recognized Simkl asset fields; unrelated assets, verified MetaHub logos, descriptions and credentials remain unchanged. Missing/unknown negotiation preserves legacy responses. Wire models and native bindings remain unchanged.

Direct references point to the official Simkl CDN. Proxy references are relative `/api/v3/images/simkl/{folder}/{hash}_{size}.{extension}` and must resolve against the client's active API origin, including approved native/LAN aliases. No canonical origin or authentication token is embedded in an image URL. Recognized resolved proxy paths in saved cards can also be translated without fetching their original host.

The unauthenticated GET/HEAD image route accepts only the known `posters`, `fanart` and `episodes` descriptors, supported sizes/extensions, and bounded alphanumeric path segments. It cannot accept arbitrary URLs, hosts, query credentials or traversal. Fanart's legacy `_w` alias maps to the documented `_medium` size for opted-in delivery. Images are fetched through pinned public DNS, HTTPS, no redirects, no ambient proxies, and no forwarded cookies/authorization. Upstream bodies have a 3-second deadline and 2 MiB limit; six requests may fetch concurrently. A coalesced cache is bounded to 128 entries/32 MiB, with one-hour positive and ten-second negative entries. The strict byte budget remains enforced when entries are active.

Returned bytes must have JPEG, PNG or WebP signatures. The endpoint sets the derived image Content-Type, `nosniff`, public one-day immutable caching and `Access-Control-Allow-Origin: *`. Public image OPTIONS supports ordinary image headers and the negotiation header; it does not widen authenticated API origin/credential policy. Authentication headers and cookies are unnecessary for public images.

Qualification found the real Season 2 episode paths valid as `_w.webp`; direct targets omitted CORS headers. The external wsrv proxy showed client/WAF variability and a 3–4-second native cold fetch. Actual negotiated direct and owned proxy images returned 200 WebP, with warm owned-proxy reads around 5 ms. Performance still depends on the public CDN and connection warm-up; private aggregate measurements remain in local proof files rather than git.

Provider reference: [official image conventions](https://api.simkl.org/conventions/images).
