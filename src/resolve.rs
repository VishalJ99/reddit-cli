use crate::model::{MoreStub, RedditItem, ThreadCapture, ThreadView};
use std::collections::{HashMap, HashSet, VecDeque};

pub struct ThreadAccumulator {
    post: Option<RedditItem>,
    comments: Vec<RedditItem>,
    by_id: HashMap<String, usize>,
    queue: VecDeque<MoreStub>,
    unresolved: Vec<MoreStub>,
}

impl ThreadAccumulator {
    pub fn new(capture: ThreadCapture) -> Self {
        let mut out = Self {
            post: capture.post,
            comments: Vec::new(),
            by_id: HashMap::new(),
            queue: VecDeque::new(),
            unresolved: Vec::new(),
        };
        out.add_capture(ThreadCapture {
            post: None,
            comments: capture.comments,
            more_stubs: capture.more_stubs,
        });
        out
    }

    pub fn post_id(&self) -> Option<&str> {
        self.post.as_ref().map(|post| post.id.as_str())
    }

    pub fn post_fullname(&self) -> Option<String> {
        self.post.as_ref().map(|post| post.fullname.clone())
    }

    pub fn post_permalink(&self) -> Option<String> {
        self.post.as_ref().and_then(|post| post.permalink.clone())
    }

    pub fn add_capture(&mut self, capture: ThreadCapture) {
        if self.post.is_none() {
            self.post = capture.post;
        }

        for comment in capture.comments {
            self.add_comment(comment);
        }
        for stub in capture.more_stubs {
            self.queue.push_back(stub);
        }
    }

    pub fn pop_stub(&mut self) -> Option<MoreStub> {
        self.queue.pop_front()
    }

    pub fn keep_unresolved(&mut self, stub: MoreStub) {
        self.unresolved.push(stub);
    }

    pub fn has_pending_stubs(&self) -> bool {
        !self.queue.is_empty() || !self.unresolved.is_empty()
    }

    pub fn unresolved_count(&self) -> usize {
        self.queue
            .iter()
            .chain(self.unresolved.iter())
            .map(MoreStub::unresolved_count)
            .sum()
    }

    pub fn into_view(mut self, http_requests: usize, truncated: bool) -> ThreadView {
        let more_stubs = self.unresolved_count();
        let comments = self.ordered_comments();
        let notice = if truncated {
            Some(format!(
                "thread truncated: {more_stubs} hidden comment(s) not fetched before --max-requests"
            ))
        } else if more_stubs > 0 {
            Some(format!(
                "thread incomplete: {more_stubs} hidden comment(s) could not be resolved"
            ))
        } else {
            None
        };

        ThreadView {
            post: self.post,
            comments,
            more_stubs,
            degraded: false,
            truncated,
            http_requests,
            notice,
        }
    }

    fn add_comment(&mut self, comment: RedditItem) {
        if let Some(index) = self.by_id.get(&comment.id).copied() {
            self.comments[index] = comment;
        } else {
            self.by_id.insert(comment.id.clone(), self.comments.len());
            self.comments.push(comment);
        }
    }

    fn ordered_comments(&mut self) -> Vec<RedditItem> {
        let Some(post_id) = self.post_id().map(|post_id| format!("t3_{post_id}")) else {
            return self.comments.clone();
        };

        let mut children_by_parent: HashMap<String, Vec<usize>> = HashMap::new();
        let known_fullnames = self
            .comments
            .iter()
            .map(|comment| comment.fullname.clone())
            .collect::<HashSet<_>>();
        let mut orphan_indexes = Vec::new();

        for (index, comment) in self.comments.iter().enumerate() {
            match comment.parent_id.as_deref() {
                Some(parent) if parent == post_id || known_fullnames.contains(parent) => {
                    children_by_parent
                        .entry(parent.to_owned())
                        .or_default()
                        .push(index);
                }
                _ => orphan_indexes.push(index),
            }
        }

        let mut out = Vec::new();
        let mut visited = HashSet::new();
        self.push_children(&post_id, 0, &children_by_parent, &mut visited, &mut out);

        for index in orphan_indexes {
            if visited.insert(index) {
                let mut comment = self.comments[index].clone();
                comment.depth = 0;
                out.push(comment);
            }
        }

        out
    }

    fn push_children(
        &self,
        parent: &str,
        depth: usize,
        children_by_parent: &HashMap<String, Vec<usize>>,
        visited: &mut HashSet<usize>,
        out: &mut Vec<RedditItem>,
    ) {
        let Some(children) = children_by_parent.get(parent) else {
            return;
        };

        for index in children {
            if !visited.insert(*index) {
                continue;
            }
            let mut comment = self.comments[*index].clone();
            comment.depth = depth;
            let fullname = comment.fullname.clone();
            out.push(comment);
            self.push_children(&fullname, depth + 1, children_by_parent, visited, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ItemKind, ItemSource};

    #[test]
    fn orders_comments_by_parent_and_recomputes_depth() {
        let root = comment("a", "t3_post");
        let child = comment("b", "t1_a");
        let capture = ThreadCapture {
            post: Some(post("post")),
            comments: vec![child, root],
            more_stubs: Vec::new(),
        };

        let view = ThreadAccumulator::new(capture).into_view(1, false);
        assert_eq!(view.comments[0].id, "a");
        assert_eq!(view.comments[0].depth, 0);
        assert_eq!(view.comments[1].id, "b");
        assert_eq!(view.comments[1].depth, 1);
    }

    #[test]
    fn reports_unresolved_truncation_count() {
        let capture = ThreadCapture {
            post: Some(post("post")),
            comments: Vec::new(),
            more_stubs: vec![MoreStub {
                id: "more".to_owned(),
                parent_id: Some("t3_post".to_owned()),
                post_id: Some("post".to_owned()),
                children: vec!["a".to_owned(), "b".to_owned()],
                count: 2,
                depth: 0,
            }],
        };

        let view = ThreadAccumulator::new(capture).into_view(1, true);
        assert!(view.truncated);
        assert_eq!(view.more_stubs, 2);
        assert!(view.notice.unwrap().contains("thread truncated"));
    }

    fn post(id: &str) -> RedditItem {
        RedditItem {
            index: None,
            kind: ItemKind::Post,
            id: id.to_owned(),
            fullname: format!("t3_{id}"),
            parent_id: None,
            post_id: Some(id.to_owned()),
            title: Some("post".to_owned()),
            author: Some("alice".to_owned()),
            subreddit: Some("rust".to_owned()),
            body: None,
            flair: None,
            is_self: Some(true),
            over_18: Some(false),
            score: None,
            upvote_ratio: None,
            num_comments: None,
            created_utc: None,
            edited_utc: None,
            permalink: Some(format!("/r/rust/comments/{id}/post/")),
            url: None,
            depth: 0,
            source: ItemSource::Json,
        }
    }

    fn comment(id: &str, parent_id: &str) -> RedditItem {
        RedditItem {
            index: None,
            kind: ItemKind::Comment,
            id: id.to_owned(),
            fullname: format!("t1_{id}"),
            parent_id: Some(parent_id.to_owned()),
            post_id: Some("post".to_owned()),
            title: None,
            author: Some("bob".to_owned()),
            subreddit: Some("rust".to_owned()),
            body: Some(id.to_owned()),
            flair: None,
            is_self: None,
            over_18: None,
            score: None,
            upvote_ratio: None,
            num_comments: None,
            created_utc: None,
            edited_utc: None,
            permalink: Some(format!("/r/rust/comments/post/title/{id}/")),
            url: None,
            depth: 0,
            source: ItemSource::Json,
        }
    }
}
