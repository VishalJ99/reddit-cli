# PER-294 source provenance semantics

## Context

PER-293 added RSS degraded sync into the same local SQLite tables used by JSON sync. RSS rows can be missing parent IDs, scores, ratios, and comment counts, so downstream workflows need to know whether missing metadata is an RSS limitation or a true unknown.

## Decision

Add a `source` column to both stored posts and comments:

- existing databases migrate existing rows to `source = 'json'`,
- new JSON rows store `json`,
- new RSS rows store `rss`,
- a later JSON capture upgrades an RSS row to `json`,
- a later RSS capture does not downgrade a row already marked `json` and does not erase JSON-only metadata when RSS reports null values.

## Consequences

The database records best-known stored fidelity, not merely the last transport that saw the item. This keeps degraded RSS captures auditable while preserving richer JSON state when both transports have seen the same Reddit item.
