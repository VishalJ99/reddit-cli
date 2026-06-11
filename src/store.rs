use crate::{
    config::Paths,
    model::{DbRow, Digest, DigestComment, DigestPost, ItemKind, RedditItem, ThreadView},
};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params, types::ValueRef};
use serde_json::{Map, Value, json};
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

const SCHEMA: &str = include_str!("store/schema.sql");
const SCHEMA_VERSION: i64 = 2;
const DIGEST_POST_LIMIT: i64 = 50;
const DIGEST_COMMENTS_PER_POST_LIMIT: i64 = 10;
const DIGEST_ORPHAN_COMMENT_LIMIT: i64 = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Posts,
    Comments,
    Backfill,
    Refresh,
}

impl StreamKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Posts => "posts",
            Self::Comments => "comments",
            Self::Backfill => "backfill",
            Self::Refresh => "refresh",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SyncStreamReport {
    pub subreddit: String,
    pub kind: StreamKind,
    pub new_items: usize,
    pub updated_items: usize,
    pub http_requests: usize,
    pub status: String,
    pub remaining_items: Option<usize>,
    pub notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchTarget {
    pub subreddit: String,
    pub page_cap: Option<u32>,
    pub budget: Option<u32>,
    pub refresh: Option<bool>,
}

impl WatchTarget {
    fn new(subreddit: impl AsRef<str>) -> Self {
        Self {
            subreddit: clean_subreddit(subreddit.as_ref()),
            page_cap: None,
            budget: None,
            refresh: None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct WatchOverrides {
    pub page_cap: Option<u32>,
    pub budget: Option<u32>,
    pub refresh: Option<bool>,
}

impl WatchOverrides {
    pub fn new(page_cap: Option<u32>, budget: Option<u32>, refresh: Option<bool>) -> Self {
        Self {
            page_cap,
            budget,
            refresh,
        }
    }

    pub fn is_empty(self) -> bool {
        self.page_cap.is_none() && self.budget.is_none() && self.refresh.is_none()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct UpsertStats {
    pub new_items: usize,
    pub updated_items: usize,
}

pub fn watch_add(paths: &Paths, subreddits: &[String], overrides: WatchOverrides) -> Result<()> {
    let conn = open(paths)?;
    let now = now_utc();
    for subreddit in subreddits {
        let subreddit = validate_subreddit(subreddit)?;
        conn.execute(
            "INSERT INTO watches (subreddit, active, added_utc, page_cap, budget, refresh)
             VALUES (?1, 1, ?2, ?3, ?4, ?5)
             ON CONFLICT(subreddit) DO UPDATE SET
              active = 1,
              page_cap = COALESCE(excluded.page_cap, watches.page_cap),
              budget = COALESCE(excluded.budget, watches.budget),
              refresh = COALESCE(excluded.refresh, watches.refresh)",
            params![
                subreddit,
                now,
                opt_u32_i64(overrides.page_cap),
                opt_u32_i64(overrides.budget),
                overrides.refresh.map(i64::from),
            ],
        )?;
    }
    Ok(())
}

pub fn watch_set(paths: &Paths, subreddit: &str, overrides: WatchOverrides) -> Result<()> {
    if overrides.is_empty() {
        anyhow::bail!(
            "watch set needs at least one of --pages, --budget, --refresh, or --no-refresh"
        );
    }

    let conn = open(paths)?;
    let now = now_utc();
    let subreddit = validate_subreddit(subreddit)?;
    conn.execute(
        "INSERT INTO watches (subreddit, active, added_utc, page_cap, budget, refresh)
         VALUES (?1, 1, ?2, ?3, ?4, ?5)
         ON CONFLICT(subreddit) DO UPDATE SET
          active = 1,
          page_cap = COALESCE(excluded.page_cap, watches.page_cap),
          budget = COALESCE(excluded.budget, watches.budget),
          refresh = COALESCE(excluded.refresh, watches.refresh)",
        params![
            subreddit,
            now,
            opt_u32_i64(overrides.page_cap),
            opt_u32_i64(overrides.budget),
            overrides.refresh.map(i64::from),
        ],
    )?;
    Ok(())
}

pub fn watch_remove(paths: &Paths, subreddit: &str) -> Result<()> {
    let conn = open(paths)?;
    conn.execute(
        "UPDATE watches SET active = 0 WHERE subreddit = ?1",
        params![clean_subreddit(subreddit)],
    )?;
    Ok(())
}

pub fn watch_list(paths: &Paths) -> Result<Vec<DbRow>> {
    query(
        paths,
        "SELECT subreddit, active, added_utc, page_cap, budget, refresh, last_synced_utc FROM watches ORDER BY subreddit",
    )
}

pub fn sync_targets(paths: &Paths, subreddits: &[String]) -> Result<Vec<WatchTarget>> {
    if subreddits.is_empty() {
        active_watches(paths)
    } else {
        explicit_watch_targets(paths, subreddits)
    }
}

fn active_watches(paths: &Paths) -> Result<Vec<WatchTarget>> {
    let conn = open(paths)?;
    let mut stmt = conn.prepare(
        "SELECT subreddit, page_cap, budget, refresh
         FROM watches
         WHERE active = 1
         ORDER BY subreddit",
    )?;
    let rows = stmt.query_map([], watch_target_from_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn explicit_watch_targets(paths: &Paths, subreddits: &[String]) -> Result<Vec<WatchTarget>> {
    let conn = open(paths)?;
    let mut stmt = conn.prepare(
        "SELECT subreddit, page_cap, budget, refresh
         FROM watches
         WHERE subreddit = ?1
           AND active = 1
         LIMIT 1",
    )?;
    let mut targets = Vec::new();
    for subreddit in subreddits {
        let normalized = validate_subreddit(subreddit)?;
        let target = stmt
            .query_row(params![normalized], watch_target_from_row)
            .optional()?
            .unwrap_or_else(|| WatchTarget::new(&normalized));
        targets.push(target);
    }
    Ok(targets)
}

pub fn save_link(paths: &Paths, permalink: &str, note: Option<&str>) -> Result<()> {
    let conn = open(paths)?;
    conn.execute(
        "INSERT INTO saved (permalink, note, saved_utc) VALUES (?1, ?2, ?3)",
        params![permalink, note, now_utc()],
    )?;
    Ok(())
}

pub fn saved(paths: &Paths) -> Result<Vec<DbRow>> {
    query(
        paths,
        "SELECT id, permalink, title, note, saved_utc FROM saved ORDER BY saved_utc DESC",
    )
}

pub fn digest(paths: &Paths, since: Option<&str>, sub: Option<&str>) -> Result<Digest> {
    let cutoff = since.map(parse_since).transpose()?.unwrap_or(0);
    let subreddit = sub.map(validate_subreddit).transpose()?;
    if !paths.db_file.exists() {
        return Ok(Digest {
            generated_utc: now_utc(),
            since_utc: cutoff,
            subreddit,
            posts: Vec::new(),
            orphan_comments: Vec::new(),
        });
    }

    let conn = open_readonly(paths)?;
    let mut posts = digest_posts(&conn, cutoff, subreddit.as_deref())?;
    for post in &mut posts {
        post.comments = digest_comments_for_post(&conn, &post.id, cutoff)?;
    }
    let orphan_comments = digest_orphan_comments(&conn, cutoff, subreddit.as_deref())?;

    Ok(Digest {
        generated_utc: now_utc(),
        since_utc: cutoff,
        subreddit,
        posts,
        orphan_comments,
    })
}

fn digest_posts(conn: &Connection, cutoff: i64, sub: Option<&str>) -> Result<Vec<DigestPost>> {
    let mut sql = "
        SELECT
            p.id,
            p.subreddit,
            p.title,
            p.author,
            p.permalink,
            p.score,
            p.num_comments,
            p.created_utc,
            COALESCE(MAX(COALESCE(c.created_utc, c.first_seen_utc, c.last_updated_utc, 0)), 0) AS latest_comment_utc,
            CASE
              WHEN COALESCE(MAX(COALESCE(c.created_utc, c.first_seen_utc, c.last_updated_utc, 0)), 0)
                   > COALESCE(p.created_utc, p.first_seen_utc, p.last_updated_utc, 0)
              THEN COALESCE(MAX(COALESCE(c.created_utc, c.first_seen_utc, c.last_updated_utc, 0)), 0)
              ELSE COALESCE(p.created_utc, p.first_seen_utc, p.last_updated_utc, 0)
            END AS activity_utc
        FROM posts p
        LEFT JOIN comments c
          ON c.post_id = p.id
         AND COALESCE(c.created_utc, c.first_seen_utc, c.last_updated_utc, 0) >= ?1
        WHERE (
            COALESCE(p.created_utc, p.first_seen_utc, p.last_updated_utc, 0) >= ?1
            OR c.id IS NOT NULL
        )"
    .to_owned();
    if sub.is_some() {
        sql.push_str(" AND p.subreddit = ?2");
    }
    let limit_param = if sub.is_some() { "?3" } else { "?2" };
    sql.push_str(&format!(
        "
        GROUP BY p.id
        ORDER BY activity_utc DESC,
                 COALESCE(p.created_utc, p.first_seen_utc, p.last_updated_utc, 0) DESC,
                 p.id ASC
        LIMIT {limit_param}",
    ));

    let mut stmt = conn.prepare(&sql)?;
    let map_post = |row: &rusqlite::Row<'_>| {
        Ok(DigestPost {
            id: row.get(0)?,
            subreddit: row.get(1)?,
            title: row.get(2)?,
            author: row.get(3)?,
            permalink: row.get(4)?,
            score: row.get(5)?,
            num_comments: row.get(6)?,
            created_utc: row.get(7)?,
            activity_utc: row.get::<_, Option<i64>>(9)?.unwrap_or(0),
            comments: Vec::new(),
        })
    };
    let rows = if let Some(subreddit) = sub {
        stmt.query_map(params![cutoff, subreddit, DIGEST_POST_LIMIT], map_post)?
    } else {
        stmt.query_map(params![cutoff, DIGEST_POST_LIMIT], map_post)?
    };
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn digest_comments_for_post(
    conn: &Connection,
    post_id: &str,
    cutoff: i64,
) -> Result<Vec<DigestComment>> {
    let mut stmt = conn.prepare(
        "SELECT id, post_id, parent_id, subreddit, author, body, score, created_utc, permalink
         FROM comments
         WHERE post_id = ?1
           AND COALESCE(created_utc, first_seen_utc, last_updated_utc, 0) >= ?2
         ORDER BY COALESCE(score, 0) DESC,
                  COALESCE(created_utc, first_seen_utc, last_updated_utc, 0) DESC,
                  id ASC
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        params![post_id, cutoff, DIGEST_COMMENTS_PER_POST_LIMIT],
        digest_comment_from_row,
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn digest_orphan_comments(
    conn: &Connection,
    cutoff: i64,
    sub: Option<&str>,
) -> Result<Vec<DigestComment>> {
    let mut sql = "
        SELECT c.id, c.post_id, c.parent_id, c.subreddit, c.author, c.body, c.score, c.created_utc, c.permalink
        FROM comments c
        LEFT JOIN posts p ON p.id = c.post_id
        WHERE p.id IS NULL
          AND COALESCE(c.created_utc, c.first_seen_utc, c.last_updated_utc, 0) >= ?1"
        .to_owned();
    if sub.is_some() {
        sql.push_str(" AND c.subreddit = ?2");
    }
    let limit_param = if sub.is_some() { "?3" } else { "?2" };
    sql.push_str(&format!(
        "
        ORDER BY COALESCE(c.score, 0) DESC,
                 COALESCE(c.created_utc, c.first_seen_utc, c.last_updated_utc, 0) DESC,
                 c.id ASC
        LIMIT {limit_param}",
    ));

    let mut stmt = conn.prepare(&sql)?;
    let rows = if let Some(subreddit) = sub {
        stmt.query_map(
            params![cutoff, subreddit, DIGEST_ORPHAN_COMMENT_LIMIT],
            digest_comment_from_row,
        )?
    } else {
        stmt.query_map(
            params![cutoff, DIGEST_ORPHAN_COMMENT_LIMIT],
            digest_comment_from_row,
        )?
    };
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn digest_comment_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DigestComment> {
    Ok(DigestComment {
        id: row.get(0)?,
        post_id: row.get(1)?,
        parent_id: row.get(2)?,
        subreddit: row.get(3)?,
        author: row.get(4)?,
        body: row.get(5)?,
        score: row.get(6)?,
        created_utc: row.get(7)?,
        permalink: row.get(8)?,
    })
}

pub fn query(paths: &Paths, sql: &str) -> Result<Vec<DbRow>> {
    let conn = open(paths)?;
    conn.execute_batch("PRAGMA query_only = ON;")?;
    let mut stmt = conn.prepare(sql)?;
    ensure_read_statement(&stmt)?;
    let names = column_names(&stmt);
    let rows = stmt.query([])?;
    rows_to_json(rows, &names)
}

pub fn search(paths: &Paths, text: &str) -> Result<Vec<DbRow>> {
    let conn = open(paths)?;
    let mut out = Vec::new();

    {
        let mut stmt = conn.prepare(
            "SELECT 'post' AS kind, p.subreddit, p.title, p.permalink, p.score, p.created_utc
             FROM posts_fts f JOIN posts p ON p.id = f.id
             WHERE posts_fts MATCH ?1
             ORDER BY rank LIMIT 50",
        )?;
        let names = column_names(&stmt);
        out.extend(rows_to_json(stmt.query(params![text])?, &names)?);
    }

    {
        let mut stmt = conn.prepare(
            "SELECT 'comment' AS kind, c.subreddit, c.author, c.body AS title, c.permalink, c.score, c.created_utc
             FROM comments_fts f JOIN comments c ON c.id = f.id
             WHERE comments_fts MATCH ?1
             ORDER BY rank LIMIT 50",
        )?;
        let names = column_names(&stmt);
        out.extend(rows_to_json(stmt.query(params![text])?, &names)?);
    }

    Ok(out)
}

pub fn known_count(paths: &Paths, kind: StreamKind, items: &[RedditItem]) -> Result<usize> {
    let conn = open(paths)?;
    let table = match kind {
        StreamKind::Posts => "posts",
        StreamKind::Comments => "comments",
        StreamKind::Backfill => anyhow::bail!("backfill is not a concrete item table"),
        StreamKind::Refresh => anyhow::bail!("refresh is not a concrete item table"),
    };
    let sql = format!("SELECT 1 FROM {table} WHERE id = ?1 LIMIT 1");
    let mut stmt = conn.prepare(&sql)?;
    let mut count = 0;
    for item in items {
        if stmt
            .query_row(params![item.id], |_| Ok(()))
            .optional()?
            .is_some()
        {
            count += 1;
        }
    }
    Ok(count)
}

pub fn upsert_items(
    paths: &Paths,
    subreddit: &str,
    kind: StreamKind,
    items: &[RedditItem],
) -> Result<UpsertStats> {
    let mut conn = open(paths)?;
    let tx = conn.transaction()?;
    let mut stats = UpsertStats::default();

    for item in items {
        match kind {
            StreamKind::Posts => {
                if item.kind == ItemKind::Post {
                    upsert_post(&tx, subreddit, item, &mut stats)?;
                }
            }
            StreamKind::Comments => {
                if item.kind == ItemKind::Comment {
                    upsert_comment(&tx, subreddit, item, &mut stats)?;
                }
            }
            StreamKind::Backfill => anyhow::bail!("backfill upsert requires a thread view"),
            StreamKind::Refresh => anyhow::bail!("refresh upsert requires concrete post items"),
        }
    }

    tx.commit()?;
    Ok(stats)
}

pub fn upsert_thread(paths: &Paths, thread: &ThreadView) -> Result<SyncStreamReport> {
    let started = now_utc();
    let (subreddit, stats) = upsert_thread_items(paths, thread)?;

    let report = SyncStreamReport {
        subreddit,
        kind: StreamKind::Backfill,
        new_items: stats.new_items,
        updated_items: stats.updated_items,
        http_requests: thread.http_requests,
        status: if thread.truncated || thread.more_stubs > 0 {
            "gap"
        } else {
            "ok"
        }
        .to_owned(),
        remaining_items: (thread.more_stubs > 0).then_some(thread.more_stubs),
        notice: thread.notice.clone(),
    };
    append_sync_log(paths, &report, started, now_utc(), None)?;
    Ok(report)
}

pub fn upsert_thread_items(paths: &Paths, thread: &ThreadView) -> Result<(String, UpsertStats)> {
    let mut conn = open(paths)?;
    let tx = conn.transaction()?;
    let mut stats = UpsertStats::default();
    let subreddit = thread
        .post
        .as_ref()
        .and_then(|post| post.subreddit.as_deref())
        .or_else(|| {
            thread
                .comments
                .iter()
                .find_map(|comment| comment.subreddit.as_deref())
        })
        .map(clean_subreddit)
        .context("thread did not include a subreddit")?;

    if let Some(post) = &thread.post {
        upsert_post(&tx, &subreddit, post, &mut stats)?;
    }
    for comment in &thread.comments {
        if comment.kind == ItemKind::Comment {
            upsert_comment(&tx, &subreddit, comment, &mut stats)?;
        }
    }

    tx.commit()?;
    Ok((subreddit, stats))
}

pub fn update_watch_watermark(
    paths: &Paths,
    subreddit: &str,
    kind: StreamKind,
    newest_fullname: Option<&str>,
) -> Result<()> {
    let conn = open(paths)?;
    let now = now_utc();
    let subreddit = clean_subreddit(subreddit);
    conn.execute(
        "INSERT INTO watches (subreddit, active, added_utc)
         VALUES (?1, 1, ?2)
         ON CONFLICT(subreddit) DO NOTHING",
        params![subreddit, now],
    )?;
    match kind {
        StreamKind::Posts => conn.execute(
            "UPDATE watches
             SET newest_post_fullname = COALESCE(?2, newest_post_fullname),
                 last_synced_utc = ?3
             WHERE subreddit = ?1",
            params![subreddit, newest_fullname, now],
        )?,
        StreamKind::Comments => conn.execute(
            "UPDATE watches
             SET newest_comment_fullname = COALESCE(?2, newest_comment_fullname),
                 last_synced_utc = ?3
             WHERE subreddit = ?1",
            params![subreddit, newest_fullname, now],
        )?,
        StreamKind::Backfill => anyhow::bail!("backfill does not use watch watermarks"),
        StreamKind::Refresh => anyhow::bail!("refresh does not use watch watermarks"),
    };
    Ok(())
}

pub fn recent_post_fullnames(paths: &Paths, subreddit: &str, limit: usize) -> Result<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let conn = open(paths)?;
    let mut stmt = conn.prepare(
        "SELECT 't3_' || id
         FROM posts
         WHERE subreddit = ?1
         ORDER BY COALESCE(created_utc, first_seen_utc, last_updated_utc, 0) DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![clean_subreddit(subreddit), limit as i64], |row| {
        row.get::<_, String>(0)
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn post_count(paths: &Paths, subreddit: &str) -> Result<usize> {
    let conn = open(paths)?;
    let count = conn.query_row(
        "SELECT COUNT(*) FROM posts WHERE subreddit = ?1",
        params![clean_subreddit(subreddit)],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(count.max(0) as usize)
}

pub fn append_sync_log(
    paths: &Paths,
    report: &SyncStreamReport,
    started_utc: i64,
    finished_utc: i64,
    error: Option<&str>,
) -> Result<()> {
    let conn = open(paths)?;
    conn.execute(
        "INSERT INTO sync_log
         (subreddit, kind, started_utc, finished_utc, new_items, updated_items, http_requests, status, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            clean_subreddit(&report.subreddit),
            report.kind.as_str(),
            started_utc,
            finished_utc,
            report.new_items as i64,
            report.updated_items as i64,
            report.http_requests as i64,
            report.status,
            error,
        ],
    )?;
    Ok(())
}

pub fn utc_now() -> i64 {
    now_utc()
}

pub fn normalize_subreddit(input: &str) -> String {
    clean_subreddit(input)
}

fn open(paths: &Paths) -> Result<Connection> {
    fs::create_dir_all(&paths.data_dir)?;
    let conn = Connection::open(&paths.db_file)
        .with_context(|| format!("opening {}", paths.db_file.display()))?;
    conn.execute_batch(SCHEMA)?;
    ensure_watch_override_columns(&conn)?;
    set_schema_version(&conn)?;
    conn.execute_batch(
        "UPDATE watches SET subreddit = lower(subreddit) WHERE subreddit != lower(subreddit);
         UPDATE posts SET subreddit = lower(subreddit) WHERE subreddit != lower(subreddit);
         UPDATE comments SET subreddit = lower(subreddit) WHERE subreddit != lower(subreddit);",
    )?;
    Ok(conn)
}

fn open_readonly(paths: &Paths) -> Result<Connection> {
    let conn = Connection::open_with_flags(&paths.db_file, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} read-only", paths.db_file.display()))?;
    conn.execute_batch("PRAGMA query_only = ON;")?;
    Ok(conn)
}

fn set_schema_version(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM schema_meta", [])?;
    conn.execute(
        "INSERT INTO schema_meta (version) VALUES (?1)",
        params![SCHEMA_VERSION],
    )?;
    Ok(())
}

fn ensure_watch_override_columns(conn: &Connection) -> Result<()> {
    let columns = watch_columns(conn)?;
    for (name, definition) in [
        ("page_cap", "page_cap INTEGER"),
        ("budget", "budget INTEGER"),
        ("refresh", "refresh INTEGER"),
    ] {
        if !columns.iter().any(|column| column == name) {
            conn.execute(&format!("ALTER TABLE watches ADD COLUMN {definition}"), [])?;
        }
    }
    Ok(())
}

fn watch_columns(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("PRAGMA table_info(watches)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn watch_target_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WatchTarget> {
    Ok(WatchTarget {
        subreddit: row.get(0)?,
        page_cap: opt_i64_u32(row.get(1)?),
        budget: opt_i64_u32(row.get(2)?),
        refresh: row.get::<_, Option<i64>>(3)?.map(|value| value != 0),
    })
}

fn upsert_post(
    tx: &Transaction<'_>,
    subreddit: &str,
    item: &RedditItem,
    stats: &mut UpsertStats,
) -> Result<()> {
    let existed = exists(tx, "posts", &item.id)?;
    let subreddit = clean_subreddit(subreddit);
    let now = now_utc();
    tx.execute(
        "INSERT INTO posts
         (id, subreddit, title, author, selftext, url, permalink, flair, is_self, over_18,
          score, upvote_ratio, num_comments, created_utc, edited_utc, first_seen_utc,
          last_updated_utc, removed, raw)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, NULL)
         ON CONFLICT(id) DO UPDATE SET
          subreddit = excluded.subreddit,
          title = CASE
            WHEN excluded.removed = 1 AND posts.title IS NOT NULL AND posts.title NOT IN ('[deleted]', '[removed]')
            THEN posts.title
            ELSE excluded.title
          END,
          author = CASE
            WHEN excluded.removed = 1 AND posts.author IS NOT NULL AND posts.author != '[deleted]'
            THEN posts.author
            ELSE excluded.author
          END,
          selftext = CASE
            WHEN excluded.removed = 1 AND posts.selftext IS NOT NULL AND posts.selftext NOT IN ('[deleted]', '[removed]')
            THEN posts.selftext
            ELSE excluded.selftext
          END,
          url = excluded.url,
          permalink = excluded.permalink,
          flair = excluded.flair,
          is_self = excluded.is_self,
          over_18 = excluded.over_18,
          score = excluded.score,
          upvote_ratio = excluded.upvote_ratio,
          num_comments = excluded.num_comments,
          created_utc = excluded.created_utc,
          edited_utc = excluded.edited_utc,
          last_updated_utc = excluded.last_updated_utc,
          removed = excluded.removed",
        params![
            item.id,
            subreddit,
            item.title,
            item.author,
            item.body,
            item.url,
            item.canonical_permalink(),
            item.flair,
            opt_bool_i64(item.is_self),
            opt_bool_i64(item.over_18),
            item.score,
            item.upvote_ratio,
            item.num_comments,
            opt_epoch(item.created_utc),
            opt_epoch(item.edited_utc),
            now,
            now,
            removed_flag(item.body.as_deref()),
        ],
    )?;
    refresh_post_fts(tx, &item.id)?;
    add_stats(stats, existed);
    Ok(())
}

fn upsert_comment(
    tx: &Transaction<'_>,
    subreddit: &str,
    item: &RedditItem,
    stats: &mut UpsertStats,
) -> Result<()> {
    let existed = exists(tx, "comments", &item.id)?;
    let subreddit = clean_subreddit(subreddit);
    let now = now_utc();
    tx.execute(
        "INSERT INTO comments
         (id, post_id, parent_id, subreddit, author, body, score, created_utc, edited_utc,
          permalink, first_seen_utc, last_updated_utc, removed, raw)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL)
         ON CONFLICT(id) DO UPDATE SET
          post_id = excluded.post_id,
          parent_id = excluded.parent_id,
          subreddit = excluded.subreddit,
          author = CASE
            WHEN excluded.removed = 1 AND comments.author IS NOT NULL AND comments.author != '[deleted]'
            THEN comments.author
            ELSE excluded.author
          END,
          body = CASE
            WHEN excluded.removed = 1 AND comments.body IS NOT NULL AND comments.body NOT IN ('[deleted]', '[removed]')
            THEN comments.body
            ELSE excluded.body
          END,
          score = excluded.score,
          created_utc = excluded.created_utc,
          edited_utc = excluded.edited_utc,
          permalink = excluded.permalink,
          last_updated_utc = excluded.last_updated_utc,
          removed = excluded.removed",
        params![
            item.id,
            item.post_id,
            item.parent_id,
            subreddit,
            item.author,
            item.body,
            item.score,
            opt_epoch(item.created_utc),
            opt_epoch(item.edited_utc),
            item.canonical_permalink(),
            now,
            now,
            removed_flag(item.body.as_deref()),
        ],
    )?;
    refresh_comment_fts(tx, &item.id)?;
    add_stats(stats, existed);
    Ok(())
}

fn exists(tx: &Transaction<'_>, table: &str, id: &str) -> Result<bool> {
    let sql = format!("SELECT 1 FROM {table} WHERE id = ?1 LIMIT 1");
    Ok(tx
        .query_row(&sql, params![id], |_| Ok(()))
        .optional()?
        .is_some())
}

fn refresh_post_fts(tx: &Transaction<'_>, id: &str) -> Result<()> {
    tx.execute("DELETE FROM posts_fts WHERE id = ?1", params![id])?;
    tx.execute(
        "INSERT INTO posts_fts (id, title, selftext)
         SELECT id, title, selftext FROM posts WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

fn refresh_comment_fts(tx: &Transaction<'_>, id: &str) -> Result<()> {
    tx.execute("DELETE FROM comments_fts WHERE id = ?1", params![id])?;
    tx.execute(
        "INSERT INTO comments_fts (id, body)
         SELECT id, body FROM comments WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

fn add_stats(stats: &mut UpsertStats, existed: bool) {
    if existed {
        stats.updated_items += 1;
    } else {
        stats.new_items += 1;
    }
}

fn opt_bool_i64(value: Option<bool>) -> Option<i64> {
    value.map(i64::from)
}

fn opt_u32_i64(value: Option<u32>) -> Option<i64> {
    value.map(i64::from)
}

fn opt_i64_u32(value: Option<i64>) -> Option<u32> {
    value.and_then(|value| u32::try_from(value).ok())
}

fn opt_epoch(value: Option<f64>) -> Option<i64> {
    value.map(|value| value as i64)
}

fn removed_flag(body: Option<&str>) -> i64 {
    match body {
        Some("[deleted]" | "[removed]") => 1,
        _ => 0,
    }
}

fn column_names(stmt: &rusqlite::Statement<'_>) -> Vec<String> {
    stmt.column_names()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect()
}

fn rows_to_json(mut rows: rusqlite::Rows<'_>, names: &[String]) -> Result<Vec<DbRow>> {
    let mut out = Vec::new();

    while let Some(row) = rows.next()? {
        let mut map = Map::new();
        for (index, name) in names.iter().enumerate() {
            map.insert(name.clone(), value_ref_to_json(row.get_ref(index)?));
        }
        out.push(map);
    }

    Ok(out)
}

fn value_ref_to_json(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => json!(value),
        ValueRef::Real(value) => json!(value),
        ValueRef::Text(value) => json!(String::from_utf8_lossy(value).to_string()),
        ValueRef::Blob(value) => json!(format!("<blob {} bytes>", value.len())),
    }
}

fn now_utc() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn parse_since(value: &str) -> Result<i64> {
    let now = now_utc();
    let trimmed = value.trim();
    if let Some(hours) = trimmed.strip_suffix('h') {
        let hours = parse_positive_duration(hours, value)?;
        let seconds = hours
            .checked_mul(60 * 60)
            .context("relative --since value is too large")?;
        return now
            .checked_sub(seconds)
            .context("relative --since value is before supported epoch");
    }
    if let Some(days) = trimmed.strip_suffix('d') {
        let days = parse_positive_duration(days, value)?;
        let seconds = days
            .checked_mul(24 * 60 * 60)
            .context("relative --since value is too large")?;
        return now
            .checked_sub(seconds)
            .context("relative --since value is before supported epoch");
    }
    let epoch: i64 = trimmed
        .parse()
        .with_context(|| format!("invalid --since value: {value}"))?;
    if epoch < 0 {
        anyhow::bail!("--since epoch must be non-negative: {value}");
    }
    Ok(epoch)
}

fn parse_positive_duration(raw: &str, original: &str) -> Result<i64> {
    let value: i64 = raw
        .parse()
        .with_context(|| format!("invalid --since value: {original}"))?;
    if value <= 0 {
        anyhow::bail!("relative --since value must be positive: {original}");
    }
    Ok(value)
}

fn clean_subreddit(input: &str) -> String {
    input
        .trim()
        .trim_start_matches("r/")
        .trim_start_matches("/r/")
        .to_ascii_lowercase()
}

fn validate_subreddit(input: &str) -> Result<String> {
    let subreddit = clean_subreddit(input);
    if (2..=21).contains(&subreddit.len())
        && subreddit
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        Ok(subreddit)
    } else {
        anyhow::bail!("invalid subreddit name: {input}")
    }
}

fn ensure_read_statement(stmt: &rusqlite::Statement<'_>) -> Result<()> {
    if stmt.readonly() {
        Ok(())
    } else {
        anyhow::bail!("rdt db query is read-only")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ItemSource;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn initializes_and_records_watches() {
        let paths = temp_paths();
        watch_add(&paths, &[String::from("r/rust")], WatchOverrides::default()).unwrap();
        let rows = watch_list(&paths).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("subreddit").unwrap(), "rust");
        assert_eq!(
            query(&paths, "SELECT version FROM schema_meta").unwrap()[0].get("version"),
            Some(&json!(2))
        );
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn rejects_invalid_watch_subreddits() {
        let paths = temp_paths();
        let error = watch_add(
            &paths,
            &[String::from("rust/new")],
            WatchOverrides::default(),
        )
        .expect_err("path-like subreddit should be rejected")
        .to_string();
        assert!(error.contains("invalid subreddit name"));

        let error = watch_set(
            &paths,
            "https://www.reddit.com/r/rust",
            WatchOverrides::new(Some(1), None, None),
        )
        .expect_err("URL-shaped subreddit should be rejected")
        .to_string();
        assert!(error.contains("invalid subreddit name"));

        let error = sync_targets(&paths, &[String::from("rust+programming")])
            .expect_err("multireddit source is not a subreddit watch")
            .to_string();
        assert!(error.contains("invalid subreddit name"));
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn records_watch_overrides_and_sync_targets() {
        let paths = temp_paths();
        watch_add(
            &paths,
            &[String::from("r/Rust")],
            WatchOverrides::new(Some(5), Some(500), Some(true)),
        )
        .unwrap();

        let rows = watch_list(&paths).unwrap();
        assert_eq!(rows[0].get("subreddit"), Some(&json!("rust")));
        assert_eq!(rows[0].get("page_cap"), Some(&json!(5)));
        assert_eq!(rows[0].get("budget"), Some(&json!(500)));
        assert_eq!(rows[0].get("refresh"), Some(&json!(1)));

        let targets = sync_targets(&paths, &[]).unwrap();
        assert_eq!(
            targets,
            vec![WatchTarget {
                subreddit: "rust".to_owned(),
                page_cap: Some(5),
                budget: Some(500),
                refresh: Some(true),
            }]
        );

        let explicit = sync_targets(&paths, &[String::from("adhd")]).unwrap();
        assert_eq!(explicit, vec![WatchTarget::new("adhd")]);
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn watch_set_updates_one_watch_override() {
        let paths = temp_paths();
        watch_set(
            &paths,
            "rust",
            WatchOverrides::new(Some(7), Some(700), Some(false)),
        )
        .unwrap();

        let target = sync_targets(&paths, &[]).unwrap().remove(0);
        assert_eq!(target.page_cap, Some(7));
        assert_eq!(target.budget, Some(700));
        assert_eq!(target.refresh, Some(false));
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn explicit_sync_ignores_inactive_watch_overrides() {
        let paths = temp_paths();
        watch_add(
            &paths,
            &[String::from("rust")],
            WatchOverrides::new(Some(9), Some(900), Some(true)),
        )
        .unwrap();
        watch_remove(&paths, "rust").unwrap();

        let targets = sync_targets(&paths, &[String::from("rust")]).unwrap();
        assert_eq!(targets, vec![WatchTarget::new("rust")]);
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn migrates_old_watch_tables_for_overrides() {
        let paths = temp_paths();
        fs::create_dir_all(&paths.data_dir).unwrap();
        let conn = Connection::open(&paths.db_file).unwrap();
        conn.execute_batch(
            "CREATE TABLE watches (
                subreddit TEXT PRIMARY KEY,
                active INTEGER NOT NULL DEFAULT 1,
                added_utc INTEGER NOT NULL,
                newest_post_fullname TEXT,
                newest_comment_fullname TEXT,
                last_synced_utc INTEGER
            );
            INSERT INTO watches (subreddit, active, added_utc) VALUES ('rust', 1, 10);",
        )
        .unwrap();
        drop(conn);

        watch_set(
            &paths,
            "rust",
            WatchOverrides::new(Some(3), Some(300), Some(true)),
        )
        .unwrap();

        let rows = query(&paths, "SELECT page_cap, budget, refresh FROM watches").unwrap();
        assert_eq!(rows[0].get("page_cap"), Some(&json!(3)));
        assert_eq!(rows[0].get("budget"), Some(&json!(300)));
        assert_eq!(rows[0].get("refresh"), Some(&json!(1)));
        assert_eq!(
            query(&paths, "SELECT version FROM schema_meta").unwrap()[0].get("version"),
            Some(&json!(2))
        );
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn db_query_rejects_writes() {
        let paths = temp_paths();
        watch_add(&paths, &[String::from("rust")], WatchOverrides::default()).unwrap();
        let error = query(
            &paths,
            "WITH doomed AS (SELECT 1) DELETE FROM watches WHERE subreddit = 'rust'",
        )
        .expect_err("mutating query should be rejected")
        .to_string();
        assert!(error.contains("read-only"));
        assert_eq!(watch_list(&paths).unwrap().len(), 1);
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn upserts_posts_and_comments() {
        let paths = temp_paths();
        let post = RedditItem {
            index: None,
            kind: ItemKind::Post,
            id: "abc".to_owned(),
            fullname: "t3_abc".to_owned(),
            parent_id: None,
            post_id: Some("abc".to_owned()),
            title: Some("Post".to_owned()),
            author: Some("alice".to_owned()),
            subreddit: Some("rust".to_owned()),
            body: Some("body".to_owned()),
            flair: None,
            is_self: Some(true),
            over_18: Some(false),
            score: Some(1),
            upvote_ratio: Some(0.9),
            num_comments: Some(1),
            created_utc: Some(10.0),
            edited_utc: None,
            permalink: Some("/r/rust/comments/abc/post/".to_owned()),
            url: None,
            depth: 0,
            source: ItemSource::Json,
        };
        let comment = RedditItem {
            index: None,
            kind: ItemKind::Comment,
            id: "def".to_owned(),
            fullname: "t1_def".to_owned(),
            parent_id: Some("t3_abc".to_owned()),
            post_id: Some("abc".to_owned()),
            title: None,
            author: Some("bob".to_owned()),
            subreddit: Some("rust".to_owned()),
            body: Some("reply".to_owned()),
            flair: None,
            is_self: None,
            over_18: None,
            score: Some(2),
            upvote_ratio: None,
            num_comments: None,
            created_utc: Some(11.0),
            edited_utc: None,
            permalink: Some("/r/rust/comments/abc/post/def/".to_owned()),
            url: None,
            depth: 0,
            source: ItemSource::Json,
        };

        let post_stats = upsert_items(&paths, "rust", StreamKind::Posts, &[post]).unwrap();
        let comment_stats = upsert_items(&paths, "rust", StreamKind::Comments, &[comment]).unwrap();

        assert_eq!(post_stats.new_items, 1);
        assert_eq!(comment_stats.new_items, 1);
        assert_eq!(
            query(&paths, "SELECT COUNT(*) AS n FROM posts").unwrap()[0].get("n"),
            Some(&json!(1))
        );
        assert_eq!(
            query(&paths, "SELECT parent_id FROM comments WHERE id = 'def'").unwrap()[0]
                .get("parent_id"),
            Some(&json!("t3_abc"))
        );
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn tombstones_preserve_captured_text_and_search_index() {
        let paths = temp_paths();
        let mut post = RedditItem {
            index: None,
            kind: ItemKind::Post,
            id: "abc".to_owned(),
            fullname: "t3_abc".to_owned(),
            parent_id: None,
            post_id: Some("abc".to_owned()),
            title: Some("Original post needle".to_owned()),
            author: Some("alice".to_owned()),
            subreddit: Some("rust".to_owned()),
            body: Some("original selftext needle".to_owned()),
            flair: None,
            is_self: Some(true),
            over_18: Some(false),
            score: Some(1),
            upvote_ratio: Some(0.9),
            num_comments: Some(1),
            created_utc: Some(10.0),
            edited_utc: None,
            permalink: Some("/r/rust/comments/abc/post/".to_owned()),
            url: None,
            depth: 0,
            source: ItemSource::Json,
        };
        let mut comment = RedditItem {
            index: None,
            kind: ItemKind::Comment,
            id: "def".to_owned(),
            fullname: "t1_def".to_owned(),
            parent_id: Some("t3_abc".to_owned()),
            post_id: Some("abc".to_owned()),
            title: None,
            author: Some("bob".to_owned()),
            subreddit: Some("rust".to_owned()),
            body: Some("original comment needle".to_owned()),
            flair: None,
            is_self: None,
            over_18: None,
            score: Some(2),
            upvote_ratio: None,
            num_comments: None,
            created_utc: Some(11.0),
            edited_utc: None,
            permalink: Some("/r/rust/comments/abc/post/def/".to_owned()),
            url: None,
            depth: 0,
            source: ItemSource::Json,
        };

        upsert_items(&paths, "rust", StreamKind::Posts, &[post.clone()]).unwrap();
        upsert_items(&paths, "rust", StreamKind::Comments, &[comment.clone()]).unwrap();

        post.title = Some("[deleted]".to_owned());
        post.author = Some("[deleted]".to_owned());
        post.body = Some("[removed]".to_owned());
        post.score = Some(9);
        comment.author = Some("[deleted]".to_owned());
        comment.body = Some("[removed]".to_owned());
        comment.score = Some(11);
        upsert_items(&paths, "rust", StreamKind::Posts, &[post]).unwrap();
        upsert_items(&paths, "rust", StreamKind::Comments, &[comment]).unwrap();

        let rows = query(
            &paths,
            "SELECT title, author, selftext, score, removed FROM posts",
        )
        .unwrap();
        assert_eq!(rows[0].get("title"), Some(&json!("Original post needle")));
        assert_eq!(rows[0].get("author"), Some(&json!("alice")));
        assert_eq!(
            rows[0].get("selftext"),
            Some(&json!("original selftext needle"))
        );
        assert_eq!(rows[0].get("score"), Some(&json!(9)));
        assert_eq!(rows[0].get("removed"), Some(&json!(1)));

        let rows = query(
            &paths,
            "SELECT author, body, score, removed FROM comments WHERE id = 'def'",
        )
        .unwrap();
        assert_eq!(rows[0].get("author"), Some(&json!("bob")));
        assert_eq!(rows[0].get("body"), Some(&json!("original comment needle")));
        assert_eq!(rows[0].get("score"), Some(&json!(11)));
        assert_eq!(rows[0].get("removed"), Some(&json!(1)));

        let hits = search(&paths, "needle").unwrap();
        assert_eq!(hits.len(), 2);
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn watermark_creates_normalized_watch_for_explicit_sync() {
        let paths = temp_paths();
        update_watch_watermark(&paths, "r/ADHD", StreamKind::Posts, Some("t3_abc")).unwrap();

        let rows = query(
            &paths,
            "SELECT subreddit, newest_post_fullname FROM watches",
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("subreddit"), Some(&json!("adhd")));
        assert_eq!(rows[0].get("newest_post_fullname"), Some(&json!("t3_abc")));
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn upserts_thread_and_logs_backfill() {
        let paths = temp_paths();
        let thread = ThreadView {
            post: Some(test_post("abc")),
            comments: vec![test_comment("def", "t3_abc")],
            more_stubs: 0,
            degraded: false,
            truncated: false,
            http_requests: 4,
            notice: None,
        };

        let report = upsert_thread(&paths, &thread).unwrap();
        assert_eq!(report.kind, StreamKind::Backfill);
        assert_eq!(report.new_items, 2);
        assert_eq!(report.http_requests, 4);
        assert_eq!(report.status, "ok");
        assert_eq!(
            query(&paths, "SELECT COUNT(*) AS n FROM posts").unwrap()[0].get("n"),
            Some(&json!(1))
        );
        assert_eq!(
            query(&paths, "SELECT COUNT(*) AS n FROM comments").unwrap()[0].get("n"),
            Some(&json!(1))
        );
        let rows = query(
            &paths,
            "SELECT kind, http_requests, status FROM sync_log WHERE subreddit = 'rust'",
        )
        .unwrap();
        assert_eq!(rows[0].get("kind"), Some(&json!("backfill")));
        assert_eq!(rows[0].get("http_requests"), Some(&json!(4)));
        assert_eq!(rows[0].get("status"), Some(&json!("ok")));
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn incomplete_thread_backfill_logs_gap() {
        let paths = temp_paths();
        let thread = ThreadView {
            post: Some(test_post("abc")),
            comments: vec![test_comment("def", "t3_abc")],
            more_stubs: 3,
            degraded: false,
            truncated: false,
            http_requests: 4,
            notice: Some("thread incomplete: 3 hidden comment(s) could not be resolved".to_owned()),
        };

        let report = upsert_thread(&paths, &thread).unwrap();
        assert_eq!(report.status, "gap");
        assert_eq!(report.remaining_items, Some(3));
        assert_eq!(
            report.notice.as_deref(),
            Some("thread incomplete: 3 hidden comment(s) could not be resolved")
        );
        let rows = query(
            &paths,
            "SELECT kind, status FROM sync_log WHERE subreddit = 'rust'",
        )
        .unwrap();
        assert_eq!(rows[0].get("kind"), Some(&json!("backfill")));
        assert_eq!(rows[0].get("status"), Some(&json!("gap")));
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn recent_post_fullnames_selects_newest_posts_for_refresh() {
        let paths = temp_paths();
        let mut older = test_post("old");
        older.created_utc = Some(10.0);
        let mut newer = test_post("new");
        newer.created_utc = Some(30.0);
        let mut middle = test_post("mid");
        middle.created_utc = Some(20.0);

        upsert_items(&paths, "r/RUST", StreamKind::Posts, &[older, newer, middle]).unwrap();
        assert_eq!(
            recent_post_fullnames(&paths, "RUST", 2).unwrap(),
            vec!["t3_new".to_owned(), "t3_mid".to_owned()]
        );
        assert_eq!(post_count(&paths, "rust").unwrap(), 3);
        assert!(recent_post_fullnames(&paths, "rust", 0).unwrap().is_empty());
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn digest_groups_posts_and_recent_comments() {
        let paths = temp_paths();
        let mut old_post = test_post("old");
        old_post.title = Some("Older thread with new replies".to_owned());
        old_post.created_utc = Some(10.0);
        old_post.num_comments = Some(3);
        let mut new_post = test_post("new");
        new_post.title = Some("Fresh post".to_owned());
        new_post.created_utc = Some(60.0);
        let mut other_sub = test_post("other");
        other_sub.subreddit = Some("python".to_owned());
        other_sub.created_utc = Some(70.0);

        upsert_items(&paths, "rust", StreamKind::Posts, &[old_post, new_post]).unwrap();
        upsert_items(&paths, "python", StreamKind::Posts, &[other_sub]).unwrap();

        let mut high = test_comment("high", "t3_old");
        high.post_id = Some("old".to_owned());
        high.body = Some("important reply".to_owned());
        high.score = Some(9);
        high.created_utc = Some(45.0);
        high.permalink = Some("/r/rust/comments/old/post/high/".to_owned());
        let mut low = test_comment("low", "t3_old");
        low.post_id = Some("old".to_owned());
        low.body = Some("lower score reply".to_owned());
        low.score = Some(1);
        low.created_utc = Some(50.0);
        low.permalink = Some("/r/rust/comments/old/post/low/".to_owned());
        let mut stale = test_comment("stale", "t3_old");
        stale.post_id = Some("old".to_owned());
        stale.score = Some(99);
        stale.created_utc = Some(20.0);
        let mut orphan = test_comment("orphan", "t3_missing");
        orphan.post_id = Some("missing".to_owned());
        orphan.body = Some("orphan reply".to_owned());
        orphan.score = Some(5);
        orphan.created_utc = Some(55.0);
        orphan.permalink = Some("/r/rust/comments/missing/post/orphan/".to_owned());
        let mut other_comment = test_comment("py", "t3_other");
        other_comment.subreddit = Some("python".to_owned());
        other_comment.post_id = Some("other".to_owned());
        other_comment.created_utc = Some(80.0);

        upsert_items(
            &paths,
            "rust",
            StreamKind::Comments,
            &[high, low, stale, orphan],
        )
        .unwrap();
        upsert_items(&paths, "python", StreamKind::Comments, &[other_comment]).unwrap();

        let digest = digest(&paths, Some("40"), Some("rust")).unwrap();
        assert_eq!(digest.since_utc, 40);
        assert_eq!(digest.subreddit.as_deref(), Some("rust"));
        assert_eq!(
            digest
                .posts
                .iter()
                .map(|post| post.id.as_str())
                .collect::<Vec<_>>(),
            vec!["new", "old"]
        );
        assert!(digest.posts[0].comments.is_empty());
        assert_eq!(
            digest.posts[1]
                .comments
                .iter()
                .map(|comment| comment.id.as_str())
                .collect::<Vec<_>>(),
            vec!["high", "low"]
        );
        assert_eq!(digest.orphan_comments.len(), 1);
        assert_eq!(digest.orphan_comments[0].id, "orphan");
        assert_eq!(digest.comment_count(), 3);
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn digest_rejects_invalid_subreddit_filter() {
        let paths = temp_paths();
        let error = digest(&paths, Some("0"), Some("rust/new"))
            .expect_err("path-like digest filter should be rejected")
            .to_string();
        assert!(error.contains("invalid subreddit name"));
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn digest_missing_database_is_empty_and_does_not_create_files() {
        let paths = temp_paths();
        let digest = digest(&paths, Some("0"), Some("rust")).unwrap();
        assert!(digest.posts.is_empty());
        assert!(digest.orphan_comments.is_empty());
        assert!(!paths.db_file.exists());
        assert!(!paths.data_dir.exists());
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn digest_uses_stable_tiebreakers_for_bounded_output() {
        let paths = temp_paths();
        let mut post_b = test_post("bbb");
        post_b.created_utc = Some(10.0);
        let mut post_a = test_post("aaa");
        post_a.created_utc = Some(10.0);
        let mut comment_b = test_comment("bbb_comment", "t3_aaa");
        comment_b.post_id = Some("aaa".to_owned());
        comment_b.score = Some(1);
        comment_b.created_utc = Some(20.0);
        let mut comment_a = test_comment("aaa_comment", "t3_aaa");
        comment_a.post_id = Some("aaa".to_owned());
        comment_a.score = Some(1);
        comment_a.created_utc = Some(20.0);

        upsert_items(&paths, "rust", StreamKind::Posts, &[post_b, post_a]).unwrap();
        upsert_items(
            &paths,
            "rust",
            StreamKind::Comments,
            &[comment_b, comment_a],
        )
        .unwrap();

        let digest = digest(&paths, Some("0"), Some("rust")).unwrap();
        assert_eq!(
            digest
                .posts
                .iter()
                .map(|post| post.id.as_str())
                .collect::<Vec<_>>(),
            vec!["aaa", "bbb"]
        );
        assert_eq!(
            digest.posts[0]
                .comments
                .iter()
                .map(|comment| comment.id.as_str())
                .collect::<Vec<_>>(),
            vec!["aaa_comment", "bbb_comment"]
        );
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn append_sync_log_records_refresh_reports() {
        let paths = temp_paths();
        let report = SyncStreamReport {
            subreddit: "RUST".to_owned(),
            kind: StreamKind::Refresh,
            new_items: 0,
            updated_items: 5,
            http_requests: 1,
            status: "gap".to_owned(),
            remaining_items: Some(3),
            notice: Some("refresh capped".to_owned()),
        };

        append_sync_log(&paths, &report, 10, 20, None).unwrap();
        let rows = query(
            &paths,
            "SELECT subreddit, kind, updated_items, http_requests, status FROM sync_log",
        )
        .unwrap();
        assert_eq!(rows[0].get("subreddit"), Some(&json!("rust")));
        assert_eq!(rows[0].get("kind"), Some(&json!("refresh")));
        assert_eq!(rows[0].get("updated_items"), Some(&json!(5)));
        assert_eq!(rows[0].get("http_requests"), Some(&json!(1)));
        assert_eq!(rows[0].get("status"), Some(&json!("gap")));
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn digest_groups_recent_comments_under_posts() {
        let paths = temp_paths();
        let mut old_post = test_post("abc");
        old_post.created_utc = Some(10.0);
        let mut recent_post = test_post("new");
        recent_post.created_utc = Some(40.0);
        recent_post.title = Some("Recent post".to_owned());

        let mut top_comment = test_comment("top", "t3_abc");
        top_comment.created_utc = Some(30.0);
        top_comment.score = Some(10);
        top_comment.body = Some("top recent comment".to_owned());
        let mut lower_comment = test_comment("low", "t3_abc");
        lower_comment.created_utc = Some(20.0);
        lower_comment.score = Some(1);
        lower_comment.body = Some("lower recent comment".to_owned());

        upsert_items(&paths, "rust", StreamKind::Posts, &[old_post, recent_post]).unwrap();
        upsert_items(
            &paths,
            "rust",
            StreamKind::Comments,
            &[top_comment, lower_comment],
        )
        .unwrap();

        let digest = digest(&paths, Some("15"), None).unwrap();
        assert_eq!(digest.posts.len(), 2);
        assert_eq!(digest.comment_count(), 2);
        assert_eq!(digest.posts[0].id, "new");
        let old_group = digest.posts.iter().find(|post| post.id == "abc").unwrap();
        assert_eq!(old_group.comments.len(), 2);
        assert_eq!(old_group.comments[0].id, "top");
        assert_eq!(
            old_group.comments[0].permalink.as_deref(),
            Some("https://www.reddit.com/r/rust/comments/abc/post/top/")
        );
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn digest_filters_subreddit_and_keeps_orphan_comments_clear() {
        let paths = temp_paths();
        let mut rust_orphan = test_comment("orphan", "t3_missing");
        rust_orphan.post_id = Some("missing".to_owned());
        rust_orphan.created_utc = Some(30.0);
        rust_orphan.score = Some(5);
        let mut swift_orphan = test_comment("swift", "t3_missing2");
        swift_orphan.post_id = Some("missing2".to_owned());
        swift_orphan.subreddit = Some("swift".to_owned());
        swift_orphan.created_utc = Some(40.0);

        upsert_items(&paths, "rust", StreamKind::Comments, &[rust_orphan.clone()]).unwrap();
        upsert_items(&paths, "swift", StreamKind::Comments, &[swift_orphan]).unwrap();

        let rust_digest = digest(&paths, Some("24"), Some("rust")).unwrap();
        assert!(rust_digest.posts.is_empty());
        assert_eq!(rust_digest.subreddit.as_deref(), Some("rust"));
        assert_eq!(rust_digest.orphan_comments.len(), 1);
        assert_eq!(rust_digest.orphan_comments[0].id, "orphan");

        let empty = digest(&paths, Some("31"), Some("rust")).unwrap();
        assert!(empty.posts.is_empty());
        assert!(empty.orphan_comments.is_empty());
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn parse_since_accepts_relative_hours_and_days() {
        let now = now_utc();
        let hour_cutoff = parse_since("1h").unwrap();
        let day_cutoff = parse_since("1d").unwrap();
        assert!((now - 3601..=now - 3599).contains(&hour_cutoff));
        assert!((now - 86401..=now - 86399).contains(&day_cutoff));
        assert_eq!(parse_since("42").unwrap(), 42);
    }

    #[test]
    fn parse_since_rejects_negative_zero_and_overflowing_values() {
        assert!(parse_since("-1").is_err());
        assert!(parse_since("0h").is_err());
        assert!(parse_since("-2d").is_err());
        assert!(parse_since("999999999999999999999999999999d").is_err());
    }

    fn temp_paths() -> Paths {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let count = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("rdt-store-{}-{stamp}-{count}", std::process::id()));
        Paths {
            config_file: root.join("config.toml"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            db_file: root.join("data/rdt.db"),
            last_file: root.join("cache/last.json"),
        }
    }

    fn test_post(id: &str) -> RedditItem {
        RedditItem {
            index: None,
            kind: ItemKind::Post,
            id: id.to_owned(),
            fullname: format!("t3_{id}"),
            parent_id: None,
            post_id: Some(id.to_owned()),
            title: Some("Post".to_owned()),
            author: Some("alice".to_owned()),
            subreddit: Some("rust".to_owned()),
            body: Some("body".to_owned()),
            flair: None,
            is_self: Some(true),
            over_18: Some(false),
            score: Some(1),
            upvote_ratio: Some(0.9),
            num_comments: Some(1),
            created_utc: Some(10.0),
            edited_utc: None,
            permalink: Some(format!("/r/rust/comments/{id}/post/")),
            url: None,
            depth: 0,
            source: ItemSource::Json,
        }
    }

    fn test_comment(id: &str, parent_id: &str) -> RedditItem {
        RedditItem {
            index: None,
            kind: ItemKind::Comment,
            id: id.to_owned(),
            fullname: format!("t1_{id}"),
            parent_id: Some(parent_id.to_owned()),
            post_id: Some("abc".to_owned()),
            title: None,
            author: Some("bob".to_owned()),
            subreddit: Some("rust".to_owned()),
            body: Some("reply".to_owned()),
            flair: None,
            is_self: None,
            over_18: None,
            score: Some(2),
            upvote_ratio: None,
            num_comments: None,
            created_utc: Some(11.0),
            edited_utc: None,
            permalink: Some(format!("/r/rust/comments/abc/post/{id}/")),
            url: None,
            depth: 0,
            source: ItemSource::Json,
        }
    }
}
