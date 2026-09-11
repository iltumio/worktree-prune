mod prune;
mod tui;

use anyhow::{Result, bail};
use clap::Parser;
use std::{io::IsTerminal, path::PathBuf};

#[derive(Parser, Debug, Clone)]
#[command(version, about)]
pub struct Args {
    /// Worktree names or paths (no arguments opens the TUI)
    worktrees: Vec<PathBuf>,
    /// Preview without removing anything (default)
    #[arg(short = 'n', long, overrides_with = "yes")]
    dry_run: bool,
    /// Apply the removal plan
    #[arg(short = 'y', long, overrides_with = "dry_run")]
    yes: bool,
    /// Discard local changes and allow local-only commits or stale files
    #[arg(short, long)]
    force: bool,
    /// Use sudo to remove foreign-owned or unwritable paths
    #[arg(short = 's', long)]
    sudo: bool,
    /// Hook mode: concise output and successful exit even on runtime errors
    #[arg(short, long)]
    quiet: bool,
    /// Leave Cargo target directories untouched
    #[arg(long, conflicts_with = "orphans")]
    keep_target: bool,
    /// Also delete the local branch
    #[arg(long)]
    delete_branch: bool,
    /// List worktrees and target sizes
    #[arg(long)]
    list: bool,
    /// Select unregistered leftover directories
    #[arg(long)]
    stale: bool,
    /// Select unclaimed Cargo target directories
    #[arg(long)]
    orphans: bool,
    /// Run in this repository
    #[arg(short = 'C', long, default_value = ".")]
    repo: PathBuf,
}

fn run(args: &Args, interactive: bool) -> Result<()> {
    let repo = prune::Repo::open(&args.repo)?;
    let entries = repo.inventory(args.orphans)?;
    if interactive {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            bail!("interactive mode requires a terminal; use --list for text output");
        }
        let mut interactive_args = args.clone();
        interactive_args.yes = true;
        if let Some(selected) = tui::select(&repo, &entries, &interactive_args)? {
            // Re-read inventory and checks after the user has reviewed the plan.
            let fresh = repo.inventory(args.orphans)?;
            let mut apply = interactive_args;
            apply.worktrees = selected.paths;
            apply.force = selected.force;
            apply.stale = false;
            apply.orphans = false;
            let plan = repo.plan(&fresh, &apply)?;
            repo.execute(&plan, &apply)?;
        }
        return Ok(());
    }
    if args.list || (args.worktrees.is_empty() && !args.stale && !args.orphans) {
        repo.list(&entries, args.quiet);
    }
    if args.worktrees.is_empty() && !args.stale && !args.orphans {
        return Ok(());
    }
    let plan = repo.plan(&entries, args)?;
    if !args.quiet {
        println!("{}", plan.describe());
    }
    repo.execute(&plan, args)
}

fn main() {
    let interactive = std::env::args_os().len() == 1;
    let args = Args::parse();
    if let Err(error) = run(&args, interactive)
        && !args.quiet
    {
        eprintln!("worktree-prune: {error:#}");
        std::process::exit(1);
    }
}
