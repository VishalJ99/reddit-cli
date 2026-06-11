# PER-282 watch sync depth and overrides

## Context

The design says subreddit watch sync should continuously capture posts and comments into SQLite, while full thread expansion is handled by `rdt pull` / `rdt thread --all`. The user clarified that most Reddit comment activity stays shallow, with a few high-interest posts accumulating the deep branches.

## Decision

Use shallow, flat subreddit firehose sync as the default watch path:

- `rdt sync` polls `/r/{sub}/new.json` and `/r/{sub}/comments.json` for active watches.
- Comment sync has no tree-depth cutoff because `/comments.json` already returns comments from any depth; it is bounded by page/item/request limits instead.
- Explicit `rdt sync --sub ... --pages ... --budget ...` may override default watch limits for arbitrary subreddits and creates or refreshes a normalized watch row so future watermarks have a durable home.
- Deep completeness for selected high-interest posts belongs in the follow-on `rdt pull <post>` / `rdt thread --all` implementation with its own `--max-requests` and optional depth controls.

## Consequences

Routine watch runs stay cheap and polite. Busy subreddits can still log `gap` when new activity exceeds the configured page cap, and the operator can temporarily raise `--pages` / `--budget` or run an explicit sync on a subreddit. Post-level deep capture remains a separate path so the few unusually deep threads can receive heavier treatment without making every watch cycle expensive.
