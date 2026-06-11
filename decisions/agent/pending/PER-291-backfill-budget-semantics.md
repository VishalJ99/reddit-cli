# PER-291 backfill budget semantics

## Context

`DESIGN.md` calls for `rdt sync --backfill <sub> --days D` to walk recent subreddit posts and deep-capture their comment trees. This differs from normal watch sync: watch sync polls flat streams, while backfill fans out from each selected post into the full thread resolver.

## Decision

Backfill uses three separate bounds:

- `--pages` caps `/r/{sub}/new.json` listing pages scanned.
- `--budget` caps candidate post threads selected for deep capture.
- `--max-requests` caps resolver requests per selected thread.

Each scanned listing page uses Reddit's 100-item page limit; `--budget` only limits how many candidate posts get deep-pulled. Backfill deliberately does not inherit watch-level or global flat-sync budgets. Its defaults are 2 days, 3 candidate posts, 1 listing page, and 10 resolver requests per selected thread. A backfill run emits one aggregate `kind = backfill` report. `gap` means the listing page cap, candidate post budget, or a per-thread resolver cap stopped a complete catch-up.

## Consequences

Small backfills stay polite on Reddit and predictable for local runs. Users can raise `--budget` and `--pages` to inspect more recent posts, and raise `--max-requests` or run `rdt pull` on a specific thread when one post has unusually deep comment growth.
