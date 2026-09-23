//! Resolve the `path` param every solx-git action takes into an absolute
//! filesystem path, and enforce the sandboxing rule that goes with it.
//!
//! - A **relative** path resolves strictly inside solx-core's file store
//!   root (`files_directory` in `solx-config.json`, defaulting to
//!   `<appdata>/files` — see `ConfigService::files_dir()` in solx-core's
//!   `solx-config` crate). No `..` or rooted component is allowed to escape
//!   it — the string arrives in an action's params payload, so it is
//!   checked rather than trusted, the same guard `solx-quickjs`'s
//!   `staged_destination` uses for staged JS sources.
//! - An **absolute** path must be inside (or equal to) one of the
//!   directories listed in `<appdata>/solx-git.json`'s `allowed_paths` — a
//!   repo that already lives somewhere outside the file store, which an
//!   operator has to opt into explicitly. A missing file, missing field, or
//!   no matching entry all mean "not allowed": this is a deny-by-default
//!   allowlist, mirroring `solx-config.json`'s own `allowed_base_urls`.
//!
//! Both config files are re-read on every call — each solx-git invocation
//! is a short-lived one-shot process, so there is no in-process cache to
//! invalidate.

use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Result};
use serde::Deserialize;
use solx_package_log::server::appdata_dir;

#[derive(Debug, Deserialize, Default)]
struct GitWhitelist {
    #[serde(default)]
    allowed_paths: Vec<String>,
}

pub fn resolve(input: &str) -> Result<PathBuf> {
    if input.trim().is_empty() {
        return Err(anyhow!("missing required param: path"));
    }
    let candidate = PathBuf::from(input);

    if candidate.is_absolute() {
        resolve_absolute(&candidate)
    } else {
        resolve_relative(&candidate)
    }
}

fn resolve_relative(relative: &Path) -> Result<PathBuf> {
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(anyhow!(
                "relative path must have no '..' or root component: {}",
                relative.display()
            ));
        }
    }
    Ok(files_dir().join(relative))
}

fn resolve_absolute(candidate: &Path) -> Result<PathBuf> {
    let candidate_canon = weakly_canonicalize(candidate);
    let whitelist = load_whitelist();

    let allowed = whitelist
        .allowed_paths
        .iter()
        .any(|entry| candidate_canon.starts_with(weakly_canonicalize(Path::new(entry))));

    if allowed {
        Ok(candidate.to_path_buf())
    } else {
        Err(anyhow!(
            "'{}' is an absolute path outside the solx files directory and is not listed in {}'s allowed_paths",
            candidate.display(),
            whitelist_path().display(),
        ))
    }
}

/// solx-core's file store root. Raw JSON read of `solx-config.json`'s
/// `files_directory` field — this crate doesn't depend on the
/// `solx-config` crate, same reasoning as
/// `solx_package_log::server::ServerConfig`'s own token-from-config-file
/// fallback — defaulting to `<appdata>/files` to match
/// `ConfigService::files_dir()`.
fn files_dir() -> PathBuf {
    let dir = fs::read_to_string(appdata_dir().join("solx-config.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("files_directory").and_then(|f| f.as_str()).map(str::to_string))
        .filter(|s| !s.trim().is_empty());

    match dir {
        Some(dir) => PathBuf::from(dir),
        None => appdata_dir().join("files"),
    }
}

fn whitelist_path() -> PathBuf {
    appdata_dir().join("solx-git.json")
}

fn load_whitelist() -> GitWhitelist {
    fs::read_to_string(whitelist_path()).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

/// Canonicalize as much of `path` as actually exists on disk, then
/// re-append whatever tail doesn't (a fresh `git-clone` destination, for
/// instance, doesn't exist yet). Comparing two paths that went through the
/// *same* function is what makes the `starts_with` check in
/// `resolve_absolute` reliable on Windows: plain `fs::canonicalize`
/// prepends a `\\?\` verbatim prefix, and comparing a canonicalized path
/// against a merely-lexically-normalized one would spuriously fail to
/// match even when they name the same location.
fn weakly_canonicalize(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut tail: Vec<&OsStr> = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(mut canon) => {
                for part in tail.iter().rev() {
                    canon.push(part);
                }
                return canon;
            }
            Err(_) => match (existing.parent(), existing.file_name()) {
                (Some(parent), Some(name)) => {
                    tail.push(name);
                    existing = parent;
                }
                // Nothing on disk to anchor against at all (e.g. a bare,
                // nonexistent drive root) -- fall back to a purely
                // syntactic normalization.
                _ => return normalize_lexically(path),
            },
        }
    }
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn clear_env() {
        std::env::remove_var("SOLX_APPDATA_DIR");
    }

    #[test]
    #[serial]
    fn relative_path_resolves_under_the_default_files_dir_when_unconfigured() {
        clear_env();
        let appdata = tempfile::tempdir().unwrap();
        std::env::set_var("SOLX_APPDATA_DIR", appdata.path());

        let resolved = resolve("myrepo/sub").unwrap();
        assert_eq!(resolved, appdata.path().join("files").join("myrepo").join("sub"));
        clear_env();
    }

    #[test]
    #[serial]
    fn relative_path_resolves_under_a_configured_files_directory() {
        clear_env();
        let appdata = tempfile::tempdir().unwrap();
        let files_root = tempfile::tempdir().unwrap();
        std::fs::write(
            appdata.path().join("solx-config.json"),
            format!(r#"{{"files_directory":"{}"}}"#, files_root.path().display().to_string().replace('\\', "\\\\")),
        )
        .unwrap();
        std::env::set_var("SOLX_APPDATA_DIR", appdata.path());

        let resolved = resolve("myrepo").unwrap();
        assert_eq!(resolved, files_root.path().join("myrepo"));
        clear_env();
    }

    #[test]
    #[serial]
    fn relative_path_cannot_escape_with_dotdot() {
        clear_env();
        let appdata = tempfile::tempdir().unwrap();
        std::env::set_var("SOLX_APPDATA_DIR", appdata.path());

        let err = resolve("../escape").unwrap_err();
        assert!(err.to_string().contains(".."), "{err}");
        clear_env();
    }

    #[test]
    #[serial]
    fn absolute_path_outside_any_whitelist_entry_is_rejected() {
        clear_env();
        let appdata = tempfile::tempdir().unwrap();
        std::env::set_var("SOLX_APPDATA_DIR", appdata.path());

        let outside = tempfile::tempdir().unwrap();
        let err = resolve(&outside.path().display().to_string()).unwrap_err();
        assert!(err.to_string().contains("allowed_paths"), "{err}");
        clear_env();
    }

    #[test]
    #[serial]
    fn absolute_path_inside_a_whitelisted_directory_is_allowed() {
        clear_env();
        let appdata = tempfile::tempdir().unwrap();
        let allowed_root = tempfile::tempdir().unwrap();
        std::fs::write(
            appdata.path().join("solx-git.json"),
            format!(r#"{{"allowed_paths":["{}"]}}"#, allowed_root.path().display().to_string().replace('\\', "\\\\")),
        )
        .unwrap();
        std::env::set_var("SOLX_APPDATA_DIR", appdata.path());

        // The whitelisted root itself...
        resolve(&allowed_root.path().display().to_string()).unwrap();
        // ...and a nested repo underneath it (subtree match).
        let nested = allowed_root.path().join("some-repo");
        std::fs::create_dir_all(&nested).unwrap();
        resolve(&nested.display().to_string()).unwrap();
        clear_env();
    }
}
