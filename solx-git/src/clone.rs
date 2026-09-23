//! `git-clone`: clone a repository into `path`, or — if a repo is already
//! checked out there — fetch and sync it to the requested branch/rev
//! instead of failing. Either way the working tree ends up matching what
//! was asked for.
//!
//! Checkout is always a **hard** checkout (`CheckoutBuilder::force`): any
//! local modifications already sitting at `path` are discarded. This
//! action exists to reliably reproduce a source tree, not to preserve local
//! edits — a caller that wants those has already read them out first, e.g.
//! with `git-patch`.

use anyhow::{anyhow, Context, Result};
use git2::build::{CheckoutBuilder, RepoBuilder};
use git2::{FetchOptions, Repository};
use serde::{Deserialize, Serialize};

use crate::auth;

#[derive(Debug, Deserialize)]
pub struct CloneParams {
    /// Remote URL — `https://...` or an SSH form (`git@host:org/repo.git`,
    /// `ssh://...`). See `auth` for how credentials are resolved.
    pub url: String,
    /// Where the repo is cloned into (or, if it already holds a checkout,
    /// synced in place). A relative path resolves inside solx-core's file
    /// store; an absolute path must be listed in `<appdata>/solx-git.json`'s
    /// `allowed_paths` — see `paths::resolve`.
    pub path: String,
    /// Branch to check out. Ignored if `rev` is also set. Defaults to the
    /// remote's own default branch on a fresh clone, or whatever branch is
    /// already checked out when syncing an existing one.
    #[serde(default)]
    pub branch: Option<String>,
    /// A specific commit or tag to check out (detached HEAD unless it
    /// happens to resolve to a branch tip). Takes priority over `branch`.
    #[serde(default)]
    pub rev: Option<String>,
    /// Shallow-clone / shallow-fetch depth. Omit for a full history clone.
    #[serde(default)]
    pub depth: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct CloneResult {
    pub path: String,
    pub url: String,
    pub branch: Option<String>,
    pub commit: String,
    /// `true` for a fresh clone, `false` when `path` already held a repo
    /// that was fetched and re-synced instead.
    pub cloned: bool,
}

pub fn run(params: CloneParams) -> Result<CloneResult> {
    if params.url.trim().is_empty() {
        return Err(anyhow!("missing required param: url"));
    }
    let dest = crate::paths::resolve(&params.path)?;

    let (repo, cloned) = if Repository::open(&dest).is_ok() {
        let repo = Repository::open(&dest)
            .with_context(|| format!("re-open existing repo at {}", dest.display()))?;
        fetch_origin(&repo, params.depth)?;
        (repo, false)
    } else {
        let mut builder = RepoBuilder::new();
        builder.fetch_options(fetch_options(params.depth));
        if let Some(branch) = params.branch.as_deref() {
            builder.branch(branch);
        }
        let repo = builder
            .clone(&params.url, &dest)
            .with_context(|| format!("clone {} into {}", params.url, dest.display()))?;
        (repo, true)
    };

    // A fresh clone with no `rev` override already has the right branch
    // checked out (RepoBuilder does that itself); only an explicit `rev`,
    // or syncing an existing checkout, needs an extra checkout step.
    if let Some(rev) = params.rev.as_deref() {
        checkout_rev(&repo, rev)?;
    } else if !cloned {
        checkout_branch(&repo, params.branch.as_deref())?;
    }

    let commit = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .context("resolve HEAD commit after checkout")?
        .id()
        .to_string();
    let branch = crate::git_util::current_branch_name(&repo);

    Ok(CloneResult { path: dest.display().to_string(), url: params.url, branch, commit, cloned })
}

fn fetch_options(depth: Option<i32>) -> FetchOptions<'static> {
    let mut fo = FetchOptions::new();
    fo.remote_callbacks(auth::callbacks());
    if let Some(depth) = depth {
        fo.depth(depth);
    }
    fo
}

fn fetch_origin(repo: &Repository, depth: Option<i32>) -> Result<()> {
    let mut remote = repo.find_remote("origin").context("repo at dest_path has no 'origin' remote")?;
    // `iter()` yields `Result<Option<&str>, Error>` (a refspec's bytes might
    // not be valid UTF-8); a non-UTF-8 or absent entry is dropped rather
    // than failing the whole fetch over it.
    let refspecs: Vec<String> = remote
        .fetch_refspecs()
        .context("read origin's refspecs")?
        .iter()
        .filter_map(|r| r.ok().flatten().map(str::to_string))
        .collect();
    let mut fo = fetch_options(depth);
    remote.fetch(&refspecs, Some(&mut fo), None).context("fetch origin")?;
    Ok(())
}

fn checkout_rev(repo: &Repository, rev: &str) -> Result<()> {
    let (object, reference) = repo.revparse_ext(rev).with_context(|| format!("resolve rev '{rev}'"))?;
    let mut checkout = CheckoutBuilder::new();
    checkout.force();
    repo.checkout_tree(&object, Some(&mut checkout)).with_context(|| format!("checkout '{rev}'"))?;
    match reference {
        Some(r) => {
            // `Reference::name()` is fallible (non-UTF-8 refnames) rather
            // than `Option`-returning — a real refname from `revparse_ext`
            // is always plain ASCII in practice, so this only matters as a
            // type on paper.
            let name = r.name().with_context(|| format!("reference for '{rev}' has a non-UTF-8 name"))?;
            repo.set_head(name).with_context(|| format!("set HEAD to '{name}'"))?;
        }
        None => repo.set_head_detached(object.id()).with_context(|| format!("detach HEAD at '{rev}'"))?,
    }
    Ok(())
}

