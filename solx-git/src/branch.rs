//! `git-branch`: create a branch and/or switch the repo's current branch.
//!
//! `create` (default `true`) means "create it if it doesn't exist yet" —
//! not "always create." A branch that already exists is just switched to,
//! unless `force` asks to also move it. `path` resolves the same way
//! `git-clone`/`git-patch` do — see `paths::resolve`.

use anyhow::{anyhow, Context, Result};
use git2::build::CheckoutBuilder;
use git2::{Commit, Repository};
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct BranchParams {
    pub path: String,
    /// Branch name to switch to (and, per `create`, to make first).
    pub branch: String,
    /// Create `branch` at `start_point` if it doesn't exist yet. If `false`
    /// and the branch is missing, this is an error instead of an implicit
    /// create.
    #[serde(default = "default_true")]
    pub create: bool,
    /// Revspec (branch, tag, sha, `HEAD`, ...) the branch is created at.
    /// Only consulted when the branch is actually being created or moved.
    /// Defaults to `HEAD`.
    #[serde(default)]
    pub start_point: Option<String>,
    /// If `branch` already exists, move it to `start_point` instead of
    /// leaving it where it is. Ignored when the branch is being newly
    /// created (it is always placed at `start_point` in that case).
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Serialize)]
pub struct BranchResult {
    pub path: String,
    pub branch: String,
    pub commit: String,
    /// `true` if this call created the branch, `false` if it already
    /// existed (moved or not, per `force`).
    pub created: bool,
}

pub fn run(params: BranchParams) -> Result<BranchResult> {
    if params.branch.trim().is_empty() {
        return Err(anyhow!("missing required param: branch"));
    }
    let repo_path = crate::paths::resolve(&params.path)?;
    let repo = Repository::open(&repo_path).with_context(|| format!("open repo at {}", repo_path.display()))?;

    let local_ref = format!("refs/heads/{}", params.branch);
    let exists = repo.find_reference(&local_ref).is_ok();

    let created = if !exists {
        if !params.create {
            return Err(anyhow!("branch '{}' does not exist and create=false", params.branch));
        }
        let start = params.start_point.as_deref().unwrap_or("HEAD");
        let commit = resolve_to_commit(&repo, start)?;
        repo.branch(&params.branch, &commit, false).with_context(|| format!("create branch '{}'", params.branch))?;
        true
    } else {
        if params.force {
            let start = params.start_point.as_deref().unwrap_or("HEAD");
            let commit = resolve_to_commit(&repo, start)?;
            let mut git_ref = repo.find_reference(&local_ref)?;
            git_ref
                .set_target(commit.id(), &format!("git-branch: force move to '{start}'"))
                .with_context(|| format!("move branch '{}'", params.branch))?;
        }
        false
    };

    repo.set_head(&local_ref).with_context(|| format!("set HEAD to '{local_ref}'"))?;
    let mut checkout = CheckoutBuilder::new();
    checkout.force();
    repo.checkout_head(Some(&mut checkout)).context("checkout branch")?;

    let commit = repo.head()?.peel_to_commit().context("resolve HEAD commit after checkout")?.id().to_string();

    Ok(BranchResult { path: repo_path.display().to_string(), branch: params.branch, commit, created })
}

fn resolve_to_commit<'a>(repo: &'a Repository, revspec: &str) -> Result<Commit<'a>> {
    let (object, _reference) = repo.revparse_ext(revspec).with_context(|| format!("resolve '{revspec}'"))?;
    object.peel_to_commit().with_context(|| format!("'{revspec}' does not resolve to a commit"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Signature;
    use serial_test::serial;
    use std::path::Path;

    fn init_repo_with_commit(dir: &Path) -> Repository {
        let repo = Repository::init(dir).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("a.txt")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = Signature::now("Test", "test@example.com").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[]).unwrap();
        }
        repo
    }

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
    #[serial]
    fn creates_and_switches_to_a_new_branch_by_default() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());
        let _appdata = allow(dir.path());

        let result = run(BranchParams {
            path: dir.path().display().to_string(),
            branch: "feature".to_string(),
            create: true,
            start_point: None,
            force: false,
        })
        .unwrap();

        assert!(result.created);
        assert_eq!(result.branch, "feature");

        let repo = Repository::open(dir.path()).unwrap();
        assert_eq!(crate::git_util::current_branch_name(&repo).as_deref(), Some("feature"));
        clear_env();
    }

    #[test]
    #[serial]
    fn switching_to_an_existing_branch_does_not_move_it() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("other", &head_commit, false).unwrap();
        let _appdata = allow(dir.path());

        let result = run(BranchParams {
            path: dir.path().display().to_string(),
            branch: "other".to_string(),
            create: true,
            start_point: None,
            force: false,
        })
        .unwrap();

        assert!(!result.created);
        assert_eq!(result.commit, head_commit.id().to_string());
        clear_env();
    }

    #[test]
    #[serial]
    fn missing_branch_without_create_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());
        let _appdata = allow(dir.path());

        let err = run(BranchParams {
            path: dir.path().display().to_string(),
            branch: "does-not-exist".to_string(),
            create: false,
            start_point: None,
            force: false,
        })
        .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
        clear_env();
    }

    #[test]
    #[serial]
    fn force_moves_an_existing_branch_to_start_point() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_repo_with_commit(dir.path());
        let first_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("other", &first_commit, false).unwrap();

        // Advance main so `other` and HEAD diverge, then force `other` to HEAD.
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = Signature::now("Test", "test@example.com").unwrap();
        let second_commit_id = repo.commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&first_commit]).unwrap();
        let _appdata = allow(dir.path());

        let result = run(BranchParams {
            path: dir.path().display().to_string(),
            branch: "other".to_string(),
            create: true,
            start_point: Some(second_commit_id.to_string()),
            force: true,
        })
        .unwrap();

        assert!(!result.created);
        assert_eq!(result.commit, second_commit_id.to_string());
        clear_env();
    }
}
