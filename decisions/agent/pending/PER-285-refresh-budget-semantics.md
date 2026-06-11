# PER-285 refresh budget semantics

## Context

`DESIGN.md` defines `rdt sync --refresh` as a hydration pass over recent stored posts using `/api/info.json`. The refresh pass needs explicit bounds because it can run as part of regular watch sync.

## Decision

Reuse the existing sync caps for refresh:

- `--budget` is the hard maximum number of stored posts selected for refresh per subreddit.
- `--pages` caps the number of `/api/info.json` batches, with up to 100 ids per batch.
- The initial default remains `--budget 300`, which yields at most three refresh requests per subreddit when `--pages` is not supplied.
- Refresh logs `status = gap` and reports `remaining_items` when stored posts exist outside the cap.

## Consequences

The operator does not need a separate refresh-specific flag to control cost. A normal `rdt sync --refresh` stays bounded, while deeper refresh passes can raise both `--budget` and `--pages`, matching the watch-sync catch-up semantics.
