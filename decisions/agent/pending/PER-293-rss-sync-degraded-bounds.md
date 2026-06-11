# PER-293 RSS sync degraded bounds

## Context

`DESIGN.md` keeps RSS as the zero-auth fallback when Reddit JSON is unavailable. Reddit Atom feeds expose subreddit `/new` and `/comments` activity, but they do not provide the same metadata as JSON listings.

## Decision

`rdt --rss sync` persists RSS feed entries into the existing SQLite `posts` and `comments` tables with degraded metadata:

- post and comment IDs are normalized from Atom IDs/permalinks into the same base36 form used by JSON sync,
- comment `post_id` is recovered from the permalink when available,
- comment `parent_id`, scores, ratios, and comment counts remain absent when RSS does not provide them,
- stored rows carry source provenance (`source = 'rss'`) through the additive source-column migration implemented under PER-294,
- RSS sync uses uncached feed reads like JSON sync,
- forced RSS sync skips `/api/info.json` refresh even when watch/config refresh is enabled,
- RSS stream fetch errors are logged and returned as per-stream reports so successful RSS streams remain visible,
- full RSS pages report `gap` because RSS feeds do not expose the JSON `after` cursor used for overlap pagination.

## Consequences

RSS sync keeps capture alive on blocked networks and preserves enough post/comment association for local search and digest workflows. It does not reconstruct comment trees; users should use JSON sync or `rdt pull` when parent metadata and full-depth expansion are required.

The source-provenance column was approved by the user and implemented as a follow-up in PER-294.
