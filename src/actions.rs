use crate::{
    config::Paths,
    model::{RedditItem, canonical_permalink},
};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

pub fn write_last(paths: &Paths, items: &mut [RedditItem]) -> Result<()> {
    fs::create_dir_all(&paths.cache_dir)?;
    for (index, item) in items.iter_mut().enumerate() {
        item.index = Some(index + 1);
    }
    let raw = serde_json::to_string_pretty(items)?;
    fs::write(&paths.last_file, raw)?;
    Ok(())
}

pub fn resolve_target(paths: &Paths, target: &str) -> Result<String> {
    if let Ok(index) = target.parse::<usize>() {
        let raw = fs::read_to_string(&paths.last_file)
            .with_context(|| format!("reading {}", paths.last_file.display()))?;
        let items: Vec<RedditItem> = serde_json::from_str(&raw)
            .with_context(|| format!("parsing {}", paths.last_file.display()))?;
        let item = items
            .iter()
            .find(|item| item.index == Some(index))
            .with_context(|| format!("last result index {index} does not exist"))?;
        return item
            .canonical_permalink()
            .with_context(|| format!("last result index {index} has no Reddit permalink"));
    }

    let permalink = canonical_permalink(target);
    validate_reddit_permalink(&permalink)?;
    Ok(permalink)
}

pub fn copy_permalink(target: &str) -> Result<()> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .context("starting pbcopy")?;
    child
        .stdin
        .as_mut()
        .context("opening pbcopy stdin")?
        .write_all(target.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        bail!("pbcopy exited with {status}");
    }
    Ok(())
}

fn validate_reddit_permalink(permalink: &str) -> Result<()> {
    let parsed = url::Url::parse(permalink)?;
    match parsed.host_str() {
        Some("reddit.com" | "www.reddit.com" | "old.reddit.com") => Ok(()),
        _ => bail!("target must be a Reddit permalink"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ItemKind, ItemSource};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn resolves_numbered_results_from_last_file() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rdt-actions-{stamp}"));
        let paths = Paths {
            config_file: root.join("config.toml"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            db_file: root.join("data/rdt.db"),
            last_file: root.join("cache/last.json"),
        };
        let mut items = vec![RedditItem {
            index: None,
            kind: ItemKind::Post,
            id: "abc".to_owned(),
            fullname: "t3_abc".to_owned(),
            title: Some("hello".to_owned()),
            author: None,
            subreddit: Some("rust".to_owned()),
            body: None,
            score: None,
            num_comments: None,
            created_utc: None,
            permalink: Some("/r/rust/comments/abc/hello/".to_owned()),
            url: None,
            depth: 0,
            source: ItemSource::Json,
        }];

        write_last(&paths, &mut items).unwrap();
        assert_eq!(
            resolve_target(&paths, "1").unwrap(),
            "https://www.reddit.com/r/rust/comments/abc/hello/"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_external_targets() {
        let root = std::env::temp_dir().join("rdt-actions-rejects-external");
        let paths = Paths {
            config_file: root.join("config.toml"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            db_file: root.join("data/rdt.db"),
            last_file: root.join("cache/last.json"),
        };
        let error = resolve_target(&paths, "https://evil.example/path")
            .expect_err("external URLs should be rejected")
            .to_string();
        assert!(error.contains("target must be a Reddit permalink"));
    }
}
