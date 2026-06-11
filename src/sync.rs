use crate::{
    cli::{SyncCommand, ThreadCommand, ThreadSort},
    config::{Config, Paths},
    model::{ItemKind, ItemSource, ListingPage, RedditItem},
    store::{self, StreamKind, SyncStreamReport, WatchTarget},
    transport::RedditClient,
};
use anyhow::{Context, Result};

const DEFAULT_SYNC_BUDGET: u32 = 300;
const DEFAULT_BACKFILL_BUDGET: u32 = 3;
const DEFAULT_BACKFILL_PAGE_CAP: u32 = 1;
const BACKFILL_LISTING_LIMIT: u32 = 100;

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
    if let Some(subreddit) = &command.backfill {
        return Ok(vec![
            sync_backfill(paths, client, command, subreddit).await?,
        ]);
    }

    let targets = store::sync_targets(paths, &command.subreddits)?;

    let mut reports = Vec::new();
    for target in targets {
        let mut options = effective_options(command, config, &target);
        if client.is_force_rss() {
            options.refresh = false;
        }
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

async fn sync_backfill(
    paths: &Paths,
    client: &RedditClient,
    command: &SyncCommand,
    subreddit: &str,
) -> Result<SyncStreamReport> {
    let started = store::utc_now();
    let mut progress = SyncProgress::default();
    let result = sync_backfill_inner(paths, client, command, subreddit, &mut progress).await;

    match result {
        Ok(report) => {
            store::append_sync_log(paths, &report, started, store::utc_now(), None)?;
            Ok(report)
        }
        Err(error) => {
            let message = error.to_string();
            let report = SyncStreamReport {
                subreddit: store::normalize_subreddit(subreddit),
                kind: StreamKind::Backfill,
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
            if client.is_force_rss() {
                return Ok(report);
            }
            Err(error)
        }
    }
}

async fn sync_backfill_inner(
    paths: &Paths,
    client: &RedditClient,
    command: &SyncCommand,
    subreddit: &str,
    progress: &mut SyncProgress,
) -> Result<SyncStreamReport> {
    let target_subreddit = store::normalize_subreddit(subreddit);
    let options = backfill_options(command);
    let cutoff = backfill_cutoff(command.backfill_days())?;
    let mut remaining_posts = options.budget;
    let mut listing_pages = 0usize;
    let mut after = None;
    let mut reached_cutoff = false;
    let mut budget_exhausted = false;
    let mut page_cap_exhausted = false;
    let mut unresolved_comments = 0usize;

    'pages: while listing_pages < options.page_cap && remaining_posts > 0 {
        listing_pages += 1;
        progress.http_requests += 1;
        let page = client
            .sync_listing_page(
                &target_subreddit,
                "new",
                backfill_listing_limit(),
                after.as_deref(),
            )
            .await?;

        if page.items.is_empty() {
            after = None;
            break;
        }

        for item in page.items.iter().filter(|item| item.kind == ItemKind::Post) {
            if let Some(created_utc) = item.created_utc.map(|created| created as i64)
                && created_utc < cutoff
            {
                reached_cutoff = true;
                break 'pages;
            }

            if remaining_posts == 0 {
                budget_exhausted = true;
                break 'pages;
            }

            let target_url = post_target(item);
            let thread_command = ThreadCommand {
                target: target_url.clone(),
                all: true,
                depth: None,
                sort: ThreadSort::Best,
                max_requests: command.backfill_max_requests(),
            };
            let thread = client
                .thread_uncached(&thread_command, &target_url)
                .await
                .with_context(|| format!("backfilling {}", item.fullname))?;
            progress.http_requests += thread.http_requests;
            if thread.degraded {
                anyhow::bail!(
                    "backfill requires JSON parent metadata; RSS degraded output was not written"
                );
            }
            if thread.truncated || thread.more_stubs > 0 {
                unresolved_comments += thread.more_stubs.max(1);
            }

            let (_, stats) = store::upsert_thread_items(paths, &thread)?;
            progress.new_items += stats.new_items;
            progress.updated_items += stats.updated_items;
            remaining_posts -= 1;
        }

        after = page.after;
        if remaining_posts == 0 && after.is_some() {
            budget_exhausted = true;
            break;
        }
        if after.is_none() {
            break;
        }
    }

    if listing_pages >= options.page_cap && after.is_some() && !reached_cutoff {
        page_cap_exhausted = true;
    }

    let mut notices = Vec::new();
    if budget_exhausted {
        notices.push(format!(
            "backfill capped at {} post(s); raise --budget to pull more",
            options.budget
        ));
    }
    if page_cap_exhausted {
        notices.push(format!(
            "backfill scanned {} listing page(s); raise --pages to inspect more",
            options.page_cap
        ));
    }
    if unresolved_comments > 0 {
        notices.push(format!(
            "{unresolved_comments} hidden comment(s) remained unresolved; raise --max-requests"
        ));
    }

    let status = if notices.is_empty() { "ok" } else { "gap" }.to_owned();
    Ok(SyncStreamReport {
        subreddit: target_subreddit,
        kind: StreamKind::Backfill,
        new_items: progress.new_items,
        updated_items: progress.updated_items,
        http_requests: progress.http_requests,
        status,
        remaining_items: (unresolved_comments > 0).then_some(unresolved_comments),
        notice: (!notices.is_empty()).then(|| notices.join("; ")),
    })
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
            if client.is_force_rss() {
                return Ok(report);
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
            if client.is_force_rss() {
                return Ok(report);
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
    let mut notice = None;

    while progress.http_requests < page_cap && remaining > 0 {
        let page_limit = remaining.min(100) as u32;
        progress.http_requests += 1;
        let page = client
            .sync_listing_page(subreddit, listing, page_limit, after.as_deref())
            .await?;
        let rss_degraded = page.items.iter().any(|item| item.source == ItemSource::Rss);
        if rss_degraded {
            notice = Some(
                "RSS degraded mode: scores and comment parent metadata are unavailable".to_owned(),
            );
        }

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
            if rss_degraded && !fully_known && relevant_len >= page_limit as usize {
                status = "gap".to_owned();
                notice = Some(
                    "RSS degraded mode: feed page was full and no pagination cursor is available"
                        .to_owned(),
                );
            }
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
        notice,
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

fn backfill_options(command: &SyncCommand) -> EffectiveSyncOptions {
    let budget = command.budget.unwrap_or(DEFAULT_BACKFILL_BUDGET).max(1) as usize;
    let page_cap = command.pages.unwrap_or(DEFAULT_BACKFILL_PAGE_CAP).max(1) as usize;
    EffectiveSyncOptions {
        budget,
        page_cap,
        refresh: false,
    }
}

fn refresh_request_limit(options: EffectiveSyncOptions) -> usize {
    options.budget.min(options.page_cap.saturating_mul(100))
}

fn backfill_cutoff(days: u32) -> Result<i64> {
    if days == 0 {
        anyhow::bail!("--days must be greater than zero for backfill");
    }
    let seconds = i64::from(days)
        .checked_mul(24 * 60 * 60)
        .context("--days value is too large")?;
    store::utc_now()
        .checked_sub(seconds)
        .context("--days value is before supported epoch")
}

fn backfill_listing_limit() -> u32 {
    BACKFILL_LISTING_LIMIT
}

fn post_target(item: &RedditItem) -> String {
    item.canonical_permalink()
        .unwrap_or_else(|| format!("https://www.reddit.com/comments/{}/", item.id))
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

    #[test]
    fn backfill_options_are_conservative_and_explicit() {
        let defaults = backfill_options(&command(None, None, false, false));
        assert_eq!(defaults.budget, DEFAULT_BACKFILL_BUDGET as usize);
        assert_eq!(defaults.page_cap, DEFAULT_BACKFILL_PAGE_CAP as usize);
        assert!(!defaults.refresh);

        let overrides = backfill_options(&command(Some(25), Some(4), false, false));
        assert_eq!(overrides.budget, 25);
        assert_eq!(overrides.page_cap, 4);
        assert!(!overrides.refresh);
    }

    #[test]
    fn backfill_cutoff_rejects_zero_days() {
        assert!(backfill_cutoff(0).is_err());
        let now = store::utc_now();
        let cutoff = backfill_cutoff(1).unwrap();
        assert!((now - 86401..=now - 86399).contains(&cutoff));
    }

    #[test]
    fn backfill_listing_limit_is_not_thread_budget_limited() {
        assert_eq!(backfill_listing_limit(), 100);
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
            backfill: None,
            days: None,
            max_requests: None,
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
