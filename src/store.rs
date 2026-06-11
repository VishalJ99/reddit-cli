use crate::{config::Paths, model::DbRow};
use anyhow::{Context, Result};
use rusqlite::{Connection, params, types::ValueRef};
use serde_json::{Map, Value, json};
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

const SCHEMA: &str = include_str!("store/schema.sql");

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

fn open(paths: &Paths) -> Result<Connection> {
    fs::create_dir_all(&paths.data_dir)?;
    let conn = Connection::open(&paths.db_file)
        .with_context(|| format!("opening {}", paths.db_file.display()))?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
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
        .to_owned()
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
    use std::time::{SystemTime, UNIX_EPOCH};

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

    fn temp_paths() -> Paths {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rdt-store-{stamp}"));
        Paths {
            config_file: root.join("config.toml"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            db_file: root.join("data/rdt.db"),
            last_file: root.join("cache/last.json"),
        }
    }
}
