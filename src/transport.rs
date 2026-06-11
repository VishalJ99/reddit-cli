use crate::{
    cli::{BrowseCommand, SearchCommand, SubCommand, ThreadCommand, UserCommand},
    config::Config,
    error::RdtError,
    model::{ListingPage, MoreStub, RedditItem, ThreadView},
    parse,
    resolve::ThreadAccumulator,
};
use anyhow::{Context, Result};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;
use wreq::header::{ACCEPT, COOKIE};
use wreq_util::Emulation;

const BASE: &str = "https://www.reddit.com";
const USER_AGENT: &str = concat!(
    "reddit-cli/",
    env!("CARGO_PKG_VERSION"),
    " by u/local-readonly"
);

pub struct RedditClient {
    http: wreq::Client,
    cookie: Option<String>,
    force_anon: bool,
    force_rss: bool,
    fresh: bool,
    request_delay: Duration,
}

impl RedditClient {
    pub fn new(config: &Config, cli: &crate::cli::Cli) -> Result<Self> {
        let http = wreq::Client::builder()
            .emulation(Emulation::Chrome133)
            .user_agent(USER_AGENT)
            .build()
            .context("building wreq client")?;

        Ok(Self {
            http,
            cookie: config.cookie.clone().filter(|_| !cli.anon),
            force_anon: cli.anon,
            force_rss: cli.rss,
            fresh: cli.fresh,
            request_delay: Duration::from_millis(config.request_delay_ms.unwrap_or(1000)),
        })
    }

    pub async fn search(&self, command: &SearchCommand) -> Result<Vec<RedditItem>> {
        let sub = command
            .subreddits
            .iter()
            .map(|subreddit| subreddit_segment(subreddit))
            .collect::<Result<Vec<_>>>()?
            .join("+");
        let json_path = if sub.is_empty() {
            "/search.json".to_owned()
        } else {
            format!("/r/{sub}/search.json")
        };
        let rss_path = if sub.is_empty() {
            "/search.rss".to_owned()
        } else {
            format!("/r/{sub}/search.rss")
        };
        let restrict = if sub.is_empty() { "0" } else { "1" };
        let limit = command.limit.to_string();
        let params = vec![
            ("q", command.query.as_str()),
            ("restrict_sr", restrict),
            ("sort", command.sort.as_reddit()),
            ("t", command.time.as_reddit()),
            ("limit", limit.as_str()),
        ];

        self.fetch_listing_with_fallback(&json_path, &rss_path, &params)
            .await
    }

    pub async fn browse(&self, command: &BrowseCommand) -> Result<Vec<RedditItem>> {
        let sort = command.sort.as_reddit();
        let subreddit = subreddit_segment(&command.sub)?;
        let json_path = format!("/r/{subreddit}/{sort}.json");
        let rss_path = format!("/r/{subreddit}/{sort}.rss");
        let mut owned = vec![
            ("limit".to_owned(), command.limit.to_string()),
            ("t".to_owned(), command.time.as_reddit().to_owned()),
        ];
        if let Some(after) = &command.after {
            owned.push(("after".to_owned(), after.clone()));
        }
        let params = owned
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();

        self.fetch_listing_with_fallback(&json_path, &rss_path, &params)
            .await
    }

    pub async fn sync_listing_page(
        &self,
        subreddit: &str,
        listing: &str,
        limit: u32,
        after: Option<&str>,
    ) -> Result<ListingPage> {
        let subreddit = subreddit_segment(subreddit)?;
        let listing = listing_segment(listing)?;
        let path = format!("/r/{subreddit}/{listing}.json");
        let limit = limit.clamp(1, 100).to_string();
        let mut owned = vec![("limit".to_owned(), limit)];
        if let Some(after) = after {
            owned.push(("after".to_owned(), after.to_owned()));
        }
        let params = owned
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        let value = self.get_json(&path, &params).await?;
        Ok(parse::parse_listing_page(&value))
    }

