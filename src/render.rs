use crate::{
    model::{DbRow, ItemKind, RedditItem, ThreadView},
    store::SyncStreamReport,
};
use anyhow::Result;
use owo_colors::OwoColorize;
use serde::Serialize;
use serde_json::Value;

pub fn print_items(items: &[RedditItem], json: bool, no_color: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(items)?);
        return Ok(());
    }

    for item in items {
        print_item(item, no_color);
    }
    Ok(())
}

pub fn print_thread(thread: &ThreadView, json: bool, no_color: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(thread)?);
        return Ok(());
    }

    if let Some(notice) = &thread.notice {
        eprintln!("note: {notice}");
    }

    if let Some(post) = &thread.post {
        print_item(post, no_color);
        println!();
    }

    for comment in &thread.comments {
        print_item(comment, no_color);
    }

    if thread.more_stubs > 0 {
        eprintln!(
            "thread truncated: {} hidden comment stub(s) not expanded",
            thread.more_stubs
        );
    }

    Ok(())
}

pub fn print_json_or_debug(value: &Value, _json: bool) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn print_db_rows(rows: &[DbRow], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(rows)?);
        return Ok(());
    }

    if rows.is_empty() {
        println!("(no rows)");
        return Ok(());
    }

    for (index, row) in rows.iter().enumerate() {
        println!("[{}]", index + 1);
        for (key, value) in row {
            println!("  {key}: {}", value_to_text(value));
        }
    }
    Ok(())
}

pub fn print_digest(rows: &[DbRow], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(rows)?);
        return Ok(());
    }

    println!("# rdt digest");
    if rows.is_empty() {
        println!("\nNo local activity matched.");
        return Ok(());
    }

    for row in rows {
        let title = row.get("title").map(value_to_text).unwrap_or_default();
        let permalink = row.get("permalink").map(value_to_text).unwrap_or_default();
        println!("\n- {title}");
        if !permalink.is_empty() {
            println!("  {permalink}");
        }
    }
    Ok(())
}

pub fn print_sync_reports(reports: &[SyncStreamReport], json: bool) -> Result<()> {
    if json {
        let rows = reports
            .iter()
            .map(|report| {
                serde_json::json!({
                    "subreddit": report.subreddit,
                    "kind": report.kind.as_str(),
                    "new_items": report.new_items,
                    "updated_items": report.updated_items,
                    "http_requests": report.http_requests,
                    "status": report.status,
                })
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    for report in reports {
        println!(
            "r/{} {}: {} new, {} updated, {} request(s), {}",
            report.subreddit,
            report.kind.as_str(),
            report.new_items,
            report.updated_items,
            report.http_requests,
            report.status
        );
    }
    Ok(())
}

fn print_item(item: &RedditItem, no_color: bool) {
    let index = item.index.map_or("-".to_owned(), |index| index.to_string());
    let indent = "  ".repeat(item.depth);
    let kind = kind_label(&item.kind);
    let subreddit = item
        .subreddit
        .as_deref()
        .map(|sub| format!(" r/{sub}"))
        .unwrap_or_default();
    let author = item
        .author
        .as_deref()
        .map(|author| format!(" u/{author}"))
        .unwrap_or_default();
    let score = item
        .score
        .map(|score| format!(" {score} pts"))
        .unwrap_or_default();
    let title = trim_for_terminal(item.display_title(), 180);

    if no_color {
        println!("{indent}{index}. [{kind}]{subreddit}{author}{score} {title}");
    } else {
        println!(
            "{}{}. {}{}{}{} {}",
            indent,
            index.blue(),
            format!("[{kind}]").bright_black(),
            subreddit.green(),
            author.yellow(),
            score.bright_black(),
            title
        );
    }

    if let Some(link) = item.canonical_permalink() {
        println!("{indent}   {link}");
    }
}

fn kind_label(kind: &ItemKind) -> &'static str {
    match kind {
        ItemKind::Post => "post",
        ItemKind::Comment => "comment",
        ItemKind::Subreddit => "sub",
        ItemKind::User => "user",
        ItemKind::More => "more",
        ItemKind::Unknown => "item",
    }
}

fn trim_for_terminal(input: &str, max: usize) -> String {
    let input = input.replace('\n', " ");
    if input.chars().count() <= max {
        return input;
    }
    let mut out = input
        .chars()
        .take(max.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

fn value_to_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

pub fn to_json_value<T: Serialize>(value: T) -> Result<Value> {
    Ok(serde_json::to_value(value)?)
}
