use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{env, fs, path::PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub db_file: PathBuf,
    pub last_file: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Self> {
        let dirs = ProjectDirs::from("com", "dross", "rdt")
            .context("could not resolve XDG project directories")?;
        let config_dir = dirs.config_dir().to_path_buf();
        let data_dir = dirs.data_dir().to_path_buf();
        let cache_dir = dirs.cache_dir().to_path_buf();

        Ok(Self {
            config_file: config_dir.join("config.toml"),
            db_file: data_dir.join("rdt.db"),
            last_file: cache_dir.join("last.json"),
            data_dir,
            cache_dir,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Config {
    pub cookie: Option<String>,
    pub cookie_file: Option<PathBuf>,
    pub request_delay_ms: Option<u64>,
    pub cache_ttl_secs: Option<u64>,
    pub page_cap: Option<u32>,
    pub sync_budget: Option<u32>,
    pub sync_refresh: Option<bool>,
}

impl Config {
    pub fn load(paths: &Paths) -> Result<Self> {
        let mut config = Self::default();

        for config_file in config_candidates(paths)
            .into_iter()
            .filter(|path| path.exists())
        {
            let raw = fs::read_to_string(&config_file)
                .with_context(|| format!("reading {}", config_file.display()))?;
            let candidate: Self = toml::from_str(&raw)
                .with_context(|| format!("parsing {}", config_file.display()))?;
            config.merge_missing(candidate);
        }

        if let Ok(cookie) = env::var("RDT_COOKIE")
            && !cookie.trim().is_empty()
        {
            config.cookie = Some(cookie);
        }

        if config.cookie.is_none()
            && let Some(cookie_file) = &config.cookie_file
        {
            config.cookie = read_cookie_file(cookie_file);
        }

        Ok(config)
    }

    pub fn clear_cookie(paths: &Paths) -> Result<()> {
        for config_file in config_candidates(paths) {
            if !config_file.exists() {
                continue;
            }
            let raw = fs::read_to_string(&config_file)?;
            let mut value: toml::Table = toml::from_str(&raw)?;
            value.remove("cookie");
            value.remove("cookie_file");
            fs::create_dir_all(config_file.parent().context("config file has no parent")?)?;
            fs::write(&config_file, toml::to_string_pretty(&value)?)?;
        }
        Ok(())
    }

    fn merge_missing(&mut self, other: Self) {
        if self.cookie.is_none() {
            self.cookie = other.cookie;
        }
        if self.cookie_file.is_none() {
            self.cookie_file = other.cookie_file;
        }
        if self.request_delay_ms.is_none() {
            self.request_delay_ms = other.request_delay_ms;
        }
        if self.cache_ttl_secs.is_none() {
            self.cache_ttl_secs = other.cache_ttl_secs;
        }
        if self.page_cap.is_none() {
            self.page_cap = other.page_cap;
        }
        if self.sync_budget.is_none() {
            self.sync_budget = other.sync_budget;
        }
        if self.sync_refresh.is_none() {
            self.sync_refresh = other.sync_refresh;
        }
    }
}

fn config_candidates(paths: &Paths) -> Vec<PathBuf> {
    let mut candidates = vec![paths.config_file.clone()];
    if let Some(home) = env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("rdt")
                .join("config.toml"),
        );
    }
    candidates
}

fn read_cookie_file(path: &PathBuf) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() >= 7 && fields[5] == "reddit_session" {
            return Some(fields[6].to_owned());
        }
    }
    None
}
