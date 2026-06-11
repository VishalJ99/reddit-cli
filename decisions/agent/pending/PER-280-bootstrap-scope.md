# PER-280 bootstrap scope

## Decision

The initial public repository is a compiling bootstrap that follows the `DESIGN.md` architecture and command surface, but it does not claim to complete every milestone in the design.

Implemented now:

- Rust package `reddit-cli` with binary `rdt`
- CLI command surface for the design
- transport/parser/rendering path for JSON and RSS reads
- XDG config/cache paths
- local SQLite schema, watches, saved links, digest/query/search scaffolding

Explicitly deferred:

- full `morechildren` and continue-thread expansion for `rdt thread --all`
- polling sync engine
- `rdt pull` DB backfill
- browser cookie extraction via `rookie`
- OAuth app-token transport

## Rationale

The user asked to create a new public git project from the design. A traceable, compiling bootstrap is a better first public commit than a large incomplete implementation that cannot be verified.

## Consequences

README and command errors must be honest about placeholder commands. Follow-up issues should split M1 transport verification, M2 read-path completion, and M3 watch/sync storage into separate work.
