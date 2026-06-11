use crate::model::{
    ItemKind, ItemSource, ListingPage, RedditItem, ThreadView, canonical_permalink,
};
use anyhow::Result;
use quick_xml::{Reader, events::Event};
use serde_json::Value;

pub fn parse_listing(value: &Value) -> Vec<RedditItem> {
    parse_listing_page(value).items
}

pub fn parse_listing_page(value: &Value) -> ListingPage {
    let items = value
        .pointer("/data/children")
        .and_then(Value::as_array)
        .map(|children| {
            children
                .iter()
                .filter_map(|thing| parse_thing(thing, 0, ItemSource::Json))
                .collect()
        })
        .unwrap_or_default();

    ListingPage {
        items,
        after: string_field(&value["data"], "after"),
        before: string_field(&value["data"], "before"),
    }
}

pub fn parse_thread(value: &Value, notice: Option<String>) -> ThreadView {
    let post = value
        .as_array()
        .and_then(|array| array.first())
        .and_then(|listing| parse_listing(listing).into_iter().next());

    let mut comments = Vec::new();
    let mut more_stubs = 0;
    if let Some(comment_listing) = value.as_array().and_then(|array| array.get(1)) {
        parse_comment_listing(comment_listing, 0, &mut comments, &mut more_stubs);
    }

    ThreadView {
        post,
        comments,
        more_stubs,
        degraded: false,
        notice,
    }
}

