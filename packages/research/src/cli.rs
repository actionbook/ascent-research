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
    },
    /// List all research sessions.
    List,
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
    },
    /// List sources attached to the current or given session.
    Sources {
        slug: Option<String>,
        #[arg(long)]
        rejected: bool,
    },
    /// Synthesize session.md + raw/ into report.json + report.html.
    Synthesize {
        slug: Option<String>,
        #[arg(long)]
        no_render: bool,
        #[arg(long)]
        open: bool,
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
        Commands::New { topic, preset, slug, force } => {
            commands::new::run(&topic, preset.as_deref(), slug.as_deref(), force)
        }
        Commands::List => commands::list::run(),
        Commands::Show { slug } => commands::show::run(&slug),
        Commands::Status { slug } => commands::status::run(slug.as_deref()),
        Commands::Resume { slug } => commands::resume::run(&slug),
        Commands::Add { url, slug, timeout, readable, no_readable } => {
            commands::add::run(&url, slug.as_deref(), timeout, readable, no_readable)
        }
        Commands::Sources { slug, rejected } => {
            commands::sources::run(slug.as_deref(), rejected)
        }
        Commands::Synthesize { .. } => commands::synthesize::run(),
        Commands::Close { slug } => commands::close::run(slug.as_deref()),
        Commands::Rm { slug, force } => commands::rm::run(&slug, force),
        Commands::Route { url, prefer, rules, preset } => {
            commands::route::run(&url, prefer.as_deref(), rules.as_deref(), preset.as_deref())
        }
        Commands::Help => unreachable!("Help handled in run()"),
    }
}
