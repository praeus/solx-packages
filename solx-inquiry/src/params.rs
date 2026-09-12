//! Parse and default an `inquire` call's params. One place so `lib.rs`,
//! `terms.rs`, `search.rs`, and `summarize.rs` all agree on what a field
//! means and what it defaults to.

use serde_json::Value;

use crate::host::{str_param, Outcome};

/// Ceiling on how many search terms the model may be asked to produce.
pub const MAX_TERMS_CEILING: usize = 10;
/// Ceiling on how many hits are fed to the summarizer / returned to the caller.
pub const MAX_RESULTS_CEILING: usize = 50;
/// Ceiling on the per-LLM-call `timeout_secs` a caller may request.
pub const MAX_LLM_TIMEOUT: u64 = 900;

pub const DEFAULT_LLM_ACTION_REF: &str = "/packages/solx-ollama/ollama-chat";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Documents,
    Actions,
    Both,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Documents => "documents",
            Scope::Actions => "actions",
            Scope::Both => "both",
        }
    }

    pub fn searches_documents(&self) -> bool {
        matches!(self, Scope::Documents | Scope::Both)
    }

    pub fn searches_actions(&self) -> bool {
        matches!(self, Scope::Actions | Scope::Both)
    }
}

#[derive(Debug)]
pub struct Params {
    pub inquiry: String,
    pub model: String,
    pub scope: Scope,
    pub max_terms: usize,
    pub max_results: usize,
    pub path_prefix: Option<String>,
    pub type_ref: Option<String>,
    pub inquiry_prompt: Option<String>,
    pub summary_prompt: Option<String>,
    /// Action reference for the chat-completion action this pipeline drives
    /// for both the search-term and summary phases. Defaults to
    /// `solx-ollama`'s `ollama-chat`, but any action taking the same
    /// `{model, messages, format?}` shape and returning Ollama-style
    /// `{message: {content}}` can be swapped in.
    pub llm_action_ref: String,
    /// Passed through verbatim to the llm action call, when present.
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub auth_secret_name: Option<String>,
    pub headers: Option<Value>,
    /// Per-LLM-call timeout, forwarded to each of the two chat calls.
    pub timeout_secs: Option<u64>,
    /// Merged into every call's own `options` object (every payload this
    /// package builds sets `{"temperature": 0}`), a key at a time - a key the
    /// caller supplies overrides this pipeline's own default for it, and every
    /// other key from either side survives untouched. This is the only way to
    /// raise `num_ctx`: an Ollama model's context window is not otherwise
    /// under this pipeline's control, and a prompt that overflows it is
    /// truncated by Ollama - typically from the front, which is where the
    /// grounding rules in every system prompt here live. See
    /// [`crate::host::join_within_budget`] for the other half of that
    /// problem: bounding what this pipeline puts in the prompt in the first
    /// place, which `num_ctx` alone cannot do since it is a ceiling, not a
    /// guarantee that the caller picked one large enough.
    pub options: Option<Value>,
}

pub fn parse(params: &Value) -> Result<Params, Outcome> {
    let params = &crate::host::normalize_params(params);
    let inquiry = str_param(params, "inquiry").ok_or_else(|| {
        Outcome::fail(
            "bad_params",
            "inquire requires a non-empty inquiry",
            serde_json::json!({ "missing": ["inquiry"] }),
        )
    })?;
    let model = str_param(params, "model").ok_or_else(|| {
        Outcome::fail(
            "bad_params",
            "inquire requires a model",
            serde_json::json!({ "missing": ["model"] }),
        )
    })?;

    let scope_str = str_param(params, "scope").unwrap_or_else(|| "documents".to_string());
    let scope = match scope_str.as_str() {
        "documents" => Scope::Documents,
        "actions" => Scope::Actions,
        "both" => Scope::Both,
        other => {
            return Err(Outcome::fail(
                "bad_params",
                format!("scope must be one of documents, actions, both (got {other:?})"),
                serde_json::json!({}),
            ))
        }
    };

    let max_terms = params
        .get("max_terms")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).clamp(1, MAX_TERMS_CEILING))
        .unwrap_or(5);
    let max_results = params
        .get("max_results")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).clamp(1, MAX_RESULTS_CEILING))
        .unwrap_or(10);
    let timeout_secs = params
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .map(|n| n.clamp(1, MAX_LLM_TIMEOUT));

    Ok(Params {
        inquiry,
        model,
        scope,
        max_terms,
        max_results,
        path_prefix: str_param(params, "path_prefix"),
        type_ref: str_param(params, "type_ref"),
        inquiry_prompt: str_param(params, "inquiry_prompt"),
        summary_prompt: str_param(params, "summary_prompt"),
        llm_action_ref: str_param(params, "llm_action_ref")
            .unwrap_or_else(|| DEFAULT_LLM_ACTION_REF.to_string()),
        base_url: str_param(params, "base_url"),
        api_key: str_param(params, "api_key"),
        auth_secret_name: str_param(params, "auth_secret_name"),
        headers: params.get("headers").filter(|v| v.is_object()).cloned(),
        timeout_secs,
        options: params.get("options").filter(|v| v.is_object()).cloned(),
    })
}