    pub async fn info_by_ids(&self, ids: &[String]) -> Result<Vec<RedditItem>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if ids.len() > 100 {
            anyhow::bail!("api/info batch exceeds 100 ids");
        }
        for id in ids {
            validate_fullname(id)?;
        }
        let joined = ids.join(",");
        let params = [("id", joined.as_str())];
        let value = self.get_json("/api/info.json", &params).await?;
        Ok(parse::parse_listing(&value))
    }

    pub async fn subreddits(&self, query: &str) -> Result<Vec<RedditItem>> {
        let params = [("q", query), ("limit", "25")];
        let value = self
            .get_json("/subreddits/search.json", &params)
            .await
            .context("searching subreddits")?;
        Ok(parse::parse_listing(&value))
    }

    pub async fn subreddit_about(&self, name: &str) -> Result<Value> {
        let command = SubCommand {
            name: subreddit_segment(name)?,
        };
        let path = format!("/r/{}/about.json", command.name);
        self.get_json(&path, &[]).await
    }

    pub async fn user(&self, command: &UserCommand) -> Result<Vec<RedditItem>> {
        let username = user_segment(&command.name)?;
        let path = format!("/user/{}/{}.json", username, command.what.as_reddit());
        let limit = command.limit.to_string();
        let params = [("limit", limit.as_str())];
        let value = self.get_json(&path, &params).await?;
        Ok(parse::parse_listing(&value))
    }

    pub async fn thread(&self, command: &ThreadCommand, target: &str) -> Result<ThreadView> {
        let json_path = target_to_path(target, "json")?;
        let rss_path = target_to_path(target, "rss")?;
        if self.force_rss {
            return self.fetch_thread_rss(&rss_path).await;
        }

        let mut owned = vec![
            ("sort".to_owned(), command.sort.as_reddit().to_owned()),
            ("limit".to_owned(), "500".to_owned()),
        ];
        if let Some(depth) = command.depth {
            owned.push(("depth".to_owned(), depth.to_string()));
        }
        let params = owned
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();

        match self.get_json(&json_path, &params).await {
            Ok(value) => {
                if command.all {
                    self.resolve_thread_all(command, value).await
                } else {
                    let mut thread = parse::parse_thread(&value, None);
                    thread.http_requests = 1;
                    Ok(thread)
                }
            }
            Err(error) if is_edge_block(&error) => self.fetch_thread_rss(&rss_path).await,
            Err(error) => Err(error),
        }
    }

    pub async fn comment(&self, url: &str, context: u32) -> Result<ThreadView> {
        let path = target_to_path(url, "json")?;
        let rss_path = target_to_path(url, "rss")?;
        if self.force_rss {
            return self.fetch_thread_rss(&rss_path).await;
        }
        let context = context.to_string();
        let params = [("context", context.as_str())];
        match self.get_json(&path, &params).await {
            Ok(value) => Ok(parse::parse_thread(&value, None)),
            Err(error) if is_edge_block(&error) => self.fetch_thread_rss(&rss_path).await,
            Err(error) => Err(error),
        }
    }

    pub async fn auth_check(&self) -> Result<String> {
        if self.cookie.is_none() {
            return Err(RdtError::CookieMissing.into());
        }
        let value = self.get_json("/api/me.json", &[]).await?;
        let name = value
            .pointer("/data/name")
            .and_then(Value::as_str)
            .or_else(|| value.get("name").and_then(Value::as_str))
            .context("auth check response did not include a username")?;
        Ok(format!("logged in as u/{name}"))
    }

    async fn resolve_thread_all(
        &self,
        command: &ThreadCommand,
        initial: Value,
    ) -> Result<ThreadView> {
        let mut accumulator = ThreadAccumulator::new(parse::parse_thread_capture(&initial));
        let mut http_requests = 1usize;
        let max_requests = (command.max_requests as usize).max(1);
        let mut truncated = false;

        'resolve: while let Some(stub) = accumulator.pop_stub() {
            if http_requests >= max_requests {
                accumulator.keep_unresolved(stub);
                truncated = true;
                break;
            }

            if stub.children.is_empty() {
                let Some(parent_short_id) = stub.continue_parent_short_id() else {
                    accumulator.keep_unresolved(stub);
                    continue;
                };
                let permalink = accumulator
                    .post_permalink()
                    .context("cannot fetch continue-thread subtree without post permalink")?;
                let permalink_path = target_to_path(&permalink, "json")?;
                let subtree_path = thread_subtree_path(&permalink_path, parent_short_id);
                let value = self.get_thread_json_path(&subtree_path, command).await?;
                http_requests += 1;
                accumulator.add_capture(parse::parse_thread_capture(&value));
                continue;
            }

            let link_id = accumulator
                .post_fullname()
                .or_else(|| stub.post_id.as_ref().map(|post_id| format!("t3_{post_id}")))
                .context("cannot expand morechildren without post id")?;
            for start in (0..stub.children.len()).step_by(100) {
                if http_requests >= max_requests {
                    accumulator.keep_unresolved(stub_with_children(&stub, start));
                    truncated = true;
                    break 'resolve;
                }

                let end = (start + 100).min(stub.children.len());
                let value = self
                    .morechildren_json(&link_id, &stub.children[start..end], command)
                    .await?;
                http_requests += 1;
                let post_id = accumulator.post_id().map(ToOwned::to_owned);
                accumulator.add_capture(parse::parse_morechildren(&value, post_id.as_deref()));
            }
        }

        Ok(accumulator.into_view(http_requests, truncated))
    }

    async fn get_thread_json_path(&self, path: &str, command: &ThreadCommand) -> Result<Value> {
        let mut owned = vec![
            ("sort".to_owned(), command.sort.as_reddit().to_owned()),
            ("limit".to_owned(), "500".to_owned()),
        ];
        if let Some(depth) = command.depth {
            owned.push(("depth".to_owned(), depth.to_string()));
        }
        let params = owned
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        self.get_json(path, &params).await
    }

    async fn morechildren_json(
        &self,
        link_id: &str,
        children: &[String],
        command: &ThreadCommand,
    ) -> Result<Value> {
        let children = children.join(",");
        let params = [
            ("api_type", "json"),
            ("link_id", link_id),
            ("children", children.as_str()),
            ("sort", command.sort.as_reddit()),
        ];
        self.get_json("/api/morechildren.json", &params).await
    }

    async fn fetch_listing_with_fallback(
        &self,
        json_path: &str,
        rss_path: &str,
        params: &[(&str, &str)],
    ) -> Result<Vec<RedditItem>> {
        if self.force_rss {
            return self.fetch_rss_listing(rss_path, params).await;
        }

        match self.get_json(json_path, params).await {
            Ok(value) => Ok(parse::parse_listing(&value)),
            Err(error) if is_edge_block(&error) => self.fetch_rss_listing(rss_path, params).await,
            Err(error) => Err(error),
        }
    }

    async fn fetch_rss_listing(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<Vec<RedditItem>> {
        eprintln!("warning: RSS degraded mode; scores and some metadata are unavailable");
        let text = self.get_text(path, params, true).await?;
        let mut items = parse::parse_atom_entries(&text)?;
        for (index, item) in items.iter_mut().enumerate() {
            item.index = Some(index + 1);
        }
        Ok(items)
    }

    async fn fetch_thread_rss(&self, path: &str) -> Result<ThreadView> {
        eprintln!(
            "warning: RSS degraded mode; comments are flat and scores/tree metadata are unavailable"
        );
        let text = self.get_text(path, &[], true).await?;
        let entries = parse::parse_atom_entries(&text)?;
        let post = entries.first().cloned();
        let comments = entries
            .into_iter()
            .enumerate()
            .filter_map(|(index, mut item)| {
                if index == 0 {
                    None
                } else {
                    item.kind = crate::model::ItemKind::Comment;
                    Some(item)
                }
            })
            .collect();
        Ok(ThreadView {
            post,
            comments,
            more_stubs: 0,
            degraded: true,
            truncated: false,
            http_requests: 1,
            notice: Some(
                "RSS degraded mode: comments are flat and scores/tree metadata are unavailable"
                    .to_owned(),
            ),
        })
    }

    async fn get_json(&self, path: &str, params: &[(&str, &str)]) -> Result<Value> {
        let mut owned = params
            .iter()
            .map(|(key, value)| (*key, *value))
            .collect::<Vec<_>>();
        owned.push(("raw_json", "1"));
        let text = self.get_text(path, &owned, false).await?;
        serde_json::from_str(&text).context("parsing reddit JSON")
    }

    async fn get_text(&self, path: &str, params: &[(&str, &str)], rss: bool) -> Result<String> {
        let url = build_url(path, params)?;
        self.pace().await;
        match self.get_text_once(&url, rss).await {
            Ok(text) => Ok(text),
            Err(error) if is_retryable_rate_limit(&error) => {
                self.sleep_retry_after(&error).await;
                self.get_text_once(&url, rss)
                    .await
                    .map_err(|_| RdtError::RateLimited { url }.into())
            }
            Err(error) => Err(error),
        }
    }

    async fn get_text_once(&self, url: &str, rss: bool) -> Result<String> {
        let mut request = self.http.get(url).header(
            ACCEPT,
            if rss {
                "application/atom+xml, application/rss+xml, text/xml;q=0.9, */*;q=0.8"
            } else {
                "application/json, text/plain;q=0.9, */*;q=0.8"
            },
        );

        if !rss
            && !self.force_anon
            && let Some(cookie) = &self.cookie
        {
            request = request.header(COOKIE, cookie_header(cookie));
        }
        if self.fresh {
            request = request.header("Cache-Control", "no-cache");
        }

        let response = request.send().await?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let text = response.text().await?;

        if status.as_u16() == 403 && looks_like_block_page(&text) {
            return Err(RdtError::EdgeBlocked.into());
        }
        if status.as_u16() == 429 {
            return Err(RdtError::HttpStatus {
                status: status.as_u16(),
                url: format!("{url} retry_after={}", retry_after.unwrap_or(1)),
                body: text.chars().take(240).collect(),
            }
            .into());
        }
        if !status.is_success() {
            return Err(RdtError::HttpStatus {
                status: status.as_u16(),
                url: url.to_owned(),
                body: text.chars().take(240).collect(),
            }
            .into());
        }

        Ok(text)
    }

    async fn pace(&self) {
        if self.request_delay.is_zero() {
            return;
        }
        tokio::time::sleep(jitter(self.request_delay)).await;
    }

    async fn sleep_retry_after(&self, error: &anyhow::Error) {
        let seconds = error
            .downcast_ref::<RdtError>()
            .and_then(|error| match error {
                RdtError::HttpStatus { url, .. } => url
                    .rsplit("retry_after=")
                    .next()
                    .and_then(|value| value.parse::<u64>().ok()),
                _ => None,
            })
            .unwrap_or(1)
            .min(60);
        tokio::time::sleep(Duration::from_secs(seconds)).await;
    }
}

