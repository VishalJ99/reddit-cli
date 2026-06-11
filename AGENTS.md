# AGENTS.md

## Project Purpose

`reddit-cli` is a public Rust project that builds `rdt`, a strictly read-only Reddit CLI following `DESIGN.md`.

The product boundary is important: the tool may search, browse, read, copy/open permalinks, and store local SQLite captures. It must not automate Reddit writes such as posts, comments, votes, moderation actions, messages, saved-items writes, or profile changes.

## Active Traceability

- Linear project: `reddit-cli`
- Bootstrap issue: `PER-280` (`Bootstrap public reddit-cli project from design`)
- Sync issue: `PER-282` (`Implement watch sync into SQLite`)
- Full-depth pull issue: `PER-283` (`Implement full-depth thread pull`)
- Refresh issue: `PER-285` (`Implement sync refresh hydration`)
- Watch override issue: `PER-286` (`Add persistent watch overrides and targeted capture workflow`)
- HTTP cache issue: `PER-287` (`Implement HTTP cache TTL`)
- GitHub target: `VishalJ99/reddit-cli`

Commit bodies should put the ticket ID on the first body line.

## Build And Test

Use:

```sh
cargo fmt --check
cargo check
cargo test
```

The transport stack uses `wreq` for browser-style HTTP emulation. Building `wreq` pulls in BoringSSL and requires `cmake` to be installed on the machine.

A local Reddit session cookie has been saved outside the repo at `~/Library/Application Support/rdt/config.toml` with `0600` file permissions inside a `0700` directory. Do not print the cookie. The code also supports the primary `directories` crate path under `~/Library/Application Support/com.dross.rdt/config.toml`.

## Design Source

`DESIGN.md` is the source of truth for the intended system. The bootstrap implementation is allowed to expose planned commands as explicit placeholders when a milestone is not implemented yet, but the README and command errors must say so plainly.

## Review Cadence

Use a code-review subagent at regular milestones:

- after the first compiling implementation,
- before the initial commit,
- before publishing/pushing,
- and after any large feature slice such as sync/store or full thread expansion.

Review subagents should be read-only unless explicitly assigned a disjoint implementation scope.

## Local State

Runtime state should stay in the XDG/project directories provided by the `directories` crate. Do not write caches, configs, cookies, or databases into the repository unless they are fixtures or documented test data.