/// Merge the connection-related overrides (`base_url`/`api_key`/
/// `auth_secret_name`/`headers`/`timeout_secs`/`options`) into a chat-call
/// payload that already carries `model`/`messages`/etc. Only present fields
/// are added, so the callee's own defaults (env lookup, stored secret, ...)
/// still apply when a caller didn't override anything.
pub fn apply_llm_overrides(mut payload: Value, p: &Params) -> Value {
    let obj = payload.as_object_mut().expect("payload must be an object");
    if let Some(v) = &p.base_url {
        obj.insert("base_url".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &p.api_key {
        obj.insert("api_key".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &p.auth_secret_name {
        obj.insert("auth_secret_name".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &p.headers {
        obj.insert("headers".to_string(), v.clone());
    }
    if let Some(v) = p.timeout_secs {
        obj.insert("timeout_secs".to_string(), Value::from(v));
    }
    // A key at a time, not a wholesale replacement: every payload here already
    // sets its own `options` (at minimum `{"temperature": 0}`), and a caller
    // adding `num_ctx` must not silently lose that default for every key they
    // did not mention. `p.options` wins per key on a collision - a caller who
    // explicitly sets `temperature` gets to override this pipeline's own
    // choice of 0, not just add to it.
    if let Some(overrides) = p.options.as_ref().and_then(Value::as_object) {
        let existing = obj.entry("options".to_string()).or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(existing) = existing.as_object_mut() {
            for (k, v) in overrides {
                existing.insert(k.clone(), v.clone());
            }
        }
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Value {
        json!({ "inquiry": "q", "model": "m" })
    }

    #[test]
    fn accepts_camel_case_spellings_of_its_own_snake_case_fields() {
        let mut p = base();
        p["typeRef"] = json!("/types/core/Object");
        p["pathPrefix"] = json!("/notes");
        p["maxTerms"] = json!(3);
        p["llmActionRef"] = json!("/packages/solx-ollama/ollama-chat");
        let parsed = parse(&p).unwrap();
        assert_eq!(parsed.type_ref.as_deref(), Some("/types/core/Object"));
        assert_eq!(parsed.path_prefix.as_deref(), Some("/notes"));
        assert_eq!(parsed.max_terms, 3);
        assert_eq!(parsed.llm_action_ref, "/packages/solx-ollama/ollama-chat");
    }

    #[test]
    fn options_parses_only_when_given_as_an_object() {
        let mut p = base();
        p["options"] = json!({ "num_ctx": 8192 });
        assert_eq!(parse(&p).unwrap().options, Some(json!({ "num_ctx": 8192 })));

        let mut p = base();
        p["options"] = json!("not an object");
        assert_eq!(parse(&p).unwrap().options, None);

        assert_eq!(parse(&base()).unwrap().options, None);
    }

    fn params_with_options(options: Option<Value>) -> Params {
        let mut p = parse(&base()).unwrap();
        p.options = options;
        p
    }

    #[test]
    fn no_caller_options_leaves_the_payloads_own_options_untouched() {
        let payload = json!({ "model": "m", "options": { "temperature": 0 } });
        let merged = apply_llm_overrides(payload, &params_with_options(None));
        assert_eq!(merged["options"], json!({ "temperature": 0 }));
    }

    #[test]
    fn caller_options_add_a_key_without_disturbing_the_default() {
        // num_ctx is the whole point of this: it is the only way to raise a
        // model's context window from this pipeline, and it must not cost the
        // caller the temperature: 0 every payload already sets.
        let payload = json!({ "model": "m", "options": { "temperature": 0 } });
        let params = params_with_options(Some(json!({ "num_ctx": 8192 })));
        let merged = apply_llm_overrides(payload, &params);
        assert_eq!(merged["options"], json!({ "temperature": 0, "num_ctx": 8192 }));
    }

    #[test]
    fn a_caller_option_overrides_this_pipelines_own_default_for_that_key() {
        let payload = json!({ "model": "m", "options": { "temperature": 0 } });
        let params = params_with_options(Some(json!({ "temperature": 0.7 })));
        let merged = apply_llm_overrides(payload, &params);
        assert_eq!(merged["options"], json!({ "temperature": 0.7 }));
    }

    #[test]
    fn options_are_merged_in_even_if_the_payload_had_none_at_all() {
        let payload = json!({ "model": "m" });
        let params = params_with_options(Some(json!({ "num_ctx": 4096 })));
        let merged = apply_llm_overrides(payload, &params);
        assert_eq!(merged["options"], json!({ "num_ctx": 4096 }));
    }
}
