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
    /// Inspect session.jsonl as a compact audit trail for hand calls, facts, and synthesis.
    Audit { slug: Option<String> },
    /// Audit GitHub repository trust signals.
    #[command(name = "github-audit")]
    GithubAudit {
        repo: String,
        #[arg(long, default_value = "stargazers")]
        depth: String,
        #[arg(long, default_value_t = 200)]
        sample: usize,
        #[arg(long)]
        out: Option<String>,
        #[arg(long)]
        html: Option<String>,
    },
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
    /// Bulk-ingest a local file or directory tree as sources.
    ///
    /// Walks the path, applies optional --glob include/exclude patterns
    /// (prefix with `!` to exclude), enforces per-file and per-walk size
    /// caps, and attaches each accepted file as its own source via the
    /// same pipeline as `research add file:///...`.
    #[command(name = "add-local")]
    AddLocal {
        /// File or directory to ingest. Accepts `file://`, absolute,
        /// relative (./x), home-relative (~/x), or bare path.
        path: String,
        #[arg(long)]
        slug: Option<String>,
        /// Glob pattern (repeatable). Prefix with `!` to exclude.
        /// Examples: `--glob '**/*.rs'  --glob '!**/test/**'`.
        /// If omitted, matches all files.
        #[arg(long = "glob", action = clap::ArgAction::Append)]
        glob: Vec<String>,
        /// Per-file cap in bytes. Files over this are skipped with a
        /// `too_large` reason. Default 256 KiB.
        #[arg(long = "max-file-bytes")]
        max_file_bytes: Option<u64>,
        /// Total cap for the whole walk. Walk stops (not truncates)
        /// when this would be exceeded. Default 2 MiB.
        #[arg(long = "max-total-bytes")]
        max_total_bytes: Option<u64>,
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
        /// Also render Chinese translations next to each English paragraph
        /// in report.html. Requires a working LLM provider; choose one with
        /// ASR_BILINGUAL_PROVIDER=claude|codex. Costs tokens proportional
        /// to report length.
        #[arg(long)]
        bilingual: bool,
    },
    /// Run the completion protocol: coverage -> synthesize -> audit.
    Finish {
        slug: String,
        #[arg(long)]
        open: bool,
        #[arg(long)]
        bilingual: bool,
    },
    /// Render an editorial report from a session (rich-html and future formats).
    Report {
        slug: Option<String>,
        /// Output format. Supported: rich-html, brief-md.
        #[arg(long)]
        format: String,
        #[arg(long)]
        open: bool,
        #[arg(long = "no-open")]
        no_open: bool,
        /// (brief-md only) print to stdout instead of writing a file.
        #[arg(long)]
        stdout: bool,
        /// (brief-md only) explicit output path; default: <session>/report-brief.md.
        #[arg(long)]
        output: Option<String>,
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
    /// Diff: list sources fetched-but-uncited (unused) and body-but-unfetched (hallucinated).
    Diff {
        slug: Option<String>,
        /// Only list unused sources; omit the hallucinated/missing set.
        #[arg(long = "unused-only")]
        unused_only: bool,
    },
    /// Coverage: fact-based completeness stats + report_ready blockers.
    Coverage { slug: Option<String> },
    /// Verify local prerequisites for the skill/playbooks without creating a session.
    Doctor {
        /// Also do a live one-shot LLM provider call. This can spend tokens.
        #[arg(long = "provider-smoke")]
        provider_smoke: bool,
        /// Also exercise postagent/actionbook command surfaces.
        #[arg(long = "tool-smoke")]
        tool_smoke: bool,
        /// Provider to smoke-test: claude | codex | all.
        #[arg(long = "provider", default_value = "all")]
        provider: String,
    },
    /// Run the autonomous research loop (feature: autoresearch).
    #[cfg(feature = "autoresearch")]
    Loop {
        slug: Option<String>,
        /// LLM provider: fake | claude | codex.
        #[arg(long, default_value = "fake")]
        provider: String,
        #[arg(long)]
        iterations: Option<u32>,
        #[arg(long = "max-actions")]
        max_actions: Option<u32>,
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// (fake provider only) semicolon-separated JSON responses to replay.
        #[arg(long = "fake-responses")]
        fake_responses: Option<String>,
    },
    /// Inspect the per-session wiki (v3).
    Wiki {
        #[command(subcommand)]
        sub: WikiCmd,
    },
    /// Show or edit the per-session SCHEMA.md (v3).
    Schema {
        #[command(subcommand)]
        sub: SchemaCmd,
    },
    /// Show help (alias of --help).
    Help,
}

