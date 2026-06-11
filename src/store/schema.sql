CREATE TABLE IF NOT EXISTS schema_meta (
    version INTEGER PRIMARY KEY
);

INSERT OR IGNORE INTO schema_meta (version) VALUES (1);

CREATE TABLE IF NOT EXISTS watches (
    subreddit TEXT PRIMARY KEY,
    active INTEGER NOT NULL DEFAULT 1,
    added_utc INTEGER NOT NULL,
    newest_post_fullname TEXT,
    newest_comment_fullname TEXT,
    last_synced_utc INTEGER
);

CREATE TABLE IF NOT EXISTS posts (
    id TEXT PRIMARY KEY,
    subreddit TEXT,
    title TEXT,
    author TEXT,
    selftext TEXT,
    url TEXT,
    permalink TEXT,
    flair TEXT,
    is_self INTEGER,
    over_18 INTEGER,
    score INTEGER,
    upvote_ratio REAL,
    num_comments INTEGER,
    created_utc INTEGER,
    edited_utc INTEGER,
    first_seen_utc INTEGER,
    last_updated_utc INTEGER,
    removed INTEGER DEFAULT 0,
    raw TEXT
);

CREATE TABLE IF NOT EXISTS comments (
    id TEXT PRIMARY KEY,
    post_id TEXT,
    parent_id TEXT,
    subreddit TEXT,
    author TEXT,
    body TEXT,
    score INTEGER,
    created_utc INTEGER,
    edited_utc INTEGER,
    permalink TEXT,
    first_seen_utc INTEGER,
    last_updated_utc INTEGER,
    removed INTEGER DEFAULT 0,
    raw TEXT
);

CREATE TABLE IF NOT EXISTS sync_log (
    id INTEGER PRIMARY KEY,
    subreddit TEXT,
    kind TEXT,
    started_utc INTEGER,
    finished_utc INTEGER,
    new_items INTEGER,
    updated_items INTEGER,
    http_requests INTEGER,
    status TEXT,
    error TEXT
);

CREATE TABLE IF NOT EXISTS saved (
    id INTEGER PRIMARY KEY,
    permalink TEXT NOT NULL,
    title TEXT,
    note TEXT,
    saved_utc INTEGER NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS posts_fts USING fts5(
    id UNINDEXED,
    title,
    selftext
);

CREATE VIRTUAL TABLE IF NOT EXISTS comments_fts USING fts5(
    id UNINDEXED,
    body
);

CREATE INDEX IF NOT EXISTS posts_subreddit_created_idx ON posts(subreddit, created_utc DESC);
CREATE INDEX IF NOT EXISTS comments_post_idx ON comments(post_id);
CREATE INDEX IF NOT EXISTS comments_parent_idx ON comments(parent_id);
CREATE INDEX IF NOT EXISTS comments_subreddit_created_idx ON comments(subreddit, created_utc DESC);