fn checkout_branch(repo: &Repository, branch: Option<&str>) -> Result<()> {
    if let Some(branch) = branch {
        let local_ref = format!("refs/heads/{branch}");
        if repo.find_reference(&local_ref).is_err() {
            // No local branch yet — create one tracking origin/<branch>.
            let remote_ref = format!("refs/remotes/origin/{branch}");
            let commit = repo
                .find_reference(&remote_ref)
                .with_context(|| format!("branch '{branch}' not found locally or on origin"))?
                .peel_to_commit()
                .with_context(|| format!("resolve origin/{branch} to a commit"))?;
            repo.branch(branch, &commit, false).with_context(|| format!("create local branch '{branch}'"))?;
        }
        repo.set_head(&local_ref).with_context(|| format!("set HEAD to '{local_ref}'"))?;
    }

    let mut checkout = CheckoutBuilder::new();
    checkout.force();
    repo.checkout_head(Some(&mut checkout)).context("checkout HEAD")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Signature;
    use serial_test::serial;
    use std::path::Path;

    /// Init a repo at `dir` with one commit (`README`), returning that
    /// commit's sha. Local-path clones (no network) exercise the same
    /// RepoBuilder/checkout code a real HTTPS/SSH clone would.
    fn init_source_repo_with_commit(dir: &Path) -> String {
        let repo = Repository::init(dir).unwrap();
        std::fs::write(dir.join("README"), "hello\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[]).unwrap();
        let sha = repo.head().unwrap().peel_to_commit().unwrap().id().to_string();
        sha
    }

    fn params(url: &Path, dest: &Path) -> CloneParams {
        CloneParams { url: url.display().to_string(), path: dest.display().to_string(), branch: None, rev: None, depth: None }
    }

    /// `path` now goes through `paths::resolve`, which requires an absolute
    /// destination to be whitelisted in `<appdata>/solx-git.json`. Points
    /// `SOLX_APPDATA_DIR` at a fresh temp dir and whitelists `dest_root`;
    /// keep the returned `TempDir` alive for the duration of the test.
    fn allow(dest_root: &Path) -> tempfile::TempDir {
        let appdata = tempfile::tempdir().unwrap();
        std::fs::write(
            appdata.path().join("solx-git.json"),
            format!(r#"{{"allowed_paths":["{}"]}}"#, dest_root.display().to_string().replace('\\', "\\\\")),
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
    fn clones_a_local_repo_fresh() {
        let src = tempfile::tempdir().unwrap();
        let expected_commit = init_source_repo_with_commit(src.path());
        let dest = tempfile::tempdir().unwrap();
        let dest_path = dest.path().join("checkout");
        let _appdata = allow(dest.path());

        let result = run(params(src.path(), &dest_path)).unwrap();

        assert!(result.cloned);
        assert_eq!(result.commit, expected_commit);
        assert!(dest_path.join("README").exists());
        clear_env();
    }

    #[test]
    #[serial]
    fn re_running_against_an_existing_checkout_syncs_instead_of_failing() {
        let src = tempfile::tempdir().unwrap();
        let expected_commit = init_source_repo_with_commit(src.path());
        let dest = tempfile::tempdir().unwrap();
        let dest_path = dest.path().join("checkout");
        let _appdata = allow(dest.path());

        run(params(src.path(), &dest_path)).unwrap();
        let second = run(params(src.path(), &dest_path)).unwrap();

        assert!(!second.cloned);
        assert_eq!(second.commit, expected_commit);
        clear_env();
    }

    #[test]
    #[serial]
    fn rev_pins_to_a_specific_commit_detached() {
        let src = tempfile::tempdir().unwrap();
        let repo = Repository::init(src.path()).unwrap();
        let sig = Signature::now("Test", "test@example.com").unwrap();
        let write_commit = |name: &str, content: &str| -> git2::Oid {
            std::fs::write(src.path().join(name), content).unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new(name)).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
            let parents: Vec<&git2::Commit> = parent.iter().collect();
            repo.commit(Some("HEAD"), &sig, &sig, "c", &tree, &parents).unwrap()
        };
        let first = write_commit("a.txt", "one\n");
        let _second = write_commit("a.txt", "one\ntwo\n");

        let dest = tempfile::tempdir().unwrap();
        let dest_path = dest.path().join("checkout");
        let _appdata = allow(dest.path());
        let mut p = params(src.path(), &dest_path);
        p.rev = Some(first.to_string());

        let result = run(p).unwrap();

        assert_eq!(result.commit, first.to_string());
        assert_eq!(result.branch, None);
        // Normalize line endings before comparing: a Windows git install
        // with `core.autocrlf` on rewrites LF to CRLF on checkout, which is
        // real (and correct) checkout-filter behavior, not something this
        // test cares about.
        let content = std::fs::read_to_string(dest_path.join("a.txt")).unwrap();
        assert_eq!(content.replace("\r\n", "\n"), "one\n");
        clear_env();
    }

    #[test]
    #[serial]
    fn missing_url_is_a_clear_error() {
        clear_env();
        let dest = tempfile::tempdir().unwrap();
        let mut p = params(Path::new("some/repo"), &dest.path().join("checkout"));
        p.url.clear();
        let err = run(p).unwrap_err();
        assert!(err.to_string().contains("url"), "{err}");
    }

    #[test]
    #[serial]
    fn an_absolute_dest_path_outside_the_whitelist_is_rejected() {
        clear_env();
        let appdata = tempfile::tempdir().unwrap();
        std::env::set_var("SOLX_APPDATA_DIR", appdata.path());

        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let err = run(params(src.path(), &dest.path().join("checkout"))).unwrap_err();
        assert!(err.to_string().contains("allowed_paths"), "{err}");
        clear_env();
    }
}
