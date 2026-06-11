# reddit-cli

`reddit-cli` is a Rust project that builds `rdt`, a strictly read-only Reddit CLI.

The design goal is to find, read, capture, and locally summarize Reddit activity without ever automating posts, comments, votes, moderation actions, or account writes. Anything worth replying to should be opened or copied and handled manually in Reddit's own UI.

See [DESIGN.md](DESIGN.md) for the full system design.

## Current Status

This is the bootstrap implementation for the public project:

- `rdt search`, `rdt browse`, `rdt thread`, `rdt thread --all`, `rdt comment`, `rdt subs`, `rdt sub`, and `rdt user` are wired through the transport/parser/rendering stack.
- Transport selection supports configured cookie JSON, anonymous JSON, and `--rss` degraded mode. JSON edge-block detection falls back to RSS where a corresponding feed exists.
- `rdt copy`, `rdt open`, `rdt save`, `rdt saved`, `rdt watch add/rm/ls`, and `rdt db path/query/search` are scaffolded on local XDG state.
- The SQLite schema from the design is present with watches, posts, comments, sync log, saved links, and FTS tables.
- `rdt sync` populates posts and comments for active watches or explicit `--sub` targets, and `rdt sync --refresh` hydrates recent stored posts through `/api/info.json`. `rdt pull` deep-captures a selected post into SQLite using the same read-only thread resolver.

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

Use:

```sh
rdt db path
rdt watch add rust programming
rdt watch ls
rdt sync --sub rust --budget 300
rdt sync --sub rust --refresh --budget 100
rdt sync --sub rust --budget 500 --pages 5
rdt pull 1 --max-requests 10
rdt save 1 --note "worth reading later"
rdt saved
```

`rdt sync` uses active watches by default. `--sub` targets an explicit subreddit without a separate watch command. For a deeper catch-up run, raise both the hard item cap (`--budget`) and the page cap (`--pages`). `--refresh` also hydrates recent stored posts for each synced subreddit; it uses the same caps and logs `kind = refresh`.

`rdt pull` is the heavier path for an interesting post. It expands hidden comment stubs up to `--max-requests`, writes the post/comments to SQLite, and logs `kind = backfill`; if the cap is reached, the report status is `gap`.

## Read-Only Boundary

This project intentionally has no Reddit write commands. Do not add automated posting, commenting, voting, saving to Reddit, reporting, moderation, messaging, or profile mutation. Local `save` means "save a permalink into the local SQLite database", not Reddit's saved-items feature.

## License

No open-source license has been selected yet. The repository can be public before a license is chosen, but reuse rights should be decided explicitly.
