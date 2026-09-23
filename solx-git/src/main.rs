//! solx-git — git operations action binary for solx-core.
//!
//! Ships as four solx `Command` actions, `git-clone`/`git-patch`/
//! `git-branch`/`git-commit`, one binary dispatching on a subcommand
//! (mirrors solx-firefox and solx-mcp-actions; unlike solx-quickjs's
//! flag-based dispatch). Params are JSON on stdin — `run_command`
//! (solx-core's `solx-actions/src/exec.rs`) writes them there for every
//! Command action — and the subcommand is baked into each action's
//! `command_actions` entry in `solx-package.json`, not chosen by the
//! caller.
//!
//! Every action takes a `path`, resolved the same way (see `paths`): a
//! relative path lands inside solx-core's file store, an absolute path
//! must be listed in `<appdata>/solx-git.json`'s `allowed_paths`.
//!
//! Plain `fn main` (no tokio runtime): git2/libgit2 is entirely
//! synchronous, so logging goes through `solx_package_log::blocking`
//! rather than the crate's async entry points (which panic with no ambient
//! runtime) — same reasoning as solx-firefox.

mod auth;
mod branch;
mod clone;
mod commit;
mod git_util;
mod patch;
mod paths;

use clap::{Parser, Subcommand};
use serde_json::json;
use solx_package_log::{print_json, stdin_params};

#[derive(Parser)]
#[command(name = "solx-git")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Clone a repository (or sync an existing checkout) to path.
    Clone,
    /// Produce a unified diff patch of a repo's current changes.
    Patch,
    /// Create a branch and/or switch the repo's current branch.
    Branch,
    /// Stage and commit a repo's current changes.
    Commit,
}

fn main() {
    solx_package_log::init("solx-git");

    let cli = Cli::parse();
    let params = stdin_params();

    let result = match cli.command {
        Commands::Clone => run_clone(params),
        Commands::Patch => run_patch(params),
        Commands::Branch => run_branch(params),
        Commands::Commit => run_commit(params),
    };

    match result {
        Ok(value) => print_json(&value),
        Err(e) => {
            solx_package_log::blocking::error(&format!("fatal: {e:#}"));
            print_json(&json!({ "error": e.to_string() }));
            std::process::exit(1);
        }
    }
}

fn run_clone(params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let params: clone::CloneParams =
        serde_json::from_value(params).map_err(|e| anyhow::anyhow!("invalid git-clone params: {e}"))?;
    solx_package_log::blocking::info(&format!("git-clone: {} -> {}", params.url, params.path));
    let result = clone::run(params)?;
    Ok(serde_json::to_value(result)?)
}

fn run_patch(params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let params: patch::PatchParams =
        serde_json::from_value(params).map_err(|e| anyhow::anyhow!("invalid git-patch params: {e}"))?;
    solx_package_log::blocking::info(&format!("git-patch: {}", params.path));
    let result = patch::run(params)?;
    Ok(serde_json::to_value(result)?)
}

fn run_branch(params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let params: branch::BranchParams =
        serde_json::from_value(params).map_err(|e| anyhow::anyhow!("invalid git-branch params: {e}"))?;
    solx_package_log::blocking::info(&format!("git-branch: {} @ {}", params.branch, params.path));
    let result = branch::run(params)?;
    Ok(serde_json::to_value(result)?)
}

fn run_commit(params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let params: commit::CommitParams =
        serde_json::from_value(params).map_err(|e| anyhow::anyhow!("invalid git-commit params: {e}"))?;
    solx_package_log::blocking::info(&format!("git-commit: {}", params.path));
    let result = commit::run(params)?;
    Ok(serde_json::to_value(result)?)
}
