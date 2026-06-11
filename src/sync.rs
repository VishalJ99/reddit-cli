use crate::{
    cli::SyncCommand,
    config::Paths,
    model::ListingPage,
    store::{self, StreamKind, SyncStreamReport},
    transport::RedditClient,
};
use anyhow::Result;

#[derive(Debug, Default)]
struct SyncProgress {
    new_items: usize,
    updated_items: usize,
    http_requests: usize,
}

pub async fn run_once(
    paths: &Paths,
    client: &RedditClient,
    command: &SyncCommand,
) -> Result<Vec<SyncStreamReport>> {
    let subreddits = if command.subreddits.is_empty() {
        store::active_watches(paths)?
    } else {
        command
            .subreddits
            .iter()
            .map(|subreddit| store::normalize_subreddit(subreddit))
            .collect()
    };

    let mut reports = Vec::new();
    for subreddit in subreddits {
        reports.push(sync_stream(paths, client, &subreddit, StreamKind::Posts, command).await?);
        reports.push(sync_stream(paths, client, &subreddit, StreamKind::Comments, command).await?);
    }
    Ok(reports)
}

async fn sync_stream(
    paths: &Paths,
    client: &RedditClient,
    subreddit: &str,
    kind: StreamKind,
    command: &SyncCommand,
) -> Result<SyncStreamReport> {
    let started = store::utc_now();
    let mut progress = SyncProgress::default();
    let result = sync_stream_inner(paths, client, subreddit, kind, command, &mut progress).await;

    match result {
        Ok(report) => {
            store::append_sync_log(paths, &report, started, store::utc_now(), None)?;
            Ok(report)
        }
        Err(error) => {
            let message = error.to_string();
            let report = SyncStreamReport {
                subreddit: store::normalize_subreddit(subreddit),
                kind,
                new_items: progress.new_items,
                updated_items: progress.updated_items,
                http_requests: progress.http_requests,
                status: "error".to_owned(),
                remaining_items: None,
                notice: Some(message.clone()),
            };
            if let Err(log_error) =
                store::append_sync_log(paths, &report, started, store::utc_now(), Some(&message))
            {
                return Err(error.context(format!("also failed to append sync_log: {log_error}")));
            }
            Err(error)
        }
    }
}

async fn sync_stream_inner(
    paths: &Paths,
    client: &RedditClient,
    subreddit: &str,
    kind: StreamKind,
    command: &SyncCommand,
    progress: &mut SyncProgress,
) -> Result<SyncStreamReport> {
    let listing = match kind {
        StreamKind::Posts => "new",
        StreamKind::Comments => "comments",
        StreamKind::Backfill => unreachable!("backfill is not a watch stream"),
    };
    let mut remaining = command.budget.max(1) as usize;
    let page_cap = command
        .pages
        .map(|pages| pages.max(1) as usize)
        .unwrap_or_else(|| remaining.div_ceil(100).max(1));

    let mut after = None;
    let mut newest_fullname = None;
    let mut status = "ok".to_owned();

    while progress.http_requests < page_cap && remaining > 0 {
        let page_limit = remaining.min(100) as u32;
        progress.http_requests += 1;
        let page = client
            .sync_listing_page(subreddit, listing, page_limit, after.as_deref())
            .await?;

        if progress.http_requests == 1 {
            newest_fullname = page.items.first().map(|item| item.fullname.clone());
        }

        if page.items.is_empty() {
            after = None;
            break;
        }

        let known = store::known_count(paths, kind, &page.items)?;
        let stats = store::upsert_items(paths, subreddit, kind, &page.items)?;
        progress.new_items += stats.new_items;
        progress.updated_items += stats.updated_items;

        let relevant_len = sync_relevant_len(kind, &page);
        remaining = remaining.saturating_sub(relevant_len);
        let fully_known = known == relevant_len;
        let next_after = page.after;
        if fully_known || next_after.is_none() {
            after = None;
            break;
        }
        after = next_after;
    }

    if after.is_some() {
        status = "gap".to_owned();
    }

    store::update_watch_watermark(paths, subreddit, kind, newest_fullname.as_deref())?;
    let report = SyncStreamReport {
        subreddit: store::normalize_subreddit(subreddit),
        kind,
        new_items: progress.new_items,
        updated_items: progress.updated_items,
        http_requests: progress.http_requests,
        status,
        remaining_items: None,
        notice: None,
    };
    Ok(report)
}

fn sync_relevant_len(kind: StreamKind, page: &ListingPage) -> usize {
    page.items
        .iter()
        .filter(|item| match kind {
            StreamKind::Posts => item.kind == crate::model::ItemKind::Post,
            StreamKind::Comments => item.kind == crate::model::ItemKind::Comment,
            StreamKind::Backfill => false,
        })
        .count()
}
