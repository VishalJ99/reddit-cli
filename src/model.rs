use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Post,
    Comment,
    Subreddit,
    User,
    More,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RedditItem {
    pub index: Option<usize>,
    pub kind: ItemKind,
    pub id: String,
    pub fullname: String,
    pub parent_id: Option<String>,
    pub post_id: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub subreddit: Option<String>,
    pub body: Option<String>,
    pub flair: Option<String>,
    pub is_self: Option<bool>,
    pub over_18: Option<bool>,
    pub score: Option<i64>,
    pub upvote_ratio: Option<f64>,
    pub num_comments: Option<i64>,
    pub created_utc: Option<f64>,
    pub edited_utc: Option<f64>,
    pub permalink: Option<String>,
    pub url: Option<String>,
    pub depth: usize,
    pub source: ItemSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListingPage {
    pub items: Vec<RedditItem>,
    pub after: Option<String>,
    pub before: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MoreStub {
    pub id: String,
    pub parent_id: Option<String>,
    pub post_id: Option<String>,
    pub children: Vec<String>,
    pub count: usize,
    pub depth: usize,
}

impl MoreStub {
    pub fn unresolved_count(&self) -> usize {
        self.children.len().max(self.count).max(1)
    }

    pub fn continue_parent_short_id(&self) -> Option<&str> {
        if self.children.is_empty() {
            self.parent_id
                .as_deref()
                .and_then(|parent| parent.strip_prefix("t1_"))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreadCapture {
    pub post: Option<RedditItem>,
    pub comments: Vec<RedditItem>,
    pub more_stubs: Vec<MoreStub>,
}

impl ThreadCapture {
    pub fn more_stub_count(&self) -> usize {
        self.more_stubs.iter().map(MoreStub::unresolved_count).sum()
    }
}

impl RedditItem {
    pub fn canonical_permalink(&self) -> Option<String> {
        self.permalink.as_deref().map(canonical_permalink)
    }

    pub fn display_title(&self) -> &str {
        self.title
            .as_deref()
            .or(self.body.as_deref())
            .unwrap_or("(untitled)")
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ItemSource {
    Json,
    Rss,
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreadView {
    pub post: Option<RedditItem>,
    pub comments: Vec<RedditItem>,
    pub more_stubs: usize,
    pub degraded: bool,
    pub truncated: bool,
    pub http_requests: usize,
    pub notice: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum DbValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Bool(bool),
}

pub type DbRow = serde_json::Map<String, serde_json::Value>;

pub fn canonical_permalink(input: &str) -> String {
    let without_query = input.split('?').next().unwrap_or(input);
    if without_query.starts_with("https://www.reddit.com/") {
        without_query.to_owned()
    } else if without_query.starts_with("https://old.reddit.com/") {
        without_query.replacen("https://old.reddit.com/", "https://www.reddit.com/", 1)
    } else if without_query.starts_with("http://") || without_query.starts_with("https://") {
        without_query.to_owned()
    } else if without_query.starts_with('/') {
        format!("https://www.reddit.com{without_query}")
    } else {
        format!("https://www.reddit.com/{without_query}")
    }
}
