---
name: reddit-cli
description: Use when working on reddit-cli or operating rdt, including building, testing, syncing Reddit activity into local SQLite, inspecting digests/search data, documenting install paths, or maintaining the read-only Reddit boundary.
---

# reddit-cli

Use this skill for repository work on `reddit-cli` and for local operation of the `rdt` binary.

## Ground Rules

- Follow `AGENTS.md` first. It contains the current project traceability, ticket, review, and commit rules.
- Treat `DESIGN.md` as the product and architecture source of truth.
- Keep Reddit strictly read-only. Do not add or run automated Reddit posting, commenting, voting, reporting, moderation, messaging, profile mutation, or Reddit saved-item writes.
- Do not print cookies, auth headers, session values, or config secrets. The local cookie config is outside the repo.
- Keep runtime state out of the repository. Use `rdt db path` and the platform application-support directory for databases, caches, logs, and watcher state.

## Build And Validate

Use the repo's normal Rust checks:

```sh
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
git diff --check
```

`wreq` depends on BoringSSL and needs `cmake` installed on macOS:

```sh
brew install cmake
```

When using a temporary `HOME` for smoke tests, keep Cargo's cache stable. Build first, then invoke `target/debug/rdt` with the temporary `HOME`, or set `CARGO_HOME` to the real user cache before overriding `HOME`.

## Common Operations

Inspect paths and local data:

```sh
rdt db path
rdt db query "SELECT COUNT(*) AS n FROM posts" --json
rdt digest --since 24h --sub rust
```

Capture broad subreddit activity cheaply:

```sh
rdt watch add adhd --pages 1 --budget 100 --refresh
rdt sync --sub adhd --pages 1 --budget 100 --refresh
```

Capture selected depth:

```sh
rdt pull 1 --max-requests 10
rdt sync --backfill adhd --days 2 --budget 3 --pages 1 --max-requests 10
```

Use RSS only as degraded fallback or when forced by the task:

```sh
rdt --rss sync --sub adhd --pages 1 --budget 25
```

RSS rows have reduced metadata. Stored `source` provenance distinguishes `json` from `rss`.

## Watcher Guidance

For overnight local monitoring, prefer a macOS LaunchAgent that runs one bounded `rdt sync` per interval and exits cleanly. This is easier to inspect and recover than a long-running shell process.

Keep watcher logs under:

```text
~/Library/Application Support/rdt/watchers/
```

Recommended conservative profile:

```sh
target/debug/rdt sync --json --sub adhd --pages 1 --budget 100 --refresh
```

Use `launchctl print`, `tail`, and `rdt db query` to inspect status and growth. Stop launchd jobs with `launchctl bootout` rather than killing child sync processes when possible.

## Completion Checklist

- Preserve the read-only Reddit boundary.
- Update README, `AGENTS.md`, decisions, or data manifests when the project contract changes.
- Run validation proportional to the change.
- Use a read-only review subagent for meaningful implementation or packaging milestones when available.
- Commit with the ticket ID as the first commit-body line.
