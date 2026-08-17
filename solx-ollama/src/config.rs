//! Base-URL and bearer-token resolution.
//!
//! A guest cannot read its own `action_config` — the host never exposes it,
//! only the secret *keys* inside it, and then only indirectly through
//! `get_secret`. So the only ambient configuration channels available here
//! are `/builtin/env/get_env` and `/builtin/secrets/get_secret`.

use serde_json::{json, Value};

use crate::host::{str_param, Host};

pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";
pub const DEFAULT_PORT: &str = "11434";
pub const HOST_ENV_KEY: &str = "OLLAMA_HOST";
/// Secret name for the bearer token, used both by the convention lookup and
/// by the `set_api_key` bootstrap action.
pub const DEFAULT_SECRET: &str = "OLLAMA_API_KEY";

// ── base url ─────────────────────────────────────────────────────────────────

/// `params.base_url` → `get_env("OLLAMA_HOST")` → [`DEFAULT_BASE_URL`].
pub fn resolve_base_url(host: &dyn Host, params: &Value) -> String {
    let raw = str_param(params, "base_url")
        .or_else(|| env_lookup(host, HOST_ENV_KEY))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    normalize_base_url(&raw)
}

/// Turn whatever `OLLAMA_HOST` happens to contain into a dialable origin.
///
/// The env var is conventionally written the way `ollama serve` wants to
/// *bind* — scheme-less, and often `0.0.0.0` — which is not directly usable
/// as a request URL.
///
/// ```text
/// "127.0.0.1:11434"   -> "http://127.0.0.1:11434"
/// "localhost"         -> "http://localhost:11434"
/// "0.0.0.0:11434"     -> "http://127.0.0.1:11434"   (bind addr, not dialable)
/// "http://box:1234/"  -> "http://box:1234"
/// "https://ollama.com"-> "https://ollama.com"       (no port appended)
/// ""                  -> DEFAULT_BASE_URL
/// ```
pub fn normalize_base_url(raw: &str) -> String {
    let raw = raw.trim().trim_end_matches('/');
    if raw.is_empty() {
        return DEFAULT_BASE_URL.to_string();
    }

    let (scheme, rest) = match raw.find("://") {
        Some(i) => (&raw[..i + 3], &raw[i + 3..]),
        None => ("http://", raw),
    };
    if rest.is_empty() {
        return DEFAULT_BASE_URL.to_string();
    }

    // Split off any path so the host:port rewrite below only inspects the
    // authority. Anything after the first `/` is preserved verbatim.
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };

    // 0.0.0.0 is a wildcard bind address; connecting to it fails on Windows.
    let authority = if let Some(port) = authority.strip_prefix("0.0.0.0") {
        format!("127.0.0.1{port}")
    } else if authority == "[::]" {
        "127.0.0.1".to_string()
    } else {
        authority.to_string()
    };

    // Append the default port only for a bare host that was written without a
    // scheme — `https://ollama.com` must stay portless.
    let has_port = match authority.rfind(':') {
        // Guard against an IPv6 literal, where the last `:` is inside `[...]`.
        Some(i) => !authority.ends_with(']') && authority[i + 1..].chars().all(|c| c.is_ascii_digit()),
        None => false,
    };
    let authority = if !has_port && scheme == "http://" && !raw.contains("://") {
        format!("{authority}:{DEFAULT_PORT}")
    } else {
        authority
    };

    format!("{scheme}{authority}{path}")
}

/// `Some` only for a non-empty string value. A missing key returns
/// `{"value": null}` rather than failing, so this must filter.
pub fn env_lookup(host: &dyn Host, key: &str) -> Option<String> {
    let call = host.exec("/builtin/env/get_env", &json!({ "key": key })).ok()?;
    if !call.success {
        return None;
    }
    call.result
        .get("value")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ── auth ─────────────────────────────────────────────────────────────────────

/// Resolve a bearer token, first hit wins:
///
/// 1. `params.api_key` — inline.
/// 2. `params.auth_secret_name` — explicit. **Fatal on failure**: the caller
///    named a secret, so silently falling through would send the request
///    unauthenticated.
/// 3. `get_secret("OLLAMA_API_KEY")` — the convention. Best-effort and
///    silent, which is what makes the zero-config local case work.
/// 4. `get_env("OLLAMA_API_KEY")` — for operators who prefer `env_mappings`
///    over the keyring.
pub fn resolve_auth(host: &dyn Host, params: &Value) -> Result<Option<String>, String> {
    if let Some(k) = str_param(params, "api_key") {
        return Ok(Some(k));
    }

    if let Some(name) = str_param(params, "auth_secret_name") {
        return match read_secret(host, &name) {
            Ok(Some(v)) => Ok(Some(v)),
            Ok(None) => Err(format!(
                "secret {name} has a key configured but no stored value; \
                 run exec /packages/solx-ollama/ollama-set-api-key to store one"
            )),
            Err(e) => Err(format!("auth_secret_name {name}: {e}")),
        };
    }

    // Both failure modes mean "no auth configured", silently:
    //   Err       -> no key for this name in the action's action_config.secrets
    //   Ok(None)  -> key configured, but nothing stored under that name yet
    if let Ok(Some(v)) = read_secret(host, DEFAULT_SECRET) {
        return Ok(Some(v));
    }
    if let Some(v) = env_lookup(host, DEFAULT_SECRET) {
        return Ok(Some(v));
    }
    Ok(None)
}

/// `Ok(None)` = no usable value. `Err` = no key configured for this name.
pub fn read_secret(host: &dyn Host, name: &str) -> Result<Option<String>, String> {
    let call = host.exec("/builtin/secrets/get_secret", &json!({ "name": name }))?;
    if !call.success {
        return Err(call
            .message
            .unwrap_or_else(|| "get_secret failed".to_string()));
    }
    Ok(call
        .result
        .get("value")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string))
}
