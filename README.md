# reddit-cli

`reddit-cli` is a Rust project that builds `rdt`, a strictly read-only Reddit CLI.

The design goal is to find, read, capture, and locally summarize Reddit activity without ever automating posts, comments, votes, moderation actions, or account writes. Anything worth replying to should be opened or copied and handled manually in Reddit's own UI.

See [DESIGN.md](DESIGN.md) for the full system design.

## Current Status

This is the bootstrap implementation for the public project:

- `rdt search`, `rdt browse`, `rdt thread`, `rdt thread --all`, `rdt comment`, `rdt subs`, `rdt sub`, and `rdt user` are wired through the transport/parser/rendering stack.
- Transport selection supports configured cookie JSON, anonymous JSON, and `--rss` degraded mode. JSON edge-block detection falls back to RSS where a corresponding feed exists, and ordinary read GETs are cached for repeated browsing.
- `rdt copy`, `rdt open`, `rdt save`, `rdt saved`, `rdt watch add/set/rm/ls`, and `rdt db path/query/search` are scaffolded on local XDG state.
- The SQLite schema from the design is present with watches, posts, comments, sync log, saved links, and FTS tables.
- `rdt sync` populates posts and comments for active watches or explicit `--sub` targets, `rdt --rss sync` provides degraded RSS capture when JSON is unavailable, and `rdt sync --refresh` hydrates recent stored posts through `/api/info.json`. `rdt pull` deep-captures a selected post into SQLite using the same read-only thread resolver.

## Build

`wreq` is used for browser-style HTTP emulation, matching the design. Its BoringSSL dependency requires `cmake`.

```sh
brew install cmake
cargo build
```

Run tests:

```sh
cargo test
```

Run the CLI locally:

```sh
cargo run -- search "rust cli" -r rust -n 5
cargo run -- browse rust --sort new -n 5
cargo run -- thread 1 --all --max-requests 10
```

The installed binary name is `rdt`:

```sh
cargo install --path .
rdt search "screen time api" -r swift -n 10
```

## Auth And Transport

The transport order is:

1. Cookie JSON if `RDT_COOKIE` or `cookie` in config is set.
2. Anonymous JSON unless `--rss` is forced.
3. RSS degraded fallback when Reddit returns an HTML edge block.

Cookie options:

```sh
export RDT_COOKIE='reddit_session_value_only'
rdt auth check
```

Or:

```toml
# Primary macOS path:
# ~/Library/Application Support/com.dross.rdt/config.toml
#
# Compatibility path also supported:
# ~/Library/Application Support/rdt/config.toml
cookie = "reddit_session_value_only"
```

`rdt auth from-browser` is reserved for the later browser-cookie feature.

## Local State

`rdt` uses XDG/project directories via the `directories` crate:

- Config: `config.toml`
- Database: `rdt.db`
- Last numbered results: `last.json`
- HTTP cache: `http/`

Use:

```sh
rdt db path
rdt watch add rust programming
rdt watch add adhd --pages 5 --budget 500 --refresh
rdt watch set adhd --pages 8 --budget 800
rdt watch ls
rdt sync --sub rust --budget 300
rdt --rss sync --sub rust --budget 25 --pages 1
rdt sync --sub rust --refresh --budget 100
rdt sync --sub rust --budget 500 --pages 5
rdt sync --backfill rust --days 2 --budget 3 --pages 2 --max-requests 10
rdt pull 1 --max-requests 10
rdt digest --since 24h --sub rust
rdt digest --since 24h --sub rust --json
rdt save 1 --note "worth reading later"
rdt saved
```

`rdt sync` uses active watches by default. `--sub` targets an explicit subreddit without a separate watch command. The subreddit comment stream is flat and newest-first but includes comments from any depth, so watch sync has no tree-depth cutoff; it is bounded by page and item caps instead. For a deeper catch-up run, raise both the hard item cap (`--budget`) and the page cap (`--pages`). CLI flags override watch settings for that run; otherwise per-watch settings override global config/defaults. `--refresh` also hydrates recent stored posts for each synced subreddit; it uses the same caps and logs `kind = refresh`.

`rdt --rss sync` uses Reddit's Atom feeds for `/new` and `/comments` and writes the same local SQLite tables with reduced fidelity. RSS sync is useful when JSON transport is blocked, but scores and comment parent metadata are unavailable; comments keep their post association when the permalink exposes it. Stored posts and comments carry `source` provenance (`json` or `rss`) so degraded rows are auditable; later RSS sightings do not downgrade rows already hydrated from JSON. RSS feeds also do not provide the JSON `after` cursor used for overlap pagination, so a full RSS page reports `gap`. Forced RSS sync skips JSON refresh; if Reddit throttles one feed, the command logs and prints that stream as `error` while keeping any successful stream rows.

The intended capture pattern is broad and cheap first, then selective depth. A watch polls `/new` and `/comments`, which is enough for the mostly-flat activity across a subreddit. For an interesting post, use `rdt pull N` from the last listing or `rdt pull URL --max-requests 50` for an arbitrary thread URL. `rdt pull` expands hidden comment stubs up to `--max-requests`, writes the post/comments to SQLite, and logs `kind = backfill`; if the cap is reached, the report status is `gap`. Arbitrary listing URL watches are not part of the current schema; ongoing watches stay subreddit-scoped.

`rdt sync --backfill SUB --days D` is the bounded catch-up path for recent posts from a subreddit. It scans `/new` up to `--pages` at Reddit's 100-item listing limit, deep-pulls up to `--budget` posts newer than the day cutoff, and gives each selected thread `--max-requests` resolver requests. Defaults are `--days 2`, `--budget 3`, `--pages 1`, and `--max-requests 10`; backfill does not inherit watch or global flat-sync budgets. The command emits one aggregate `kind = backfill` report; `gap` means a page, post, or thread expansion cap stopped a complete catch-up.

Ordinary read commands use a disk-backed HTTP cache with a 5 minute default TTL. Use `--fresh` to bypass cache reads and refresh the stored response. Watch sync, refresh hydration, auth checks, and pull/backfill capture bypass this cache by design. Cache keys separate anonymous reads from each configured cookie identity without writing cookies or raw URLs into filenames. Set `cache_ttl_secs` in `config.toml` to override the TTL.

`rdt digest` reads only the local SQLite database. It emits grouped post/comment activity as compact Markdown by default, or structured JSON with `--json`, so an LLM can summarize recent captured activity without scraping Reddit's UI:

```sh
rdt sync --sub programming --refresh
rdt digest --since 24h --sub programming | claude -p "Summarize and flag the threads most worth opening."
```

Digest output is bounded for terminal and prompt use: up to 50 post groups, 10 comments per post, and 25 comments whose post is not present locally. Use `rdt db query ... --json` for exhaustive exports.

## Read-Only Boundary

This project intentionally has no Reddit write commands. Do not add automated posting, commenting, voting, saving to Reddit, reporting, moderation, messaging, or profile mutation. Local `save` means "save a permalink into the local SQLite database", not Reddit's saved-items feature.

## License

MIT. See [LICENSE](LICENSE).
