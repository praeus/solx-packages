//! `git-patch`: produce a unified diff of a repo's current changes.
//!
//! By default this diffs the working tree against `HEAD` — the union of
//! staged and unstaged changes, i.e. what `git diff HEAD` shows, including
//! untracked files (matching `git status`'s idea of "current changes").
//! `staged_only` narrows it to `git diff --cached` (index vs `HEAD`), which
//! never includes untracked files regardless of `include_untracked`.

use anyhow::{Context, Result};
use git2::{DiffFormat, DiffOptions, Repository, Tree};
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct PatchParams {
    /// The repo's working directory (not necessarily its root — any path
    /// inside the repo works, same as the `git` CLI). A relative path
    /// resolves inside solx-core's file store; an absolute path must be
    /// listed in `<appdata>/solx-git.json`'s `allowed_paths` — see
    /// `paths::resolve`.
    pub path: String,
    /// Restrict the diff to staged changes (index vs `HEAD`) instead of all
    /// current changes (working tree vs `HEAD`).
    #[serde(default)]
    pub staged_only: bool,
    /// Include untracked files as additions. Ignored when `staged_only` is
    /// set (untracked files are never part of the index).
    #[serde(default = "default_true")]
    pub include_untracked: bool,
    /// Lines of context around each hunk. Defaults to 3, matching `git`.
    #[serde(default)]
    pub context_lines: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct PatchResult {
    pub path: String,
    /// Unified diff text, ready to write to a `.patch` file or feed to
    /// `git apply`.
    pub patch: String,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
}

pub fn run(params: PatchParams) -> Result<PatchResult> {
    let repo_path = crate::paths::resolve(&params.path)?;
    let repo = Repository::open(&repo_path).with_context(|| format!("open repo at {}", repo_path.display()))?;

    let want_untracked = params.include_untracked && !params.staged_only;
    let mut diff_opts = DiffOptions::new();
    diff_opts
        .include_untracked(want_untracked)
        .recurse_untracked_dirs(want_untracked)
        // `include_untracked` alone only marks untracked files as changed
        // for stats/status purposes -- without this, their content never
        // makes it into the printed patch text at all.
        .show_untracked_content(want_untracked)
        .context_lines(params.context_lines.unwrap_or(3));

    let head_tree = head_tree(&repo)?;

    let diff = if params.staged_only {
        repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut diff_opts))
            .context("diff HEAD tree to the index (staged changes)")?
    } else {
        repo.diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut diff_opts))
            .context("diff HEAD tree to the working directory")?
    };

    let stats = diff.stats().context("compute diff stats")?;

    let mut patch = String::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        match line.origin() {
            '+' | '-' | ' ' => patch.push(line.origin()),
            _ => {}
        }
        patch.push_str(&String::from_utf8_lossy(line.content()));
        true
    })
    .context("format diff as a patch")?;

    Ok(PatchResult {
        path: repo_path.display().to_string(),
        patch,
        files_changed: stats.files_changed(),
        insertions: stats.insertions(),
        deletions: stats.deletions(),
    })
}

/// `None` for a brand-new repo with no commits yet (unborn HEAD) — diffing
/// against `None` compares against an empty tree, so every tracked/staged
/// file shows up as an addition instead of erroring out.
fn head_tree(repo: &Repository) -> Result<Option<Tree<'_>>> {
    match repo.head() {
        Ok(head) => Ok(Some(head.peel_to_tree().context("peel HEAD to a tree")?)),
        Err(e) if e.code() == git2::ErrorCode::UnbornBranch => Ok(None),
        Err(e) => Err(e).context("resolve HEAD"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::path::Path;

    fn commit_all(repo: &Repository, message: &str) -> git2::Oid {
        let mut index = repo.index().unwrap();
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents).unwrap()
    }

    /// `path` now goes through `paths::resolve`, which requires an absolute
    /// path to be whitelisted in `<appdata>/solx-git.json`. Points
    /// `SOLX_APPDATA_DIR` at a fresh temp dir and whitelists `dir`; keep the
    /// returned `TempDir` alive for the duration of the test.
    fn allow(dir: &Path) -> tempfile::TempDir {
        let appdata = tempfile::tempdir().unwrap();
        std::fs::write(
            appdata.path().join("solx-git.json"),
            format!(r#"{{"allowed_paths":["{}"]}}"#, dir.display().to_string().replace('\\', "\\\\")),
        )
        .unwrap();
        std::env::set_var("SOLX_APPDATA_DIR", appdata.path());
        appdata
    }

    fn clear_env() {
        std::env::remove_var("SOLX_APPDATA_DIR");
    }

    #[test]
    fn defaults_match_documented_behavior() {
        let params: PatchParams = serde_json::from_str(r#"{"path":"."}"#).unwrap();
        assert!(!params.staged_only);
        assert!(params.include_untracked);
        assert_eq!(params.context_lines, None);
    }

    #[test]
    #[serial]
    fn diffs_working_tree_against_head_including_untracked() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        commit_all(&repo, "initial");

        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "new file\n").unwrap();
        let _appdata = allow(dir.path());

        let result = run(PatchParams {
            path: dir.path().display().to_string(),
            staged_only: false,
            include_untracked: true,
            context_lines: None,
        })
        .unwrap();

        assert!(result.patch.contains("a.txt"), "{}", result.patch);
        assert!(result.patch.contains("new.txt"), "{}", result.patch);
        assert!(result.patch.contains("+two"), "{}", result.patch);
        assert_eq!(result.files_changed, 2);
        assert_eq!(result.insertions, 2);
        assert_eq!(result.deletions, 0);
        clear_env();
    }

    #[test]
    #[serial]
    fn staged_only_excludes_untracked_and_unstaged_changes() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        commit_all(&repo, "initial");

        // Unstaged modification -- must not appear in a staged_only diff.
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        // Staged addition -- must appear.
        std::fs::write(dir.path().join("staged.txt"), "staged content\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("staged.txt")).unwrap();
        index.write().unwrap();
        let _appdata = allow(dir.path());

        let result = run(PatchParams {
            path: dir.path().display().to_string(),
            staged_only: true,
            include_untracked: true,
            context_lines: None,
        })
        .unwrap();

        assert!(result.patch.contains("staged.txt"), "{}", result.patch);
        assert!(!result.patch.contains("a.txt"), "{}", result.patch);
        assert_eq!(result.files_changed, 1);
        clear_env();
    }

    #[test]
    #[serial]
    fn unborn_head_diffs_against_an_empty_tree() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let _appdata = allow(dir.path());

        let result = run(PatchParams {
            path: dir.path().display().to_string(),
            staged_only: false,
            include_untracked: true,
            context_lines: None,
        })
        .unwrap();

        assert!(result.patch.contains("a.txt"), "{}", result.patch);
        assert_eq!(result.files_changed, 1);
        assert_eq!(result.insertions, 1);
        clear_env();
    }

    #[test]
    #[serial]
    fn an_absolute_path_outside_the_whitelist_is_rejected() {
        clear_env();
        let appdata = tempfile::tempdir().unwrap();
        std::env::set_var("SOLX_APPDATA_DIR", appdata.path());

        let dir = tempfile::tempdir().unwrap();
        let err = run(PatchParams {
            path: dir.path().display().to_string(),
            staged_only: false,
            include_untracked: true,
            context_lines: None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("allowed_paths"), "{err}");
        clear_env();
    }
}
