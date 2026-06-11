# PER-289 digest output bounds

## Context

`DESIGN.md` defines `rdt digest` as a zero-SQL export for LLM workflows: recent local activity grouped by post with top new comments. A busy subreddit can produce more local comments than are useful in a terminal or direct LLM prompt, so the digest needs deterministic bounds.

## Decision

Bound digest output while preserving grouped activity:

- include at most 50 post groups per digest,
- include at most 10 recent/top comments per post group,
- include at most 25 orphan comments whose post is not present locally,
- sort post groups by latest local activity,
- sort comments by score descending, then recency.

The command remains local-DB only and read-only. `--json` emits the same bounded structure as Markdown.

## Consequences

The default digest is compact enough for CLI and LLM use. It is a summary/export surface rather than a full database dump; users who need exhaustive activity can use `rdt db query ... --json` directly.
