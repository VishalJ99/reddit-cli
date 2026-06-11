pub mod actions;
pub mod cli;
pub mod config;
pub mod error;
pub mod model;
pub mod parse;
pub mod render;
pub mod store;
pub mod transport;

pub use cli::Cli;

use anyhow::Context;
use cli::{AuthCommand, Commands, DbCommand, WatchCommand};
use config::{Config, Paths};
use transport::RedditClient;

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let paths = Paths::resolve()?;
    let config = Config::load(&paths)?;

    match &cli.command {
        Commands::Search(command) => {
            let client = RedditClient::new(&config, &cli)?;
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
            let client = RedditClient::new(&config, &cli)?;
            let mut items = client.browse(command).await?;
            actions::write_last(&paths, &mut items)?;
            render::print_items(&items, cli.json, cli.no_color)?;
        }
        Commands::Subs(command) => {
            let client = RedditClient::new(&config, &cli)?;
            let mut items = client.subreddits(&command.query).await?;
            actions::write_last(&paths, &mut items)?;
            render::print_items(&items, cli.json, cli.no_color)?;
        }
        Commands::Sub(command) => {
            let client = RedditClient::new(&config, &cli)?;
            let value = client.subreddit_about(&command.name).await?;
            render::print_json_or_debug(&value, cli.json)?;
        }
        Commands::User(command) => {
            let client = RedditClient::new(&config, &cli)?;
            let mut items = client.user(command).await?;
            actions::write_last(&paths, &mut items)?;
            render::print_items(&items, cli.json, cli.no_color)?;
        }
        Commands::Thread(command) => {
            let client = RedditClient::new(&config, &cli)?;
            let target = actions::resolve_target(&paths, &command.target)?;
            let thread = client.thread(command, &target).await?;
            render::print_thread(&thread, cli.json, cli.no_color)?;
        }
        Commands::Comment(command) => {
            let client = RedditClient::new(&config, &cli)?;
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
            let _ = command;
            anyhow::bail!(
                "sync is planned for M3 in DESIGN.md; watch storage is scaffolded, but polling is not implemented yet"
            );
        }
        Commands::Pull(command) => {
            let _ = command;
            anyhow::bail!(
                "pull is planned for M3 in DESIGN.md; deep DB capture is not implemented yet"
            );
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
                let client = RedditClient::new(&config, &cli)?;
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