fn is_edge_block(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<RdtError>()
        .is_some_and(|error| matches!(error, RdtError::EdgeBlocked))
}

fn is_retryable_rate_limit(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<RdtError>()
        .is_some_and(|error| matches!(error, RdtError::HttpStatus { status: 429, .. }))
}

fn build_url(path: &str, params: &[(&str, &str)]) -> Result<String> {
    let mut url = if path.starts_with("http://") || path.starts_with("https://") {
        let parsed = Url::parse(path)?;
        validate_reddit_host(&parsed)?;
        parsed.to_string()
    } else {
        format!("{BASE}{path}")
    };

    if params.is_empty() {
        return Ok(url);
    }

    let joiner = if url.contains('?') { '&' } else { '?' };
    let query = params
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    url.push(joiner);
    url.push_str(&query);
    Ok(url)
}

fn cookie_header(cookie: &str) -> String {
    if cookie.contains("reddit_session=") {
        cookie.to_owned()
    } else {
        format!("reddit_session={cookie}")
    }
}

fn looks_like_block_page(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("<html") && lower.contains("blocked") && lower.contains("reddit")
}

fn subreddit_segment(input: &str) -> Result<String> {
    let subreddit = input
        .trim()
        .trim_start_matches("r/")
        .trim_start_matches("/r/")
        .to_owned();
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

fn user_segment(input: &str) -> Result<String> {
    let username = input
        .trim()
        .trim_start_matches("u/")
        .trim_start_matches("/u/");
    if (3..=20).contains(&username.len())
        && username
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        Ok(username.to_owned())
    } else {
        anyhow::bail!("invalid Reddit username: {input}")
    }
}

