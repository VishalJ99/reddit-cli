use crate::{
    model::{DbRow, Digest, DigestComment, DigestPost, ItemKind, RedditItem, ThreadView},
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

    if thread.more_stubs > 0 && thread.notice.is_none() {
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

pub fn print_digest(digest: &Digest, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(digest)?);
        return Ok(());
    }

    print!("{}", format_digest_markdown(digest));
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
                    "remaining_items": report.remaining_items,
                    "notice": report.notice.as_deref(),
                })
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    for report in reports {
        let remaining = report
            .remaining_items
            .map(|count| format!(", {count} remaining"))
            .unwrap_or_default();
        println!(
            "r/{} {}: {} new, {} updated, {} request(s), {}{}",
            report.subreddit,
            report.kind.as_str(),
            report.new_items,
            report.updated_items,
            report.http_requests,
            report.status,
            remaining
        );
        if let Some(notice) = &report.notice {
            println!("  {notice}");
        }
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

pub fn format_digest_markdown(digest: &Digest) -> String {
    let mut out = String::new();
    out.push_str("# rdt digest\n\n");
    out.push_str(&format!("- Since: {}\n", digest.since_utc));
    out.push_str(&format!("- Generated: {}\n", digest.generated_utc));
    if let Some(subreddit) = &digest.subreddit {
        out.push_str(&format!("- Subreddit: r/{subreddit}\n"));
    }
    out.push_str(&format!(
        "- Activity: {} post group(s), {} new comment(s)\n",
        digest.posts.len(),
        digest.comment_count()
    ));

    if digest.posts.is_empty() && digest.orphan_comments.is_empty() {
        out.push_str("\nNo local activity matched.\n");
        return out;
    }

    for post in &digest.posts {
        append_digest_post(&mut out, post);
    }

    if !digest.orphan_comments.is_empty() {
        out.push_str("\n## Comments without local posts\n");
        for comment in &digest.orphan_comments {
            append_digest_comment(&mut out, comment);
        }
    }

    out
}

fn append_digest_post(out: &mut String, post: &DigestPost) {
    let title = post.title.as_deref().unwrap_or("(untitled)");
    let subreddit = post
        .subreddit
        .as_deref()
        .map(|subreddit| format!("r/{subreddit}"))
        .unwrap_or_else(|| "unknown subreddit".to_owned());
    out.push_str(&format!(
        "\n## {subreddit}: {}\n",
        trim_for_digest(title, 180)
    ));
    if let Some(permalink) = &post.permalink {
        out.push_str(&format!("- Post: {permalink}\n"));
    }
    let author = post.author.as_deref().unwrap_or("[unknown]");
    out.push_str(&format!(
        "- u/{author} - {} pts - {} comments - activity {}\n",
        post.score
            .map(|score| score.to_string())
            .unwrap_or_else(|| "?".to_owned()),
        post.num_comments
            .map(|count| count.to_string())
            .unwrap_or_else(|| "?".to_owned()),
        post.activity_utc
    ));

    if post.comments.is_empty() {
        out.push_str("\nNo new comments captured for this post.\n");
        return;
    }

    out.push_str("\nTop new comments:\n");
    for comment in &post.comments {
        append_digest_comment(out, comment);
    }
}

fn append_digest_comment(out: &mut String, comment: &DigestComment) {
    let author = comment.author.as_deref().unwrap_or("[unknown]");
    let body = comment.body.as_deref().unwrap_or("(empty)");
    let score = comment
        .score
        .map(|score| score.to_string())
        .unwrap_or_else(|| "?".to_owned());
    let created = comment
        .created_utc
        .map(|created| created.to_string())
        .unwrap_or_else(|| "?".to_owned());
    out.push_str(&format!(
        "- u/{author} - {score} pts - {created}: {}\n",
        trim_for_digest(body, 500)
    ));
    if let Some(permalink) = &comment.permalink {
        out.push_str(&format!("  {permalink}\n"));
    }
}

fn trim_for_digest(input: &str, max: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_digest_markdown_with_posts_comments_and_links() {
        let digest = Digest {
            generated_utc: 100,
            since_utc: 10,
            subreddit: Some("rust".to_owned()),
            posts: vec![DigestPost {
                id: "abc".to_owned(),
                subreddit: Some("rust".to_owned()),
                title: Some("Interesting thread".to_owned()),
                author: Some("alice".to_owned()),
                permalink: Some("https://www.reddit.com/r/rust/comments/abc/post/".to_owned()),
                score: Some(12),
                num_comments: Some(3),
                created_utc: Some(20),
                activity_utc: 30,
                comments: vec![DigestComment {
                    id: "def".to_owned(),
                    post_id: Some("abc".to_owned()),
                    parent_id: Some("t3_abc".to_owned()),
                    subreddit: Some("rust".to_owned()),
                    author: Some("bob".to_owned()),
                    body: Some("Useful reply".to_owned()),
                    score: Some(4),
                    created_utc: Some(30),
                    permalink: Some(
                        "https://www.reddit.com/r/rust/comments/abc/post/def/".to_owned(),
                    ),
                }],
            }],
            orphan_comments: Vec::new(),
        };

        let markdown = format_digest_markdown(&digest);
        assert!(markdown.contains("# rdt digest"));
        assert!(markdown.contains("r/rust: Interesting thread"));
        assert!(markdown.contains("https://www.reddit.com/r/rust/comments/abc/post/"));
        assert!(markdown.contains("Top new comments"));
        assert!(markdown.contains("u/bob - 4 pts - 30: Useful reply"));
    }

    #[test]
    fn formats_empty_digest_clearly() {
        let digest = Digest {
            generated_utc: 100,
            since_utc: 10,
            subreddit: None,
            posts: Vec::new(),
            orphan_comments: Vec::new(),
        };
        let markdown = format_digest_markdown(&digest);
        assert!(markdown.contains("0 post group(s), 0 new comment(s)"));
        assert!(markdown.contains("No local activity matched."));
    }
}