pub fn parse_atom_entries(xml: &str) -> Result<Vec<RedditItem>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut entries = Vec::new();
    let mut in_entry = false;
    let mut current_tag: Option<String> = None;
    let mut entry = AtomEntry::default();

    loop {
        match reader.read_event()? {
            Event::Start(start) => {
                let name = local_name(start.name().as_ref()).to_owned();
                if name == "entry" || name == "item" {
                    in_entry = true;
                    entry = AtomEntry::default();
                } else if in_entry {
                    current_tag = Some(name);
                }

                if in_entry && local_name(start.name().as_ref()) == "link" {
                    for attr in start.attributes().flatten() {
                        if local_name(attr.key.as_ref()) == "href" {
                            entry.link = Some(String::from_utf8_lossy(&attr.value).into_owned());
                        }
                    }
                }
            }
            Event::Empty(empty) => {
                if in_entry && local_name(empty.name().as_ref()) == "link" {
                    for attr in empty.attributes().flatten() {
                        if local_name(attr.key.as_ref()) == "href" {
                            entry.link = Some(String::from_utf8_lossy(&attr.value).into_owned());
                        }
                    }
                }
            }
            Event::Text(text) => {
                if in_entry {
                    let decoded = text.decode()?.into_owned();
                    match current_tag.as_deref() {
                        Some("title") => entry.title = Some(decoded),
                        Some("id") | Some("guid") => entry.id = Some(decoded),
                        Some("name") => entry.author = Some(decoded),
                        Some("content") | Some("summary") | Some("description") => {
                            if entry.body.is_none() {
                                entry.body = Some(decoded);
                            }
                        }
                        Some("updated") | Some("published") | Some("pubDate") => {
                            entry.updated = Some(decoded);
                        }
                        _ => {}
                    }
                }
            }
            Event::End(end) => {
                let name = local_name(end.name().as_ref());
                if name == "entry" || name == "item" {
                    in_entry = false;
                    entries.push(entry.to_item(entries.len()));
                    entry = AtomEntry::default();
                }
                current_tag = None;
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(entries)
}

fn parse_comment_listing(
    value: &Value,
    depth: usize,
    comments: &mut Vec<RedditItem>,
    more_stubs: &mut usize,
) {
    let Some(children) = value.pointer("/data/children").and_then(Value::as_array) else {
        return;
    };

    for thing in children {
        if thing.get("kind").and_then(Value::as_str) == Some("more") {
            *more_stubs += thing
                .pointer("/data/children")
                .and_then(Value::as_array)
                .map_or(1, |children| children.len().max(1));
            continue;
        }

        if let Some(item) = parse_thing(thing, depth, ItemSource::Json) {
            comments.push(item);
        }

        let replies = thing.pointer("/data/replies");
        if let Some(reply_listing) = replies.filter(|value| value.is_object()) {
            parse_comment_listing(reply_listing, depth + 1, comments, more_stubs);
        }
    }
}

fn parse_thing(thing: &Value, depth: usize, source: ItemSource) -> Option<RedditItem> {
    let kind_raw = thing
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let data = thing.get("data")?;
    let kind = match kind_raw {
        "t3" => ItemKind::Post,
        "t1" => ItemKind::Comment,
        "t5" => ItemKind::Subreddit,
        "t2" => ItemKind::User,
        "more" => ItemKind::More,
        _ => ItemKind::Unknown,
    };

    let id = string_field(data, "id")
        .or_else(|| string_field(data, "name"))
        .unwrap_or_default();
    let fullname = string_field(data, "name").unwrap_or_else(|| {
        if id.is_empty() {
            kind_raw.to_owned()
        } else {
            format!("{kind_raw}_{id}")
        }
    });

    let permalink = string_field(data, "permalink").map(|value| canonical_permalink(&value));
    let link_id = string_field(data, "link_id");
    let post_id = link_id
        .as_deref()
        .and_then(|value| value.strip_prefix("t3_"))
        .map(ToOwned::to_owned)
        .or_else(|| {
            if kind == ItemKind::Post {
                Some(id.clone())
            } else {
                None
            }
        });

    Some(RedditItem {
        index: None,
        kind,
        id,
        fullname,
        parent_id: string_field(data, "parent_id"),
        post_id,
        title: string_field(data, "title").or_else(|| string_field(data, "display_name_prefixed")),
        author: string_field(data, "author").or_else(|| string_field(data, "name")),
        subreddit: string_field(data, "subreddit"),
        body: string_field(data, "body").or_else(|| string_field(data, "selftext")),
        flair: string_field(data, "link_flair_text"),
        is_self: bool_field(data, "is_self"),
        over_18: bool_field(data, "over_18"),
        score: int_field(data, "score"),
        upvote_ratio: data.get("upvote_ratio").and_then(Value::as_f64),
        num_comments: int_field(data, "num_comments"),
        created_utc: data.get("created_utc").and_then(Value::as_f64),
        edited_utc: edited_field(data),
        permalink,
        url: string_field(data, "url"),
        depth,
        source,
    })
}

fn string_field(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn int_field(data: &Value, key: &str) -> Option<i64> {
    data.get(key).and_then(Value::as_i64)
}

fn bool_field(data: &Value, key: &str) -> Option<bool> {
    data.get(key).and_then(Value::as_bool)
}

fn edited_field(data: &Value) -> Option<f64> {
    match data.get("edited") {
        Some(Value::Number(value)) => value.as_f64(),
        _ => None,
    }
}

fn local_name(name: &[u8]) -> String {
    let raw = String::from_utf8_lossy(name);
    raw.rsplit(':').next().unwrap_or(&raw).to_owned()
}

#[derive(Debug, Default)]
struct AtomEntry {
    id: Option<String>,
    title: Option<String>,
    author: Option<String>,
    body: Option<String>,
    link: Option<String>,
    updated: Option<String>,
}

impl AtomEntry {
    fn to_item(&self, index: usize) -> RedditItem {
        let id = self
            .id
            .as_deref()
            .or(self.link.as_deref())
            .unwrap_or_default()
            .to_owned();

        let link = self.link.as_deref().map(canonical_permalink);
        let subreddit = link.as_deref().and_then(extract_subreddit);
        RedditItem {
            index: Some(index + 1),
            kind: ItemKind::Post,
            id: id.clone(),
            fullname: id,
            parent_id: None,
            post_id: None,
            title: self.title.clone(),
            author: self.author.clone(),
            subreddit,
            body: self.body.clone(),
            flair: None,
            is_self: None,
            over_18: None,
            score: None,
            upvote_ratio: None,
            num_comments: None,
            created_utc: None,
            edited_utc: None,
            permalink: link,
            url: self.link.clone(),
            depth: 0,
            source: ItemSource::Rss,
        }
    }
}

fn extract_subreddit(url: &str) -> Option<String> {
    let marker = "/r/";
    let start = url.find(marker)? + marker.len();
    let rest = &url[start..];
    let end = rest.find('/').unwrap_or(rest.len());
    Some(rest[..end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_listing_children() {
        let value = json!({
            "data": {"children": [{
                "kind": "t3",
                "data": {
                    "id": "abc",
                    "name": "t3_abc",
                    "title": "Hello",
                    "author": "alice",
                    "subreddit": "rust",
                    "score": 7,
                    "num_comments": 2,
                    "permalink": "/r/rust/comments/abc/hello/"
                }
            }]}
        });

        let items = parse_listing(&value);
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].canonical_permalink().unwrap(),
            "https://www.reddit.com/r/rust/comments/abc/hello/"
        );
        assert_eq!(items[0].kind, ItemKind::Post);
        assert_eq!(items[0].post_id.as_deref(), Some("abc"));
    }

    #[test]
    fn parses_thread_depth_and_more() {
        let value = json!([
            {"data": {"children": [{"kind": "t3", "data": {"id": "p", "name": "t3_p", "title": "Post"}}]}},
            {"data": {"children": [
                {"kind": "t1", "data": {
                    "id": "c1",
                    "name": "t1_c1",
                    "body": "Root",
                    "replies": {"data": {"children": [
                        {"kind": "t1", "data": {"id": "c2", "name": "t1_c2", "body": "Child", "replies": ""}}
                    ]}}
                }},
                {"kind": "more", "data": {"children": ["x", "y"]}}
            ]}}
        ]);

        let thread = parse_thread(&value, None);
        assert_eq!(thread.comments.len(), 2);
        assert_eq!(thread.comments[1].depth, 1);
        assert_eq!(thread.more_stubs, 2);
    }
}