fn listing_segment(input: &str) -> Result<&str> {
    match input {
        "new" | "comments" => Ok(input),
        _ => anyhow::bail!("invalid listing for sync: {input}"),
    }
}

fn validate_fullname(input: &str) -> Result<()> {
    let Some((kind, id)) = input.split_once('_') else {
        anyhow::bail!("invalid Reddit fullname: {input}");
    };
    let valid_kind = matches!(kind, "t1" | "t2" | "t3" | "t5");
    if valid_kind && is_probable_base36_id(id) {
        Ok(())
    } else {
        anyhow::bail!("invalid Reddit fullname: {input}")
    }
}

fn thread_subtree_path(base_json_path: &str, comment_id: &str) -> String {
    let base = base_json_path
        .trim_end_matches(".json")
        .trim_end_matches('/');
    format!("{base}/{comment_id}.json")
}

fn stub_with_children(stub: &MoreStub, start: usize) -> MoreStub {
    let mut out = stub.clone();
    out.children = stub.children[start..].to_vec();
    out.count = out.children.len().max(1);
    out
}

fn target_to_path(target: &str, ext: &str) -> Result<String> {
    let clean = target.trim();
    let clean = clean.strip_prefix("t3_").unwrap_or(clean);

    if is_probable_base36_id(clean) {
        return Ok(format!("/comments/{clean}.{ext}"));
    }

    let without_query = clean.split('?').next().unwrap_or(clean);
    let path = if without_query.starts_with("http://") || without_query.starts_with("https://") {
        let parsed = Url::parse(without_query)?;
        validate_reddit_host(&parsed)?;
        if parsed.host_str() == Some("redd.it") {
            let id = parsed.path().trim_matches('/');
            return Ok(format!("/comments/{id}.{ext}"));
        }
        if parsed.path().is_empty() {
            "/".to_owned()
        } else {
            parsed.path().to_owned()
        }
    } else if without_query.starts_with('/') {
        without_query.to_owned()
    } else {
        format!("/{without_query}")
    };

    Ok(append_extension(&path, ext))
}

