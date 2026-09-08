//! Phase 1: ask the configured LLM action to turn the inquiry into search
//! terms.
//!
//! Structured output (`format` as a JSON schema) is requested so a compliant
//! model answers with exactly `{"terms": [...]}`. [`parse_terms`] still falls
//! back to looser parsing for a model that ignores `format` (older Ollama
//! models predate schema-constrained decoding) or wraps the JSON in prose or
//! a markdown fence — the alternative is a pipeline that only works with a
//! handful of models, which would defeat the point of taking `model` as a
//! caller-supplied param.

use serde_json::{json, Value};

use crate::host::{Host, Outcome};
use crate::params::Params;
use crate::prompts;

pub fn generate_search_terms(host: &dyn Host, p: &Params) -> Result<Vec<String>, Outcome> {
    let system = p
        .inquiry_prompt
        .clone()
        .unwrap_or_else(|| prompts::DEFAULT_INQUIRY_PROMPT.to_string());

    let payload = json!({
        "model": p.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": p.inquiry },
        ],
        "format": prompts::terms_schema(p.max_terms),
        "think": false,
        "options": { "temperature": 0 },
    });
    let payload = crate::params::apply_llm_overrides(payload, p);

    host.log(&format!("solx-inquiry: generating search terms via {}", p.llm_action_ref));

    let result = crate::llm::call(host, p, payload, "terms")?;

    let content = result.pointer("/message/content").and_then(Value::as_str).unwrap_or("");

    parse_terms(content, p.max_terms).ok_or_else(|| {
        Outcome::fail(
            "bad_llm_output",
            "could not extract search terms from the model's response",
            json!({ "stage": "terms", "raw": content }),
        )
    })
}

/// Best-effort extraction of a term list from a chat response's text
/// content. Tried in order:
///
/// 1. `{"terms": [...]}` — the schema-constrained shape.
/// 2. `[...]` — a bare JSON array of strings.
/// 3. Either of the above inside a ```-fenced code block.
/// 4. Line/comma-separated text, stripping common list markers.
///
/// `None` only when every strategy yields zero non-empty terms.
fn parse_terms(content: &str, max_terms: usize) -> Option<Vec<String>> {
    let trimmed = content.trim();

    if let Some(terms) = parse_terms_json(trimmed) {
        return Some(cap(terms, max_terms));
    }
    if let Some(fenced) = strip_code_fence(trimmed) {
        if let Some(terms) = parse_terms_json(fenced) {
            return Some(cap(terms, max_terms));
        }
    }

    let terms: Vec<String> = trimmed
        .split(['\n', ','])
        .map(strip_list_marker)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(cap(terms, max_terms))
    }
}

fn parse_terms_json(text: &str) -> Option<Vec<String>> {
    let value: Value = serde_json::from_str(text).ok()?;
    let array = value
        .get("terms")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())?;
    let terms: Vec<String> = array
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms)
    }
}

fn strip_code_fence(text: &str) -> Option<&str> {
    let text = text.strip_prefix("```")?;
    let text = text.strip_prefix("json").unwrap_or(text);
    let (body, _) = text.split_once("```")?;
    Some(body.trim())
}

/// Strip a leading list marker (`-`, `*`, `1.`, `1)`) and surrounding quotes.
fn strip_list_marker(s: &str) -> &str {
    let s = s.trim();
    let s = s
        .trim_start_matches(|c: char| c == '-' || c == '*' || c == '•')
        .trim_start();
    let s = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(i) if i > 0 && matches!(s.as_bytes().get(i), Some(b'.') | Some(b')')) => s[i + 1..].trim_start(),
        _ => s,
    };
    s.trim_matches(|c: char| c == '"' || c == '\'').trim()
}

fn cap(mut terms: Vec<String>, max_terms: usize) -> Vec<String> {
    terms.truncate(max_terms);
    terms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_schema_shape() {
        let terms = parse_terms(r#"{"terms": ["alpha", "beta"]}"#, 5).unwrap();
        assert_eq!(terms, vec!["alpha", "beta"]);
    }

    #[test]
    fn parses_a_bare_array() {
        let terms = parse_terms(r#"["alpha", "beta"]"#, 5).unwrap();
        assert_eq!(terms, vec!["alpha", "beta"]);
    }

    #[test]
    fn parses_a_fenced_code_block() {
        let terms = parse_terms("```json\n{\"terms\": [\"alpha\"]}\n```", 5).unwrap();
        assert_eq!(terms, vec!["alpha"]);
    }

    #[test]
    fn falls_back_to_list_markers() {
        let terms = parse_terms("- alpha\n- beta\n1. gamma", 5).unwrap();
        assert_eq!(terms, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn falls_back_to_comma_separated() {
        let terms = parse_terms("alpha, beta, gamma", 5).unwrap();
        assert_eq!(terms, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn caps_at_max_terms() {
        let terms = parse_terms(r#"{"terms": ["a", "b", "c", "d"]}"#, 2).unwrap();
        assert_eq!(terms, vec!["a", "b"]);
    }

    #[test]
    fn empty_content_yields_none() {
        assert!(parse_terms("", 5).is_none());
        assert!(parse_terms("   ", 5).is_none());
    }
}
