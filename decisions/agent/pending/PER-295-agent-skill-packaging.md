# PER-295 agent skill packaging

## Context

The project now needs a reusable agent skill for `reddit-cli` that can be used from Claude Code, Codex, and OpenClaw. The tools share the `SKILL.md` concept but do not all use the same default search roots.

## Decision

Use `.agents/skills/reddit-cli/SKILL.md` as the canonical checked-in skill.

- Codex loads repo skills from `.agents/skills` when launched from the repo.
- OpenClaw loads workspace `.agents/skills`.
- Claude Code users copy the same skill folder into `.claude/skills` or `~/.claude/skills`.

## Consequences

The repo keeps one source of truth for the skill instead of maintaining separate Claude, Codex, and OpenClaw copies. Claude Code install instructions require a copy step, so users must recopy after skill edits if they installed it outside the repository.
