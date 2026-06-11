# PER-287 HTTP cache TTL

## Context

`DESIGN.md` calls for repeated read GETs to use a disk cache with a 5 minute TTL, while sync bypasses cache. The implementation also needs to respect `--fresh`.

## Decision

Cache ordinary read GET response bodies under the XDG cache directory at `http/`, using a SHA-256 hashed cache filename derived from response mode, cookie identity, and URL. Cookie identity is represented only as a hash inside the final hashed key input. The default TTL is 300 seconds and can be overridden with `cache_ttl_secs` in `config.toml`.

`--fresh` bypasses cache reads but still writes the new successful response. Sync listing, `/api/info` refresh hydration, auth checks, and pull/backfill thread capture use uncached transport paths.

## Consequences

Repeated browse/search/thread/user/sub reads are cheaper and faster within the TTL. Cache files do not expose cookies or raw URLs in filenames. The cache is intentionally best-effort and response-body-only; validator headers such as ETag are left for a later slice.
