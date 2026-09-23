//! Git credentials callback, shared by every operation that talks to a
//! remote (`git-clone` today; a future `git-fetch`/`git-push` would reuse
//! it too).
//!
//! Resolution order, tried only for the credential kind `libgit2` actually
//! asks for (`allowed_types`):
//!
//! - SSH: `GIT_SSH_KEY_PATH` (+ optional `GIT_SSH_KEY_PASSPHRASE`) if set,
//!   otherwise the running ssh-agent.
//! - HTTPS: `GIT_USERNAME`/`GIT_PASSWORD` if both are set, otherwise
//!   `GIT_TOKEN` alone (sent as the username with an empty password — the
//!   convention GitHub/GitLab personal-access tokens expect over HTTPS).
//! - Anything else (or nothing configured): `Cred::default()`, which covers
//!   a public repo needing no auth at all and is also libgit2's own
//!   fallback.
//!
//! None of this is a hard requirement — a clone of a public HTTPS repo
//! never triggers the callback in the first place.

use git2::{Cred, CredentialType, RemoteCallbacks};
use std::path::Path;

pub fn callbacks<'a>() -> RemoteCallbacks<'a> {
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(|_url, username_from_url, allowed_types| {
        let username = username_from_url.unwrap_or("git");

        if allowed_types.contains(CredentialType::SSH_KEY) {
            if let Ok(key_path) = std::env::var("GIT_SSH_KEY_PATH") {
                let passphrase = std::env::var("GIT_SSH_KEY_PASSPHRASE").ok();
                return Cred::ssh_key(username, None, Path::new(&key_path), passphrase.as_deref());
            }
            if let Ok(cred) = Cred::ssh_key_from_agent(username) {
                return Ok(cred);
            }
        }

        if allowed_types.contains(CredentialType::USER_PASS_PLAINTEXT) {
            let user = std::env::var("GIT_USERNAME");
            let pass = std::env::var("GIT_PASSWORD");
            if let (Ok(user), Ok(pass)) = (&user, &pass) {
                return Cred::userpass_plaintext(user, pass);
            }
            if let Ok(token) = std::env::var("GIT_TOKEN") {
                return Cred::userpass_plaintext(&token, "");
            }
        }

        Cred::default()
    });
    callbacks
}
