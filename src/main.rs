use clap::{Parser, Subcommand};
use rrecall::commands;
use std::process::ExitCode;

/// Keeps a versioned map of a software project and answers questions about it.
#[derive(Parser)]
#[command(name = "rrecall", version)]
struct Cli {
    /// The project (else RRECALL_PROJECT, else the current repo's origin remote looked up in the user config).
    #[arg(long, global = true)]
    project: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create the map store with the default skeleton and a config naming the sources found.
    Init { project: Option<String> },
    /// Seed an empty map from an indented outline of `title | detail` lines.
    Import { outline: std::path::PathBuf },
    /// The tree of titles and ids.
    Ls {
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u32).range(1..))]
        depth: u32,
        #[arg(long)]
        json: bool,
    },
    /// One node and its subtree, with bodies and sources.
    Show {
        id: String,
        #[arg(long, default_value_t = 0)]
        depth: u32,
        /// Cut the output at a node boundary to about this many tokens (4 bytes each).
        #[arg(long)]
        max_tokens: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Nodes whose title or body contains the text, each with its path.
    Find {
        text: String,
        #[arg(long)]
        json: bool,
    },
    /// When the node appeared and changed, with the commit reasons.
    Why {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// What changed in the map lately, and dated nodes in the window.
    Recent {
        /// The window: `<N>d` days or `<N>w` weeks.
        #[arg(long, default_value = "7d")]
        since: String,
        #[arg(long)]
        json: bool,
    },
    /// Run the pipeline: collect from the sources that changed, check, judge, commit.
    Run {
        /// Run only this collector instance.
        #[arg(long)]
        only: Option<String>,
        /// Ignore fingerprints and collect every source again.
        #[arg(long)]
        force: bool,
        /// Exit 4 instead of waiting when another run holds the store lock.
        #[arg(long)]
        no_wait: bool,
    },
    /// The last run and its result.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Set your annotation on a node (an empty text clears it); runs never change it.
    Note { id: String, text: String },
    /// Remove a node by hand.
    Rm {
        id: String,
        #[arg(long)]
        reason: Option<String>,
        /// Also remove the node's descendants.
        #[arg(long)]
        recursive: bool,
    },
    /// Print a paragraph for an agent's instruction file.
    AgentInstructions,
    /// Run a built-in check plugin: check input on stdin, verdicts on stdout (spec §6.2, §6.5).
    #[command(hide = true)]
    Builtin { id: String },
}

/// Plugins run in their own dir (spec §6.1), so a relative `CLAUDE_CONFIG_DIR` is made absolute
/// against the dir rrecall was started in, before anything runs.
fn absolute_claude_config() {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty())
        && let Ok(abs) = std::path::absolute(&d)
        && abs.as_os_str() != d
    {
        // SAFETY: the first call in `main`, before any thread starts.
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", abs) };
    }
}

fn main() -> ExitCode {
    absolute_claude_config();
    let cli = Cli::parse();
    let p = cli.project.as_deref();
    let out = match &cli.cmd {
        Cmd::Init { project } => commands::init(project.as_deref(), p),
        Cmd::Import { outline } => commands::import(p, outline),
        Cmd::Ls { depth, json } => commands::ls(p, *depth as usize, *json),
        Cmd::Show {
            id,
            depth,
            max_tokens,
            json,
        } => commands::show(p, id, *depth as usize, *max_tokens, *json),
        Cmd::Find { text, json } => commands::find(p, text, *json),
        Cmd::Why { id, json } => commands::why(p, id, *json),
        Cmd::Recent { since, json } => commands::recent(p, since, *json),
        Cmd::Run {
            only,
            force,
            no_wait,
        } => rrecall::run::run(
            p,
            &rrecall::run::Opts {
                only: only.clone(),
                force: *force,
                wait: !*no_wait,
            },
        ),
        Cmd::Status { json } => commands::status(p, *json),
        Cmd::Note { id, text } => commands::note(p, id, text),
        Cmd::Rm {
            id,
            reason,
            recursive,
        } => commands::rm(p, id, reason.as_deref(), *recursive),
        Cmd::AgentInstructions => Ok(commands::AGENT_INSTRUCTIONS.to_string()),
        Cmd::Builtin { id } => commands::builtin(id),
    };
    match out {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("rrecall: {e:#}");
            ExitCode::from(rrecall::error::exit_code(&e))
        }
    }
}
