//! The endpoint table: the single place that knows how each `fn_name` maps
//! onto an Ollama REST call.
//!
//! Adding an action is a row here plus a row in `install.solx`; nothing else
//! in the crate needs to change.
//!
//! Two invariants this table encodes, both asserted in the tests:
//!
//! * `"stream"` never appears in a `passthrough` list, so a caller cannot
//!   re-enable streaming. `force_stream_false` then writes it unconditionally.
//!   A WASM action returns exactly one value and has no channel to emit
//!   chunks, so a streamed NDJSON response would arrive as an unparseable
//!   body and collapse to `Value::Null`.
//! * `max_timeout` is the inner (per-HTTP-request) ceiling. `install.solx`
//!   sets each action's outer `action_config.timeout_secs` to
//!   `max_timeout + TIMEOUT_HEADROOM_SECS`, so the outer wall-clock budget can
//!   never fire before the inner one for any caller-supplied value.

use serde_json::{Map, Value};

/// Headroom between the inner HTTP timeout ceiling and the outer wasm
/// `action_config.timeout_secs`. Covers first-call component compilation
/// (wasmtime compiles on a cache miss) plus the `get_env` / `get_secret`
/// round trips.
pub const TIMEOUT_HEADROOM_SECS: u64 = 60;

pub struct Endpoint {
    /// The action row's `fn_name`, which the host passes to the guest's
    /// `run(action-name, params)` export.
    pub fn_name: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    /// Params that must be present and non-null.
    pub required: &'static [&'static str],
    /// Optional params copied into the request body verbatim when present.
    pub passthrough: &'static [&'static str],
    /// Whether to inject `"stream": false`. False for endpoints that have no
    /// streaming mode at all (`/api/show`, `/api/copy`, ...), where sending
    /// the field would just be noise.
    pub force_stream_false: bool,
    pub default_timeout: u64,
    pub max_timeout: u64,
}

impl Endpoint {
    /// The value `install.solx` must set as this action's
    /// `action_config.timeout_secs`.
    pub fn outer_timeout(&self) -> u64 {
        self.max_timeout + TIMEOUT_HEADROOM_SECS
    }
}

/// Common params handled by `request.rs` rather than forwarded to Ollama.
/// Listed here so `install.solx` schema generation and the tests agree on the
/// set.
pub const COMMON_PARAMS: &[&str] = &[
    "base_url",
    "api_key",
    "auth_secret_name",
    "headers",
    "timeout_secs",
];

pub const SET_API_KEY_FN: &str = "set_api_key";

pub static ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        fn_name: "generate",
        method: "POST",
        path: "/api/generate",
        required: &["model"],
        passthrough: &[
            "prompt",
            "suffix",
            "images",
            "format",
            "system",
            "template",
            "raw",
            "think",
            "keep_alive",
            "options",
            "context",
            "logprobs",
            "top_logprobs",
        ],
        force_stream_false: true,
        default_timeout: 300,
        max_timeout: 1800,
    },
    Endpoint {
        fn_name: "chat",
        method: "POST",
        path: "/api/chat",
        required: &["model", "messages"],
        passthrough: &[
            "tools",
            "format",
            "options",
            "think",
            "keep_alive",
            "logprobs",
            "top_logprobs",
        ],
        force_stream_false: true,
        default_timeout: 300,
        max_timeout: 1800,
    },
    Endpoint {
        fn_name: "embed",
        method: "POST",
        path: "/api/embed",
        required: &["model", "input"],
        passthrough: &["truncate", "dimensions", "keep_alive", "options"],
        force_stream_false: false,
        default_timeout: 120,
        max_timeout: 600,
    },
    Endpoint {
        fn_name: "list_models",
        method: "GET",
        path: "/api/tags",
        required: &[],
        passthrough: &[],
        force_stream_false: false,
        default_timeout: 30,
        max_timeout: 300,
    },
    Endpoint {
        fn_name: "show_model",
        method: "POST",
        path: "/api/show",
        required: &["model"],
        passthrough: &["verbose"],
        force_stream_false: false,
        default_timeout: 60,
        max_timeout: 300,
    },
    Endpoint {
        fn_name: "ps",
        method: "GET",
        path: "/api/ps",
        required: &[],
        passthrough: &[],
        force_stream_false: false,
        default_timeout: 30,
        max_timeout: 300,
    },
    Endpoint {
        fn_name: "version",
        method: "GET",
        path: "/api/version",
        required: &[],
        passthrough: &[],
        force_stream_false: false,
        default_timeout: 15,
        max_timeout: 300,
    },
    Endpoint {
        fn_name: "pull_model",
        method: "POST",
        path: "/api/pull",
        required: &["model"],
        passthrough: &["insecure"],
        force_stream_false: true,
        default_timeout: 3600,
        max_timeout: 7200,
    },
    Endpoint {
        fn_name: "push_model",
        method: "POST",
        path: "/api/push",
        required: &["model"],
        passthrough: &["insecure"],
        force_stream_false: true,
        default_timeout: 3600,
        max_timeout: 7200,
    },
    Endpoint {
        fn_name: "create_model",
        method: "POST",
        path: "/api/create",
        required: &["model"],
        passthrough: &[
            "from",
            "files",
            "adapters",
            "template",
            "renderer",
            "parser",
            "license",
            "system",
            "parameters",
            "messages",
            "quantize",
        ],
        force_stream_false: true,
        default_timeout: 1800,
        max_timeout: 7200,
    },
    Endpoint {
        fn_name: "copy_model",
        method: "POST",
        path: "/api/copy",
        required: &["source", "destination"],
        passthrough: &[],
        force_stream_false: false,
        default_timeout: 60,
        max_timeout: 300,
    },
    Endpoint {
        fn_name: "delete_model",
        // DELETE with a JSON body — unusual, but it is what Ollama expects.
        method: "DELETE",
        path: "/api/delete",
        required: &["model"],
        passthrough: &[],
        force_stream_false: false,
        default_timeout: 60,
        max_timeout: 300,
    },
];

pub fn lookup(fn_name: &str) -> Option<&'static Endpoint> {
    ENDPOINTS.iter().find(|e| e.fn_name == fn_name)
}

/// Every dispatchable `fn_name`, for the `unknown_action` error payload.
pub fn known_fn_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = ENDPOINTS.iter().map(|e| e.fn_name).collect();
    names.push(SET_API_KEY_FN);
    names
}

/// The legacy `/api/embeddings` endpoint, reached by passing `legacy: true`
/// to `embed`. It takes a single `prompt` string instead of `input`, and
/// returns `embedding` (one vector) instead of `embeddings` (a list).
pub const LEGACY_EMBED_PATH: &str = "/api/embeddings";

/// Assemble the request body, or report the required params that are missing.
///
/// `Ok(None)` means "no body" — GET endpoints only.
pub fn build_body(ep: &Endpoint, params: &Value) -> Result<Option<Value>, Vec<String>> {
    let missing: Vec<String> = ep
        .required
        .iter()
        .filter(|k| params.get(**k).map_or(true, Value::is_null))
        .map(|k| (*k).to_string())
        .collect();
    if !missing.is_empty() {
        return Err(missing);
    }
    if ep.method == "GET" {
        return Ok(None);
    }

    let mut body = Map::new();
    for k in ep.required.iter().chain(ep.passthrough.iter()) {
        if let Some(v) = params.get(*k) {
            if !v.is_null() {
                body.insert((*k).to_string(), v.clone());
            }
        }
    }
    if ep.force_stream_false {
        body.insert("stream".to_string(), Value::Bool(false));
    }
    Ok(Some(Value::Object(body)))
}
