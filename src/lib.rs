pub mod actions;
pub mod cli;
pub mod config;
pub mod error;
pub mod model;
pub mod parse;
pub mod render;
pub mod resolve;
pub mod store;
pub mod sync;
pub mod transport;

pub use cli::Cli;

use anyhow::Context;
use cli::{AuthCommand, Commands, DbCommand, ThreadCommand, WatchCommand};
use config::{Config, Paths};
use std::time::Duration;
use transport::RedditClient;

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let paths = Paths::resolve()?;
    let config = Config::load(&paths)?;

    match &cli.command {
        Commands::Search(command) => {
            let client = RedditClient::new(&config, &paths, &cli)?;
            if command.local {
                let rows = store::search(&paths, &command.query)?;
                render::print_db_rows(&rows, cli.json)?;
                return Ok(());
            }

            let mut items = client.search(command).await?;
            actions::write_last(&paths, &mut items)?;
            render::print_items(&items, cli.json, cli.no_color)?;
        }
        Commands::Browse(command) => {
            let client = RedditClient::new(&config, &paths, &cli)?;
            let mut items = client.browse(command).await?;
            actions::write_last(&paths, &mut items)?;
            render::print_items(&items, cli.json, cli.no_color)?;
        }
        Commands::Subs(command) => {
            let client = RedditClient::new(&config, &paths, &cli)?;
            let mut items = client.subreddits(&command.query).await?;
            actions::write_last(&paths, &mut items)?;
            render::print_items(&items, cli.json, cli.no_color)?;
        }
        Commands::Sub(command) => {
            let client = RedditClient::new(&config, &paths, &cli)?;
            let value = client.subreddit_about(&command.name).await?;
            render::print_json_or_debug(&value, cli.json)?;
        }
        Commands::User(command) => {
            let client = RedditClient::new(&config, &paths, &cli)?;
            let mut items = client.user(command).await?;
            actions::write_last(&paths, &mut items)?;
            render::print_items(&items, cli.json, cli.no_color)?;
        }
        Commands::Thread(command) => {
            let client = RedditClient::new(&config, &paths, &cli)?;
            let target = actions::resolve_target(&paths, &command.target)?;
            let thread = client.thread(command, &target).await?;
            render::print_thread(&thread, cli.json, cli.no_color)?;
        }
        Commands::Comment(command) => {
            let client = RedditClient::new(&config, &paths, &cli)?;
            let thread = client.comment(&command.url, command.context).await?;
            render::print_thread(&thread, cli.json, cli.no_color)?;
        }
        Commands::Watch(command) => match command {
            WatchCommand::Add { subreddits } => {
                store::watch_add(&paths, subreddits)?;
                println!("added {} watch(es)", subreddits.len());
            }
            WatchCommand::Rm { subreddit } => {
                store::watch_remove(&paths, subreddit)?;
                println!("removed watch for r/{subreddit}");
            }
            WatchCommand::Ls => {
                let rows = store::watch_list(&paths)?;
                render::print_db_rows(&rows, cli.json)?;
            }
        },
        Commands::Sync(command) => {
            if cli.rss {
                anyhow::bail!(
                    "rdt --rss sync is planned but not implemented yet; run JSON sync or use RSS browse/search for degraded reads"
                );
            }

            let client = RedditClient::new(&config, &paths, &cli)?;
            loop {
                let reports = sync::run_once(&paths, &client, command).await?;
                render::print_sync_reports(&reports, cli.json)?;

                let Some(loop_secs) = command.loop_secs else {
                    break;
                };
                tokio::time::sleep(Duration::from_secs(loop_secs)).await;
            }
        }
        Commands::Pull(command) => {
            if cli.rss {
                anyhow::bail!(
                    "rdt --rss pull is planned but not implemented yet; full-depth pull needs JSON parent metadata"
                );
            }
            let client = RedditClient::new(&config, &paths, &cli)?;
            let target = actions::resolve_post_target(&paths, &command.target)?;
            let thread_command = ThreadCommand {
                target: command.target.clone(),
                all: true,
                depth: command.depth,
                sort: command.sort,
                max_requests: command.max_requests,
            };
            let thread = client.thread(&thread_command, &target).await?;
            if thread.degraded {
                anyhow::bail!(
                    "rdt pull requires JSON parent metadata; RSS degraded thread output was not written"
                );
            }
            let report = store::upsert_thread(&paths, &thread)?;
            render::print_sync_reports(&[report], cli.json)?;
        }
        Commands::Digest(command) => {
            let rows =
                store::digest_rows(&paths, command.since.as_deref(), command.sub.as_deref())?;
            render::print_digest(&rows, command.json || cli.json)?;
        }
        Commands::Db(command) => match command {
            DbCommand::Path => println!("{}", paths.db_file.display()),
            DbCommand::Query { sql, json } => {
                let rows =
                    store::query(&paths, sql).with_context(|| format!("query failed: {sql}"))?;
                render::print_db_rows(&rows, *json || cli.json)?;
            }
            DbCommand::Search { query } => {
                let rows = store::search(&paths, query)?;
                render::print_db_rows(&rows, cli.json)?;
            }
        },
        Commands::Copy { target } => {
            let target = actions::resolve_target(&paths, target)?;
            actions::copy_permalink(&target)?;
            println!("{}", target);
        }
        Commands::Open { target } => {
            let target = actions::resolve_target(&paths, target)?;
            open::that(&target)?;
            println!("{}", target);
        }
        Commands::Save { target, note } => {
            let target = actions::resolve_target(&paths, target)?;
            store::save_link(&paths, &target, note.as_deref())?;
            println!("saved {target}");
        }
        Commands::Saved => {
            let rows = store::saved(&paths)?;
            render::print_db_rows(&rows, cli.json)?;
        }
        Commands::Auth(command) => match command {
            AuthCommand::Check => {
                let client = RedditClient::new(&config, &paths, &cli)?;
                let me = client.auth_check().await?;
                println!("{me}");
            }
            AuthCommand::Clear => {
                Config::clear_cookie(&paths)?;
                println!("cleared cached reddit_session cookie");
            }
            AuthCommand::FromBrowser { browser } => {
                let _ = browser;
                anyhow::bail!(
                    "browser cookie extraction is planned behind the browser-cookies feature; set RDT_COOKIE or config.toml cookie for now"
                );
            }
        },
    }

    Ok(())
}
