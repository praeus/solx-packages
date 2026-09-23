//! `git-commit`: stage and commit a repo's current changes.
//!
//! `all` (default `true`) stages everything before committing — new,
//! modified, and deleted files, the `git add -A` equivalent (`add_all` for
//! new/modified, `update_all` for deletions of already-tracked files).
//! Set it `false` to commit whatever is already staged instead. `path`
//! resolves the same way `git-clone`/`git-patch`/`git-branch` do — see
//! `paths::resolve`.

use anyhow::{anyhow, Context, Result};
use git2::{Commit, IndexAddOption, Repository, Signature};
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct CommitParams {
    pub path: String,
    pub message: String,
    /// Stage all changes (new/modified/deleted, tracked and untracked)
    /// before committing. Default `true`. `false` commits only what's
    /// already staged.
    #[serde(default = "default_true")]
    pub all: bool,
    /// Overrides for the commit's author/committer. Both are required
    /// together; if either is omitted, `repo.signature()` is used instead
    /// (reads the repo/global git config's `user.name`/`user.email`).
    #[serde(default)]
    pub author_name: Option<String>,
    #[serde(default)]
    pub author_email: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommitResult {
    pub path: String,
    pub commit: String,
    pub branch: Option<String>,
}

pub fn run(params: CommitParams) -> Result<CommitResult> {
    if params.message.trim().is_empty() {
        return Err(anyhow!("missing required param: message"));
    }
    let repo_path = crate::paths::resolve(&params.path)?;
    let repo = Repository::open(&repo_path).with_context(|| format!("open repo at {}", repo_path.display()))?;

    let mut index = repo.index().context("open index")?;
    if params.all {
        index.add_all(["*"].iter(), IndexAddOption::DEFAULT, None).context("stage new/modified files")?;
        index.update_all(["*"].iter(), None).context("stage deletions")?;
        index.write().context("write index")?;
    }

    let tree_id = index.write_tree().context("write tree from index")?;
    let tree = repo.find_tree(tree_id).context("look up written tree")?;

    let signature = author_signature(&repo, params.author_name.as_deref(), params.author_email.as_deref())?;

    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&Commit> = parent.iter().collect();

    let commit_id = repo
        .commit(Some("HEAD"), &signature, &signature, &params.message, &tree, &parents)
        .context("create commit")?;

    let branch = crate::git_util::current_branch_name(&repo);

    Ok(CommitResult { path: repo_path.display().to_string(), commit: commit_id.to_string(), branch })
}

fn author_signature(repo: &Repository, name: Option<&str>, email: Option<&str>) -> Result<Signature<'static>> {
    match (name, email) {
        (Some(n), Some(e)) => Signature::now(n, e).context("build commit signature from author_name/author_email"),
        _ => repo
            .signature()
            .context("no author_name/author_email given, and the repo has no user.name/user.email configured"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::path::Path;

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

    fn params(dir: &Path, message: &str) -> CommitParams {
        CommitParams {
            path: dir.display().to_string(),
            message: message.to_string(),
            all: true,
            author_name: Some("Test".to_string()),
            author_email: Some("test@example.com".to_string()),
        }
    }

    #[test]
    #[serial]
    fn commits_all_current_changes_including_untracked_as_the_initial_commit() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let _appdata = allow(dir.path());

        let result = run(params(dir.path(), "initial")).unwrap();

        assert_eq!(result.path, dir.path().display().to_string());
        assert!(!result.commit.is_empty());

        let repo = Repository::open(dir.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.id().to_string(), result.commit);
        assert_eq!(head.parent_count(), 0);
        assert_eq!(head.message().unwrap(), "initial");
        clear_env();
    }

    #[test]
    #[serial]
    fn second_commit_has_the_first_as_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let _appdata = allow(dir.path());

        let first = run(params(dir.path(), "first")).unwrap();

        std::fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        let second = run(params(dir.path(), "second")).unwrap();

        let repo = Repository::open(dir.path()).unwrap();
        let commit = repo.find_commit(git2::Oid::from_str(&second.commit).unwrap()).unwrap();
        assert_eq!(commit.parent_count(), 1);
        assert_eq!(commit.parent_id(0).unwrap().to_string(), first.commit);
        clear_env();
    }

    #[test]
    #[serial]
    fn stages_deletions_of_tracked_files_when_all_is_true() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let _appdata = allow(dir.path());
        run(params(dir.path(), "first")).unwrap();

        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        let result = run(params(dir.path(), "remove a.txt")).unwrap();

        let repo = Repository::open(dir.path()).unwrap();
        let tree = repo.find_commit(git2::Oid::from_str(&result.commit).unwrap()).unwrap().tree().unwrap();
        assert!(tree.get_name("a.txt").is_none());
        clear_env();
    }

    #[test]
    #[serial]
    fn empty_message_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        let _appdata = allow(dir.path());

        let err = run(params(dir.path(), "")).unwrap_err();
        assert!(err.to_string().contains("message"), "{err}");
        clear_env();
    }
}
