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
    pub title: Option<String>,
    pub author: Option<String>,
    pub subreddit: Option<String>,
    pub body: Option<String>,
    pub score: Option<i64>,
    pub num_comments: Option<i64>,
    pub created_utc: Option<f64>,
    pub permalink: Option<String>,
    pub url: Option<String>,
    pub depth: usize,
    pub source: ItemSource,
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