fn validate_reddit_host(url: &Url) -> Result<()> {
    match url.host_str() {
        Some("reddit.com" | "www.reddit.com" | "old.reddit.com" | "redd.it") => Ok(()),
        _ => Err(RdtError::NonRedditUrl(url.to_string()).into()),
    }
}

fn append_extension(path: &str, ext: &str) -> String {
    let suffix = format!(".{ext}");
    if path.ends_with(&suffix) {
        return path.to_owned();
    }
    let trimmed = path.trim_end_matches('/');
    format!("{trimmed}.{ext}")
}

fn is_probable_base36_id(input: &str) -> bool {
    (4..=12).contains(&input.len()) && input.chars().all(|ch| ch.is_ascii_alphanumeric())
}

fn jitter(delay: Duration) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    let pct = 70 + (nanos % 61) as u64;
    Duration::from_millis((delay.as_millis() as u64).saturating_mul(pct) / 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_bare_post_ids_to_comments_endpoint() {
        assert_eq!(
            target_to_path("t3_abc123", "json").unwrap(),
            "/comments/abc123.json"
        );
        assert_eq!(
            target_to_path("https://redd.it/abc123", "rss").unwrap(),
            "/comments/abc123.rss"
        );
    }

    #[test]
    fn appends_extension_to_reddit_urls() {
        assert_eq!(
            target_to_path(
                "https://www.reddit.com/r/rust/comments/abc/title/?utm=1",
                "json"
            )
            .unwrap(),
            "/r/rust/comments/abc/title.json"
        );
    }

    #[test]
    fn builds_continue_thread_subtree_paths() {
        assert_eq!(
            thread_subtree_path("/r/rust/comments/abc/title.json", "def"),
            "/r/rust/comments/abc/title/def.json"
        );
        assert_eq!(
            thread_subtree_path("/comments/abc.json", "def"),
            "/comments/abc/def.json"
        );
    }

    #[test]
    fn rejects_lookalike_hosts() {
        let error = target_to_path("https://www.reddit.com.evil.test/r/rust", "json")
            .expect_err("lookalike host should be rejected")
            .to_string();
        assert!(error.contains("refusing non-Reddit URL"));
    }

    #[test]
    fn rejects_path_like_subreddit_names() {
        let error = subreddit_segment("rust/about").unwrap_err().to_string();
        assert!(error.contains("invalid subreddit"));
    }

    #[test]
    fn wraps_cookie_values() {
        assert_eq!(cookie_header("abc"), "reddit_session=abc");
        assert_eq!(cookie_header("reddit_session=abc"), "reddit_session=abc");
    }

    #[test]
    fn validates_api_info_fullnames() {
        assert!(validate_fullname("t3_abc123").is_ok());
        assert!(validate_fullname("abc123").is_err());
        assert!(validate_fullname("t9_abc123").is_err());
        assert!(validate_fullname("t3_not/valid").is_err());
    }
}
