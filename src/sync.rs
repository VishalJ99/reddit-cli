use crate::{
    cli::SyncCommand,
    config::{Config, Paths},
    model::ListingPage,
    store::{self, StreamKind, SyncStreamReport, WatchTarget},
    transport::RedditClient,
};
use anyhow::Result;

const DEFAULT_SYNC_BUDGET: u32 = 300;

#[derive(Debug, Default)]
struct SyncProgress {
    new_items: usize,
    updated_items: usize,
    http_requests: usize,
}

#[derive(Debug, Clone, Copy)]
struct EffectiveSyncOptions {
    budget: usize,
    page_cap: usize,
    refresh: bool,
}

pub async fn run_once(
    paths: &Paths,
    client: &RedditClient,
    config: &Config,
    command: &SyncCommand,
) -> Result<Vec<SyncStreamReport>> {
    let targets = store::sync_targets(paths, &command.subreddits)?;

    let mut reports = Vec::new();
    for target in targets {
        let options = effective_options(command, config, &target);
        reports
            .push(sync_stream(paths, client, &target.subreddit, StreamKind::Posts, options).await?);
        reports.push(
            sync_stream(
                paths,
                client,
                &target.subreddit,
                StreamKind::Comments,
                options,
            )
            .await?,
        );
        if options.refresh {
            reports.push(sync_refresh(paths, client, &target.subreddit, options).await?);
        }
    }
    Ok(reports)
}

async fn sync_refresh(
    paths: &Paths,
    client: &RedditClient,
    subreddit: &str,
    options: EffectiveSyncOptions,
) -> Result<SyncStreamReport> {
    let started = store::utc_now();
    let mut progress = SyncProgress::default();
    let result = sync_refresh_inner(paths, client, subreddit, options, &mut progress).await;

    match result {
        Ok(report) => {
            store::append_sync_log(paths, &report, started, store::utc_now(), None)?;
            Ok(report)
        }
        Err(error) => {
            let message = error.to_string();
            let report = SyncStreamReport {
                subreddit: store::normalize_subreddit(subreddit),
                kind: StreamKind::Refresh,
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

async fn sync_stream(
    paths: &Paths,
    client: &RedditClient,
    subreddit: &str,
    kind: StreamKind,
    options: EffectiveSyncOptions,
) -> Result<SyncStreamReport> {
    let started = store::utc_now();
    let mut progress = SyncProgress::default();
    let result = sync_stream_inner(paths, client, subreddit, kind, options, &mut progress).await;

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
    options: EffectiveSyncOptions,
    progress: &mut SyncProgress,
) -> Result<SyncStreamReport> {
    let listing = match kind {
        StreamKind::Posts => "new",
        StreamKind::Comments => "comments",
        StreamKind::Backfill => unreachable!("backfill is not a watch stream"),
        StreamKind::Refresh => unreachable!("refresh is not a watch stream"),
    };
    let mut remaining = options.budget;
    let page_cap = options.page_cap;

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

async fn sync_refresh_inner(
    paths: &Paths,
    client: &RedditClient,
    subreddit: &str,
    options: EffectiveSyncOptions,
    progress: &mut SyncProgress,
) -> Result<SyncStreamReport> {
    let request_limit = refresh_request_limit(options);
    let total_candidates = store::post_count(paths, subreddit)?;
    let candidates = store::recent_post_fullnames(paths, subreddit, request_limit)?;
    let remaining_items = total_candidates.saturating_sub(candidates.len());

    for chunk in candidates.chunks(100) {
        progress.http_requests += 1;
        let items = client.info_by_ids(chunk).await?;
        let stats = store::upsert_items(paths, subreddit, StreamKind::Posts, &items)?;
        progress.new_items += stats.new_items;
        progress.updated_items += stats.updated_items;
    }

    Ok(SyncStreamReport {
        subreddit: store::normalize_subreddit(subreddit),
        kind: StreamKind::Refresh,
        new_items: progress.new_items,
        updated_items: progress.updated_items,
        http_requests: progress.http_requests,
        status: if remaining_items > 0 { "gap" } else { "ok" }.to_owned(),
        remaining_items: (remaining_items > 0).then_some(remaining_items),
        notice: (remaining_items > 0).then(|| {
            format!("refresh capped at {request_limit} recent post(s); raise --budget/--pages to refresh more")
        }),
    })
}

fn effective_options(
    command: &SyncCommand,
    config: &Config,
    target: &WatchTarget,
) -> EffectiveSyncOptions {
    let budget = command
        .budget
        .or(target.budget)
        .or(config.sync_budget)
        .unwrap_or(DEFAULT_SYNC_BUDGET)
        .max(1) as usize;
    let page_cap = command
        .pages
        .or(target.page_cap)
        .or(config.page_cap)
        .map(|pages| pages.max(1) as usize)
        .unwrap_or_else(|| budget.div_ceil(100).max(1));
    let refresh = command
        .refresh_override()
        .or(target.refresh)
        .or(config.sync_refresh)
        .unwrap_or(false);

    EffectiveSyncOptions {
        budget,
        page_cap,
        refresh,
    }
}

fn refresh_request_limit(options: EffectiveSyncOptions) -> usize {
    options.budget.min(options.page_cap.saturating_mul(100))
}

fn sync_relevant_len(kind: StreamKind, page: &ListingPage) -> usize {
    page.items
        .iter()
        .filter(|item| match kind {
            StreamKind::Posts => item.kind == crate::model::ItemKind::Post,
            StreamKind::Comments => item.kind == crate::model::ItemKind::Comment,
            StreamKind::Backfill => false,
            StreamKind::Refresh => false,
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_limit_uses_budget_as_hard_cap() {
        let options = options(250, 10, true);
        assert_eq!(refresh_request_limit(options), 250);
    }

    #[test]
    fn refresh_limit_uses_pages_as_batch_cap() {
        let options = options(500, 2, true);
        assert_eq!(refresh_request_limit(options), 200);
    }

    #[test]
    fn effective_options_prefer_cli_then_watch_then_config() {
        let command = command(Some(25), Some(1), false, false);
        let config = Config {
            page_cap: Some(4),
            sync_budget: Some(400),
            sync_refresh: Some(false),
            ..Config::default()
        };
        let target = target(Some(2), Some(200), Some(true));
        let options = effective_options(&command, &config, &target);
        assert_eq!(options.budget, 25);
        assert_eq!(options.page_cap, 1);
        assert!(options.refresh);
    }

    #[test]
    fn effective_options_allow_cli_refresh_disable() {
        let command = command(None, None, false, true);
        let config = Config::default();
        let target = target(None, None, Some(true));
        let options = effective_options(&command, &config, &target);
        assert!(!options.refresh);
    }

    fn command(
        budget: Option<u32>,
        pages: Option<u32>,
        refresh: bool,
        no_refresh: bool,
    ) -> SyncCommand {
        SyncCommand {
            subreddits: Vec::new(),
            pages,
            loop_secs: None,
            budget,
            refresh,
            no_refresh,
        }
    }

    fn target(page_cap: Option<u32>, budget: Option<u32>, refresh: Option<bool>) -> WatchTarget {
        WatchTarget {
            subreddit: "rust".to_owned(),
            page_cap,
            budget,
            refresh,
        }
    }

    fn options(budget: usize, page_cap: usize, refresh: bool) -> EffectiveSyncOptions {
        EffectiveSyncOptions {
            budget,
            page_cap,
            refresh,
        }
    }
}
