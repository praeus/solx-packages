//! Parse and default an `inquire` call's params.

use serde_json::Value;

use crate::host::{str_param, Outcome};
use super::{Params, Scope, DEFAULT_LLM_ACTION_REF, MAX_LLM_TIMEOUT};

/// Ceiling on how many search terms the model may be asked to produce.
pub const MAX_TERMS_CEILING: usize = 10;
/// Ceiling on how many hits are fed to the summarizer / returned to the caller.
pub const MAX_RESULTS_CEILING: usize = 50;

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
}
