# PER-286 targeted capture workflow

## Context

The user clarified that most Reddit comment activity is shallow, while a few high-interest posts collect unusually deep branches. The tool needs cheap ongoing capture for broad subreddit activity, plus explicit overrides when an operator decides a subreddit or post deserves more attention.

## Decision

Keep ongoing watches subreddit-scoped and flat:

- `rdt sync` polls `/r/{sub}/new.json` and `/r/{sub}/comments.json`.
- Comment sync has no tree-depth cutoff because the subreddit comment stream includes comments from any depth.
- Sync cost is bounded by page caps and item budgets, not comment depth.
- `rdt watch add` and `rdt watch set` can persist per-watch page, budget, and refresh settings.
- One-shot CLI sync flags take highest precedence over watch settings and global config/defaults.
- Selected posts and arbitrary thread URLs use `rdt pull URL|ID|N --max-requests ...` for deep tree capture.

Arbitrary listing URL watches are out of scope for this slice because they need source identity and watermark storage beyond the current subreddit watch schema.

## Consequences

Routine watch runs stay cheap and polite. Busy or important subreddits can get durable higher caps, and a specific run can temporarily override those caps. The few posts with deep, valuable comment trees are handled by `rdt pull` without making every watch cycle crawl thread trees.
