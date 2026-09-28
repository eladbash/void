//! `void` — Void on the command line, and as an MCP server your AI agents
//! can call.
//!
//! Every clean goes scan → plan → apply: `void plan` shows exactly what would
//! run and saves it; `void apply <plan-id>` runs precisely that, through the
//! same safety-checked executor the desktop app uses, and records it in the
//! app's history.

mod commands;
mod ctx;
mod hook;
mod mcp;
mod output;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use deepclean_core::model::Ecosystem;

use crate::commands::PlanArgs;
use crate::ctx::Ctx;

#[derive(Parser)]
#[command(
    name = "void",
    version,
    about = "Find and clean developer disk bloat — and let your AI agents ask for it.",
    after_help = "Start with `void scan`, then `void plan`, then `void apply <plan-id>`.\n\
                  Try it safely: `void dev seed /tmp/void-sandbox && void --home /tmp/void-sandbox scan`."
)]
struct Cli {
    /// Treat DIR as the home directory: a sandbox. Scans stay inside it, and
    /// config, history, plans and the Trash live inside it too.
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,

    /// Config file (default: the Void app's config.json). History is kept
    /// beside it.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Machine-readable output.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List everything that could be cleaned. Changes nothing.
    Scan {
        /// Only these ecosystems (repeatable): rust, node, worktrees, models…
        #[arg(long = "ecosystem", short = 'e', value_name = "NAME")]
        ecosystems: Vec<String>,
    },
    /// Build a dry-run plan and print exactly what each entry would run.
    Plan(PlanCli),
    /// Apply a saved plan exactly as it was shown.
    Apply {
        plan_id: String,
        /// Do not ask for confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Git worktrees created by AI agents, and their leftovers.
    Worktrees,
    /// Local AI models (Ollama, Hugging Face, LM Studio, …).
    Models,
    /// Data AI agents keep about themselves (transcripts, logs, caches).
    Agents,
    /// Disk space attributed to each AI tool.
    Usage,
    /// Disk guard.
    Guard {
        #[command(subcommand)]
        command: GuardCommand,
    },
    /// Claude Code hooks: install/uninstall/status, and the hook handlers.
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },
    /// Run the MCP server on stdio.
    Mcp,
    /// Print the MCP server config for your agent.
    McpConfig,
    /// Development tools.
    Dev {
        #[command(subcommand)]
        command: DevCommand,
    },
}

#[derive(Args)]
struct PlanCli {
    /// Only these ecosystems (repeatable).
    #[arg(long = "ecosystem", short = 'e', value_name = "NAME")]
    ecosystems: Vec<String>,
    /// Only these artifact kinds (repeatable), e.g. node_modules, target_dir.
    #[arg(long = "kind", short = 'k', value_name = "KIND")]
    kinds: Vec<String>,
    /// Riskiest action allowed: safe (default) or caution.
    #[arg(long, value_name = "RISK", default_value = "safe")]
    max_risk: String,
    /// Only items untouched for at least N days.
    #[arg(long, value_name = "N")]
    stale_days: Option<u64>,
    /// Only items under this path (repeatable).
    #[arg(long = "path", short = 'p', value_name = "PATH")]
    paths: Vec<PathBuf>,
    /// Only these item ids from a scan (repeatable).
    #[arg(long = "item", value_name = "ID")]
    items: Vec<String>,
    /// Stop once this much is planned, e.g. 10GB.
    #[arg(long, value_name = "SIZE")]
    limit: Option<String>,
}

#[derive(Subcommand)]
enum GuardCommand {
    /// Check free space. Exit code 0 = ok, 1 = low, 2 = critical.
    Check,
}

#[derive(Subcommand)]
enum HookCommand {
    /// Add Void's hooks to ~/.claude/settings.json (a backup is written first).
    Install,
    /// Remove only Void's hooks.
    Uninstall,
    /// Show whether Void's hooks are installed.
    Status,
    /// Hook handler: warn the agent when disk space is low.
    SessionStart,
    /// Hook handler: trim build folders inside a worktree being removed.
    WorktreeRemove,
}

#[derive(Subcommand)]
enum DevCommand {
    /// Build a realistic fake home in DIR to try Void against.
    Seed { dir: PathBuf },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let code = match run(cli).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            1
        }
    };
    std::process::exit(code);
}

async fn run(cli: Cli) -> Result<i32, String> {
    // Commands that need no home or config.
    match &cli.command {
        Command::Dev {
            command: DevCommand::Seed { dir },
        } => return commands::dev_seed(dir),
        Command::McpConfig => return commands::mcp_config(),
        _ => {}
    }

    let hook_handler = matches!(
        cli.command,
        Command::Hook {
            command: HookCommand::SessionStart | HookCommand::WorktreeRemove
        }
    );
    let ctx = match Ctx::new(cli.home, cli.config, cli.json) {
        Ok(ctx) => ctx,
        // A hook handler must never fail the agent's session.
        Err(err) if hook_handler => {
            eprintln!("void: {err}");
            return Ok(0);
        }
        Err(err) => return Err(err),
    };
    if let Some(warning) = &ctx.config_warning {
        eprintln!("warning: {warning}; using defaults");
    }

    match cli.command {
        Command::Scan { ecosystems } => commands::scan(&ctx, &ecosystems).await,
        Command::Plan(p) => {
            let args = PlanArgs {
                ecosystems: p.ecosystems,
                kinds: p.kinds,
                max_risk: Some(p.max_risk),
                stale_days: p.stale_days,
                paths: p.paths,
                item_ids: p.items,
                limit: p.limit,
                limit_bytes: None,
            };
            // Validate before a potentially long scan.
            args.filter(&ctx)?;
            commands::plan(&ctx, &args).await
        }
        Command::Apply { plan_id, yes } => commands::apply(&ctx, &plan_id, yes).await,
        Command::Worktrees => commands::focused(&ctx, Ecosystem::Worktrees).await,
        Command::Models => commands::focused(&ctx, Ecosystem::Models).await,
        Command::Agents => commands::focused(&ctx, Ecosystem::AgentData).await,
        Command::Usage => commands::usage(&ctx).await,
        Command::Guard {
            command: GuardCommand::Check,
        } => commands::guard_check(&ctx),
        Command::Hook { command } => match command {
            HookCommand::Install => hook::install(&ctx),
            HookCommand::Uninstall => hook::uninstall(&ctx),
            HookCommand::Status => hook::status(&ctx),
            HookCommand::SessionStart => Ok(hook::session_start(&ctx)),
            HookCommand::WorktreeRemove => Ok(hook::worktree_remove(&ctx).await),
        },
        Command::Mcp => mcp::serve(ctx).await,
        Command::McpConfig | Command::Dev { .. } => Ok(0),
    }
}