#[derive(Subcommand, Debug)]
pub enum SchemaCmd {
    /// Print the session's SCHEMA.md.
    Show {
        #[arg(long)]
        slug: Option<String>,
    },
    /// Open `$EDITOR` on the session's SCHEMA.md; logs `SchemaUpdated`
    /// on change so the loop re-reads it next iteration.
    Edit {
        #[arg(long)]
        slug: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum WikiCmd {
    /// List every wiki page in a session with slug, bytes, frontmatter kind.
    List {
        #[arg(long)]
        slug: Option<String>,
    },
    /// Print one wiki page to stdout.
    Show {
        /// The page slug (filename without `.md`).
        page: String,
        #[arg(long)]
        slug: Option<String>,
    },
    /// Remove a wiki page. Dry-run unless `--force` is passed.
    Rm {
        /// The page slug to remove.
        page: String,
        #[arg(long)]
        slug: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// Ask a question over the session's wiki; optionally save answer
    /// as a `kind: analysis` page via `--save-as <slug>`.
    Query {
        /// The question to ask.
        question: String,
        #[arg(long)]
        slug: Option<String>,
        /// Save the answer as `wiki/<slug>.md` with `kind: analysis`.
        #[arg(long = "save-as")]
        save_as: Option<String>,
        /// Answer shape: prose (default) | comparison | table.
        #[arg(long)]
        format: Option<String>,
        /// LLM provider: fake | claude | codex.
        #[arg(long, default_value = "claude")]
        provider: String,
    },
    /// Health check over the wiki (orphans, broken links, stale pages,
    /// missing crossrefs, kind conflicts). Never blocks coverage.
    Lint {
        #[arg(long)]
        slug: Option<String>,
        /// Flag pages whose `updated:` frontmatter is older than this
        /// many days. Default 7.
        #[arg(long = "stale-days")]
        stale_days: Option<i64>,
    },
}

/// Entry point used by `main.rs`. Returns the process exit code.
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;

    let (envelope, github_audit_plain) = match cli.command {
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
        Some(cmd) => {
            let github_audit_plain = matches!(cmd, Commands::GithubAudit { .. });
            (dispatch(cmd), github_audit_plain)
        }
    };

    if github_audit_plain && !json {
        commands::github_audit::render_plain_summary(&envelope);
    } else {
        envelope.render(json);
    }
    if envelope.ok {
        ExitCode::SUCCESS
    } else {
        // 64 = EX_USAGE per sysexits.h; keep single non-zero code for MVP
        ExitCode::from(64)
    }
}

fn dispatch(cmd: Commands) -> Envelope {
    match cmd {
        Commands::New {
            topic,
            preset,
            slug,
            force,
            from,
            tag,
        } => commands::new::run(
            &topic,
            preset.as_deref(),
            slug.as_deref(),
            force,
            from.as_deref(),
            &tag,
        ),
        Commands::List { tag, tree } => commands::list::run(tag.as_deref(), tree),
        Commands::Show { slug } => commands::show::run(&slug),
        Commands::Status { slug } => commands::status::run(slug.as_deref()),
        Commands::Audit { slug } => commands::audit::run(slug.as_deref()),
        Commands::GithubAudit {
            repo,
            depth,
            sample,
            out,
            html,
        } => commands::github_audit::run(&repo, &depth, sample, out.as_deref(), html.as_deref()),
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
        Commands::AddLocal {
            path,
            slug,
            glob,
            max_file_bytes,
            max_total_bytes,
        } => commands::add_local::run(
            &path,
            slug.as_deref(),
            &glob,
            max_file_bytes,
            max_total_bytes,
        ),
        Commands::Sources { slug, rejected } => commands::sources::run(slug.as_deref(), rejected),
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
        Commands::Synthesize {
            slug,
            no_render,
            open,
            bilingual,
        } => commands::synthesize::run(slug.as_deref(), no_render, open, bilingual),
        Commands::Finish {
            slug,
            open,
            bilingual,
        } => commands::finish::run(&slug, open, bilingual),
        Commands::Report {
            slug,
            format,
            open,
            no_open,
            stdout,
            output,
        } => commands::report::run(
            slug.as_deref(),
            &format,
            open,
            no_open,
            stdout,
            output.as_deref(),
        ),
        Commands::Close { slug } => commands::close::run(slug.as_deref()),
        Commands::Rm { slug, force } => commands::rm::run(&slug, force),
        Commands::Route {
            url,
            prefer,
            rules,
            preset,
        } => commands::route::run(&url, prefer.as_deref(), rules.as_deref(), preset.as_deref()),
        Commands::Series { tag, open } => commands::series::run(&tag, open),
        Commands::Diff { slug, unused_only } => commands::diff::run(slug.as_deref(), unused_only),
        Commands::Coverage { slug } => commands::coverage::run(slug.as_deref()),
        Commands::Doctor {
            provider_smoke,
            tool_smoke,
            provider,
        } => commands::doctor::run(provider_smoke, tool_smoke, &provider),
        #[cfg(feature = "autoresearch")]
        Commands::Loop {
            slug,
            provider,
            iterations,
            max_actions,
            dry_run,
            fake_responses,
        } => commands::loop_cmd::run(
            slug.as_deref(),
            &provider,
            iterations,
            max_actions,
            dry_run,
            fake_responses.as_deref().map(split_fake_responses),
        ),
        Commands::Wiki { sub } => match sub {
            WikiCmd::List { slug } => commands::wiki::run_list(slug.as_deref()),
            WikiCmd::Show { page, slug } => commands::wiki::run_show(&page, slug.as_deref()),
            WikiCmd::Rm { page, slug, force } => {
                commands::wiki::run_rm(&page, slug.as_deref(), force)
            }
            WikiCmd::Query {
                question,
                slug,
                save_as,
                format,
                provider,
            } => commands::wiki_query::run(
                &question,
                slug.as_deref(),
                save_as.as_deref(),
                format.as_deref(),
                &provider,
            ),
            WikiCmd::Lint { slug, stale_days } => {
                commands::wiki_lint::run(slug.as_deref(), stale_days)
            }
        },
        Commands::Schema { sub } => match sub {
            SchemaCmd::Show { slug } => commands::schema::run_show(slug.as_deref()),
            SchemaCmd::Edit { slug } => commands::schema::run_edit(slug.as_deref()),
        },
        Commands::Help => unreachable!("Help handled in run()"),
    }
}

/// Split `--fake-responses` into individual JSON turns.
///
/// Accepts BOTH separators:
/// - ASCII Record Separator (`\u{1e}`) — original wire format, used by
///   integration tests that pipe multiple JSON payloads where `;` or
///   commas inside the JSON would be ambiguous.
/// - Semicolon (`;`) — what the `--help` text advertises; also the
///   ergonomic choice for a developer typing a quick debug replay.
///
/// Semicolon takes precedence: if the string contains a literal `;` we
/// split on it, otherwise we fall back to the record separator. This
/// keeps the test wire format working and lets CLI users follow the
/// documented syntax.
#[cfg(any(feature = "autoresearch", test))]
fn split_fake_responses(raw: &str) -> Vec<String> {
    let delim: char = if raw.contains(';') { ';' } else { '\u{1e}' };
    raw.split(delim).map(str::to_string).collect()
}

#[cfg(test)]
mod split_fake_tests {
    use super::split_fake_responses;

    #[test]
    fn splits_on_semicolon_when_present() {
        let v = split_fake_responses("resp1;resp2;resp3");
        assert_eq!(v, vec!["resp1", "resp2", "resp3"]);
    }

    #[test]
    fn falls_back_to_record_separator() {
        let v = split_fake_responses("a\u{1e}b\u{1e}c");
        assert_eq!(v, vec!["a", "b", "c"]);
    }

    #[test]
    fn single_payload_yields_one_element() {
        let v = split_fake_responses("just-one");
        assert_eq!(v, vec!["just-one"]);
    }

    #[test]
    fn semicolon_wins_over_record_separator_if_both_present() {
        // Record separator is vanishingly unlikely inside a JSON payload,
        // but verify the precedence documented in the helper's docstring.
        let v = split_fake_responses("a;b\u{1e}c");
        assert_eq!(v, vec!["a", "b\u{1e}c"]);
    }
}
