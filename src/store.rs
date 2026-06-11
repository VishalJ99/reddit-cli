use crate::{
    config::Paths,
    model::{DbRow, ItemKind, RedditItem, ThreadView},
};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params, types::ValueRef};
use serde_json::{Map, Value, json};
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

const SCHEMA: &str = include_str!("store/schema.sql");

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

#[derive(Debug, Default, Clone, Copy)]
pub struct UpsertStats {
    pub new_items: usize,
    pub updated_items: usize,
}

pub fn watch_add(paths: &Paths, subreddits: &[String]) -> Result<()> {
    let conn = open(paths)?;
    let now = now_utc();
    for subreddit in subreddits {
        let subreddit = clean_subreddit(subreddit);
        conn.execute(
            "INSERT INTO watches (subreddit, active, added_utc) VALUES (?1, 1, ?2)
             ON CONFLICT(subreddit) DO UPDATE SET active = 1",
            params![subreddit, now],
        )?;
    }
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
        "SELECT subreddit, active, added_utc, last_synced_utc FROM watches ORDER BY subreddit",
    )
}

pub fn active_watches(paths: &Paths) -> Result<Vec<String>> {
    let conn = open(paths)?;
    let mut stmt =
        conn.prepare("SELECT subreddit FROM watches WHERE active = 1 ORDER BY subreddit")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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

pub fn digest_rows(paths: &Paths, since: Option<&str>, sub: Option<&str>) -> Result<Vec<DbRow>> {
    let cutoff = since.map(parse_since).transpose()?.unwrap_or(0);
    let conn = open(paths)?;
    let mut sql = "SELECT 'post' AS kind, subreddit, title, permalink, created_utc FROM posts WHERE created_utc >= ?1".to_owned();
    if sub.is_some() {
        sql.push_str(" AND subreddit = ?2");
    }
    sql.push_str(" ORDER BY created_utc DESC LIMIT 100");

    let mut stmt = conn.prepare(&sql)?;
    let names = column_names(&stmt);
    let rows = if let Some(subreddit) = sub {
        rows_to_json(
            stmt.query(params![cutoff, clean_subreddit(subreddit)])?,
            &names,
        )?
    } else {
        rows_to_json(stmt.query(params![cutoff])?, &names)?
    };
    Ok(rows)
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
    conn.execute_batch(
        "UPDATE watches SET subreddit = lower(subreddit) WHERE subreddit != lower(subreddit);
         UPDATE posts SET subreddit = lower(subreddit) WHERE subreddit != lower(subreddit);
         UPDATE comments SET subreddit = lower(subreddit) WHERE subreddit != lower(subreddit);",
    )?;
    Ok(conn)
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
        let hours: i64 = hours.parse()?;
        return Ok(now - hours * 60 * 60);
    }
    if let Some(days) = trimmed.strip_suffix('d') {
        let days: i64 = days.parse()?;
        return Ok(now - days * 24 * 60 * 60);
    }
    Ok(trimmed.parse()?)
}

fn clean_subreddit(input: &str) -> String {
    input
        .trim()
        .trim_start_matches("r/")
        .trim_start_matches("/r/")
        .to_ascii_lowercase()
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
        watch_add(&paths, &[String::from("r/rust")]).unwrap();
        let rows = watch_list(&paths).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("subreddit").unwrap(), "rust");
        let _ = fs::remove_dir_all(paths.data_dir.parent().unwrap());
    }

    #[test]
    fn db_query_rejects_writes() {
        let paths = temp_paths();
        watch_add(&paths, &[String::from("rust")]).unwrap();
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
