//! CLI argument parsing + dispatch. All 12 subcommands resolve to
//! handlers in `commands::*` that, in MVP #1, return NOT_IMPLEMENTED.

use clap::{Parser, Subcommand};
use std::process::ExitCode;

use crate::commands;
use crate::output::Envelope;

#[derive(Parser, Debug)]
#[command(
    name = "research",
    about = "Research workflow CLI — orchestrate postagent + actionbook for reproducible research sessions",
    disable_version_flag = true
)]
pub struct Cli {
    /// JSON output (default is plain text)
    #[arg(long, global = true)]
    pub json: bool,

    /// Increase logging verbosity (to stderr)
    #[arg(long, short = 'v', global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Disable ANSI color in plain-text output
    #[arg(long, global = true)]
    pub no_color: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
#[command(disable_help_subcommand = true)]
pub enum Commands {
    /// Create a new research session and set it active.
    New {
        topic: String,
        #[arg(long)]
        preset: Option<String>,
        #[arg(long)]
        slug: Option<String>,
        #[arg(long)]
        force: bool,
        /// Fork from a parent session — copies its ## Overview as ## Context.
        #[arg(long = "from")]
        from: Option<String>,
        /// Tag this session (repeatable). Inherited from --from if provided.
        #[arg(long = "tag", action = clap::ArgAction::Append)]
        tag: Vec<String>,
    },
    /// List all research sessions.
    List {
        /// Filter by tag.
        #[arg(long)]
        tag: Option<String>,
        /// Show parent→child hierarchy as an ASCII tree.
        #[arg(long)]
        tree: bool,
    },
    /// Print a session.md to stdout so an agent can resume context.
    Show { slug: String },
    /// Show counts + timings for the current or given session.
    Status { slug: Option<String> },
    /// Set a session active again and print its session.md + recent events.
    Resume { slug: String },
    /// Route + fetch + smell-test a URL and attach to the active session.
    Add {
        url: String,
        #[arg(long)]
        slug: Option<String>,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        readable: bool,
        #[arg(long)]
        no_readable: bool,
        /// Override smell-test min body bytes (browser path only).
        #[arg(long = "min-bytes")]
        min_bytes: Option<u64>,
        /// Short-body behavior: "reject" (default) or "warn".
        #[arg(long = "on-short-body")]
        on_short_body: Option<String>,
    },
    /// List sources attached to the current or given session.
    Sources {
        slug: Option<String>,
        #[arg(long)]
        rejected: bool,
    },
    /// Route + fetch + smell-test multiple URLs in parallel.
    Batch {
        /// One or more URLs to fetch concurrently.
        urls: Vec<String>,
        #[arg(long)]
        slug: Option<String>,
        /// Worker threads (1–16, default 4).
        #[arg(long)]
        concurrency: Option<usize>,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        readable: bool,
        #[arg(long)]
        no_readable: bool,
        /// Override smell-test min body bytes (browser path only).
        #[arg(long = "min-bytes")]
        min_bytes: Option<u64>,
        /// Short-body behavior: "reject" (default) or "warn".
        #[arg(long = "on-short-body")]
        on_short_body: Option<String>,
    },
    /// Synthesize session.md + raw/ into report.json + report.html.
    Synthesize {
        slug: Option<String>,
        #[arg(long)]
        no_render: bool,
        #[arg(long)]
        open: bool,
    },
    /// Render an editorial report from a session (rich-html and future formats).
    Report {
        slug: Option<String>,
        /// Output format. v1 supports: rich-html.
        #[arg(long)]
        format: String,
        #[arg(long)]
        open: bool,
        #[arg(long = "no-open")]
        no_open: bool,
    },
    /// Mark a session closed (files preserved).
    Close { slug: Option<String> },
    /// Remove a session directory.
    Rm {
        slug: String,
        #[arg(long)]
        force: bool,
    },
    /// Classify a URL: which executor + command template.
    Route {
        url: String,
        #[arg(long)]
        prefer: Option<String>,
        #[arg(long)]
        rules: Option<String>,
        #[arg(long)]
        preset: Option<String>,
    },
    /// Generate an HTML index page for all sessions with a given tag.
    Series {
        tag: String,
        #[arg(long)]
        open: bool,
    },
    /// Show help (alias of --help).
    Help,
}

/// Entry point used by `main.rs`. Returns the process exit code.
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;

    let envelope = match cli.command {
        None => {
            // bare `research` with no subcommand: print help via clap and exit 0
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            let _ = cmd.print_help();
            println!();
            return ExitCode::SUCCESS;
        }
        Some(Commands::Help) => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            let _ = cmd.print_help();
            println!();
            return ExitCode::SUCCESS;
        }
        Some(cmd) => dispatch(cmd),
    };

    envelope.render(json);
    if envelope.ok {
        ExitCode::SUCCESS
    } else {
        // 64 = EX_USAGE per sysexits.h; keep single non-zero code for MVP
        ExitCode::from(64)
    }
}

fn dispatch(cmd: Commands) -> Envelope {
    match cmd {
        Commands::New { topic, preset, slug, force, from, tag } => {
            commands::new::run(
                &topic,
                preset.as_deref(),
                slug.as_deref(),
                force,
                from.as_deref(),
                &tag,
            )
        }
        Commands::List { tag, tree } => commands::list::run(tag.as_deref(), tree),
        Commands::Show { slug } => commands::show::run(&slug),
        Commands::Status { slug } => commands::status::run(slug.as_deref()),
        Commands::Resume { slug } => commands::resume::run(&slug),
        Commands::Add {
            url,
            slug,
            timeout,
            readable,
            no_readable,
            min_bytes,
            on_short_body,
        } => commands::add::run(
            &url,
            slug.as_deref(),
            timeout,
            readable,
            no_readable,
            min_bytes,
            on_short_body.as_deref(),
        ),
        Commands::Sources { slug, rejected } => {
            commands::sources::run(slug.as_deref(), rejected)
        }
        Commands::Batch {
            urls,
            slug,
            concurrency,
            timeout,
            readable,
            no_readable,
            min_bytes,
            on_short_body,
        } => commands::batch::run(
            &urls,
            slug.as_deref(),
            concurrency,
            timeout,
            readable,
            no_readable,
            min_bytes,
            on_short_body.as_deref(),
        ),
        Commands::Synthesize { slug, no_render, open } => {
            commands::synthesize::run(slug.as_deref(), no_render, open)
        }
        Commands::Report { slug, format, open, no_open } => {
            commands::report::run(slug.as_deref(), &format, open, no_open)
        }
        Commands::Close { slug } => commands::close::run(slug.as_deref()),
        Commands::Rm { slug, force } => commands::rm::run(&slug, force),
        Commands::Route { url, prefer, rules, preset } => {
            commands::route::run(&url, prefer.as_deref(), rules.as_deref(), preset.as_deref())
        }
        Commands::Series { tag, open } => commands::series::run(&tag, open),
        Commands::Help => unreachable!("Help handled in run()"),
    }
}
