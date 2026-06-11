use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "rdt")]
#[command(about = "Strictly read-only Reddit CLI")]
#[command(version)]
pub struct Cli {
    #[arg(long, global = true)]
    pub json: bool,
    #[arg(long, global = true)]
    pub fresh: bool,
    #[arg(long, global = true)]
    pub anon: bool,
    #[arg(long, global = true)]
    pub rss: bool,
    #[arg(long, global = true)]
    pub no_color: bool,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Search(SearchCommand),
    Browse(BrowseCommand),
    Subs(SubsCommand),
    Sub(SubCommand),
    User(UserCommand),
    Thread(ThreadCommand),
    Comment(CommentCommand),
    #[command(subcommand)]
    Watch(WatchCommand),
    Sync(SyncCommand),
    Pull(PullCommand),
    Digest(DigestCommand),
    #[command(subcommand)]
    Db(DbCommand),
    Copy {
        target: String,
    },
    Open {
        target: String,
    },
    Save {
        target: String,
        #[arg(long)]
        note: Option<String>,
    },
    Saved,
    #[command(subcommand)]
    Auth(AuthCommand),
}

#[derive(Debug, Args)]
pub struct SearchCommand {
    pub query: String,
    #[arg(short = 'r', long, value_delimiter = ',')]
    pub subreddits: Vec<String>,
    #[arg(long, value_enum, default_value_t = SearchSort::Relevance)]
    pub sort: SearchSort,
    #[arg(long = "time", value_enum, default_value_t = TimeWindow::All)]
    pub time: TimeWindow,
    #[arg(short = 'n', long, default_value_t = 25)]
    pub limit: u32,
    #[arg(long)]
    pub local: bool,
}

#[derive(Debug, Args)]
pub struct BrowseCommand {
    pub sub: String,
    #[arg(long, value_enum, default_value_t = BrowseSort::Hot)]
    pub sort: BrowseSort,
    #[arg(long = "time", value_enum, default_value_t = TimeWindow::Day)]
    pub time: TimeWindow,
    #[arg(short = 'n', long, default_value_t = 25)]
    pub limit: u32,
    #[arg(long)]
    pub after: Option<String>,
}

#[derive(Debug, Args)]
pub struct SubsCommand {
    pub query: String,
}

#[derive(Debug, Args)]
pub struct SubCommand {
    pub name: String,
}

#[derive(Debug, Args)]
pub struct UserCommand {
    pub name: String,
    #[arg(long, value_enum, default_value_t = UserWhat::Overview)]
    pub what: UserWhat,
    #[arg(short = 'n', long, default_value_t = 25)]
    pub limit: u32,
}

#[derive(Debug, Args)]
pub struct ThreadCommand {
    pub target: String,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub depth: Option<u32>,
    #[arg(long, value_enum, default_value_t = ThreadSort::Best)]
    pub sort: ThreadSort,
    #[arg(long, default_value_t = 30)]
    pub max_requests: u32,
}

#[derive(Debug, Args)]
pub struct CommentCommand {
    pub url: String,
    #[arg(long, default_value_t = 3)]
    pub context: u32,
}

#[derive(Debug, Subcommand)]
pub enum WatchCommand {
    Add { subreddits: Vec<String> },
    Rm { subreddit: String },
    Ls,
}

#[derive(Debug, Args)]
pub struct SyncCommand {
    #[arg(
        long = "sub",
        value_delimiter = ',',
        help = "Sync explicit subreddit(s) instead of only active watches"
    )]
    pub subreddits: Vec<String>,
    #[arg(long, help = "Override the page cap per stream")]
    pub pages: Option<u32>,
    #[arg(long = "loop", help = "Repeat sync every SECS seconds")]
    pub loop_secs: Option<u64>,
    #[arg(long, default_value_t = 300, help = "Target item budget per stream")]
    pub budget: u32,
    #[arg(long)]
    pub refresh: bool,
}

#[derive(Debug, Args)]
pub struct PullCommand {
    pub target: String,
    #[arg(long)]
    pub depth: Option<u32>,
    #[arg(long, value_enum, default_value_t = ThreadSort::Best)]
    pub sort: ThreadSort,
    #[arg(long, default_value_t = 30)]
    pub max_requests: u32,
}

#[derive(Debug, Args)]
pub struct DigestCommand {
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub sub: Option<String>,
    #[arg(long)]
    pub md: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum DbCommand {
    Path,
    Query {
        sql: String,
        #[arg(long)]
        json: bool,
    },
    Search {
        query: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    FromBrowser {
        #[arg(long)]
        browser: Option<String>,
    },
    Check,
    Clear,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SearchSort {
    Relevance,
    Top,
    New,
    Comments,
}

impl SearchSort {
    pub fn as_reddit(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Top => "top",
            Self::New => "new",
            Self::Comments => "comments",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum BrowseSort {
    Hot,
    New,
    Top,
    Rising,
    Controversial,
}

impl BrowseSort {
    pub fn as_reddit(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::New => "new",
            Self::Top => "top",
            Self::Rising => "rising",
            Self::Controversial => "controversial",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ThreadSort {
    Best,
    Top,
    New,
    Controversial,
    Old,
    Qa,
}

impl ThreadSort {
    pub fn as_reddit(self) -> &'static str {
        match self {
            Self::Best => "best",
            Self::Top => "top",
            Self::New => "new",
            Self::Controversial => "controversial",
            Self::Old => "old",
            Self::Qa => "qa",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TimeWindow {
    Hour,
    Day,
    Week,
    Month,
    Year,
    All,
}

impl TimeWindow {
    pub fn as_reddit(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum UserWhat {
    Overview,
    Submitted,
    Comments,
}

impl UserWhat {
    pub fn as_reddit(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Submitted => "submitted",
            Self::Comments => "comments",
        }
    }
}
