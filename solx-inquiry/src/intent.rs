//! Phase 1 of `instruct`: decide what the instruction actually needs.
//!
//! One llm call, `format`-constrained to a decision rather than prose: either
//! answer now (`direct`), or name up to three things to look up (`inquire`).
//! Each proposed inquiry carries its own search terms, which is what keeps the
//! whole pipeline at `1 + N` model calls — see
//! [`crate::prompts::intent_schema`].
//!
//! This call is *not* fanned out. It is the one thing everything else depends
//! on, so it runs through [`crate::llm::call`] exactly as `inquire`'s phases
//! do, with the same detached-or-blocking behaviour and the same live console
//! echo.

use serde_json::{json, Value};

use crate::host::{Host, Outcome};
use crate::instruct_params::InstructParams;
use crate::params::Scope;
use crate::prompts;
use crate::recall::{all_skills, memory_block, skill_block, Recalled};
use crate::session::{history_block, Session};
use crate::terms::{expand_terms, fallback_terms};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Direct,
    Inquire,
}

#[derive(Debug, Clone)]
pub struct Inquiry {
    pub kind: Scope,
    pub question: String,
    pub terms: Vec<String>,
    /// An instruction-specific note the intent phase wants appended to that
    /// inquiry's prompt. It **appends**; it never replaces the default, so a
    /// model cannot talk the pipeline out of its own grounding rules. A caller
    /// who genuinely wants replacement uses the `document_prompt` /
    /// `action_prompt` params.
    pub amendment: Option<String>,
}

impl Inquiry {
    pub fn is_actions(&self) -> bool {
        self.kind.searches_actions()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind.as_str(),
            "question": self.question,
            "terms": self.terms,
            "prompt": self.amendment,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Intent {
    pub mode: Mode,
    /// The answer, when `mode` is `Direct`.
    pub response: Option<String>,
    pub memory: bool,
    pub inquiries: Vec<Inquiry>,
    /// An instruction for a later, separate `instruct` call, once this run's
    /// inquiries have been acted on. Proposed here, before any inquiry has
    /// run, so it is speculative — a hint at what comes next in general
    /// terms, not something grounded in results it has not seen. Nothing in
    /// this pipeline re-invokes itself with it; a caller decides whether and
    /// when to.
    pub next_prompt: Option<String>,
}

impl Intent {
    pub fn to_json(&self) -> Value {
        json!({
            "mode": match self.mode { Mode::Direct => "direct", Mode::Inquire => "inquire" },
            "response": self.response,
            "memory": self.memory,
            "inquiries": self.inquiries.iter().map(Inquiry::to_json).collect::<Vec<_>>(),
            "next_prompt": self.next_prompt,
        })
    }
}

pub const STAGE: &str = "intent";

pub fn decide(
    host: &dyn Host,
    p: &InstructParams,
    recalled: &Recalled,
    session: &Session,
) -> Result<Intent, Outcome> {
    let mut system = p
        .intent_prompt
        .clone()
        .unwrap_or_else(|| prompts::DEFAULT_INTENT_PROMPT.to_string());

    // Nothing has been searched yet, so there are no hits to narrow a skill's
    // `tools` globs against and no one scope to select by: every skill is
    // eligible, under one shared budget.
    let applicable = all_skills(&recalled.skills);
    if let Some(block) = skill_block(&applicable) {
        system.push_str("\n\n");
        system.push_str(&block);
    }
    if let Some(block) = memory_block(&recalled.memories) {
        system.push_str("\n\n");
        system.push_str(&block);
    }
    if let Some(block) = history_block(session, p.history_limit) {
        system.push_str("\n\n");
        system.push_str(&block);
    }

    let payload = json!({
        "model": p.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": p.instruction },
        ],
        "format": prompts::intent_schema(p.max_inquiries, p.max_terms),
        "think": false,
        "options": { "temperature": 0 },
    });
    let payload = crate::params::apply_llm_overrides(payload, &p.llm);

    host.log(&format!("solx-inquiry: deciding intent via {}", p.llm_action_ref()));

    let result = crate::llm::call(host, &p.llm, payload, STAGE)?;
    let content = result.pointer("/message/content").and_then(Value::as_str).unwrap_or("");

    Ok(parse(content, p.max_inquiries, p.max_terms))
}

/// Turn the model's content into an [`Intent`].
///
/// Never fails. A model that ignored `format` still said *something*, and the
/// useful reading of unparseable content is "it answered directly" — which is
/// exactly what `direct` mode is. Failing instead would turn a chatty model
/// into a broken pipeline, and the fallback costs nothing: a direct response
/// is returned to the caller as a response like any other.
pub fn parse(content: &str, max_inquiries: usize, max_terms: usize) -> Intent {
    let trimmed = content.trim();
    let Some(value) = parse_json(trimmed) else {
        return Intent {
            mode: Mode::Direct,
            response: (!trimmed.is_empty()).then(|| trimmed.to_string()),
            memory: false,
            inquiries: Vec::new(),
            next_prompt: None,
        };
    };

    let response = value
        .get("response")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let memory = value.get("memory").and_then(Value::as_bool).unwrap_or(false);
    let inquiries = parse_inquiries(&value, max_inquiries, max_terms);
    let next_prompt = value
        .get("next_prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // The declared mode is a hint, not the authority: a model that says
    // "inquire" and lists nothing has produced no work to do, and one that
    // says "direct" while listing inquiries has. Trusting the word over the
    // content would strand the run in the first case and silently discard
    // real work in the second.
    let mode = if inquiries.is_empty() { Mode::Direct } else { Mode::Inquire };

    Intent { mode, response, memory, inquiries, next_prompt }
}

fn parse_inquiries(value: &Value, max_inquiries: usize, max_terms: usize) -> Vec<Inquiry> {
    let Some(items) = value.get("inquiries").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let question = item
                .get("question")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())?;
            let kind = match item.get("kind").and_then(Value::as_str) {
                Some("actions") => Scope::Actions,
                // Anything else is a document inquiry. Defaulting to documents
                // rather than dropping the inquiry keeps a typo'd `kind` a
                // wrong-but-useful search instead of silent work loss.
                _ => Scope::Documents,
            };
            let mut terms: Vec<String> = item
                .get("terms")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if terms.is_empty() {
                terms = fallback_terms(question, max_terms);
            } else {
                // A model that answers with a phrase where the prompt asked
                // for keywords would otherwise drive this inquiry to zero
                // hits, since FTS ANDs every word within one term.
                terms = expand_terms(&terms, max_terms);
            }
            Some(Inquiry {
                kind,
                question: question.to_string(),
                terms,
                amendment: item
                    .get("prompt")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            })
        })
        .take(max_inquiries)
        .collect()
}

/// Strict JSON, then a ```-fenced block containing it — the same two tiers
/// [`crate::terms::parse_terms`] tries, and for the same reason: not every
/// model honors `format`.
fn parse_json(text: &str) -> Option<Value> {
    if let Ok(v @ Value::Object(_)) = serde_json::from_str(text) {
        return Some(v);
    }
    let fenced = strip_code_fence(text)?;
    match serde_json::from_str(fenced) {
        Ok(v @ Value::Object(_)) => Some(v),
        _ => None,
    }
}

fn strip_code_fence(text: &str) -> Option<&str> {
    let text = text.strip_prefix("```")?;
    let text = text.strip_prefix("json").unwrap_or(text);
    let (body, _) = text.split_once("```")?;
    Some(body.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_direct_answer() {
        let intent = parse(r#"{"mode":"direct","response":"42","memory":true}"#, 3, 5);
        assert_eq!(intent.mode, Mode::Direct);
        assert_eq!(intent.response.as_deref(), Some("42"));
        assert!(intent.memory);
        assert!(intent.inquiries.is_empty());
    }

    #[test]
    fn parses_inquiries_with_their_own_terms() {
        let intent = parse(
            r#"{"mode":"inquire","inquiries":[
                {"kind":"documents","question":"what is auth?","terms":["auth","session"]},
                {"kind":"actions","question":"how do I deploy?","terms":["deploy"],"prompt":"prefer dry runs"}
            ]}"#,
            3,
            5,
        );
        assert_eq!(intent.mode, Mode::Inquire);
        assert_eq!(intent.inquiries.len(), 2);
        assert!(!intent.inquiries[0].is_actions());
        assert_eq!(intent.inquiries[0].terms, vec!["auth", "session"]);
        assert!(intent.inquiries[1].is_actions());
        assert_eq!(intent.inquiries[1].amendment.as_deref(), Some("prefer dry runs"));
    }

    #[test]
    fn an_inquiry_with_no_terms_falls_back_locally_rather_than_to_another_llm_call() {
        let intent = parse(
            r#"{"mode":"inquire","inquiries":[{"kind":"documents","question":"how does deployment work?"}]}"#,
            3,
            5,
        );
        assert_eq!(intent.inquiries[0].terms, vec!["deployment", "work"]);
    }

    #[test]
    fn inquiries_are_capped_and_terms_truncated() {
        let intent = parse(
            r#"{"mode":"inquire","inquiries":[
                {"kind":"documents","question":"a","terms":["1","2","3"]},
                {"kind":"documents","question":"b","terms":["x"]},
                {"kind":"documents","question":"c","terms":["y"]}
            ]}"#,
            2,
            2,
        );
        assert_eq!(intent.inquiries.len(), 2);
        assert_eq!(intent.inquiries[0].terms, vec!["1", "2"]);
    }

    #[test]
    fn the_content_decides_the_mode_not_the_declared_word() {
        // "inquire" with nothing to inquire about is a direct answer.
        let intent = parse(r#"{"mode":"inquire","response":"nothing to look up","inquiries":[]}"#, 3, 5);
        assert_eq!(intent.mode, Mode::Direct);
        // "direct" while listing real work must not throw that work away.
        let intent = parse(
            r#"{"mode":"direct","inquiries":[{"kind":"actions","question":"q","terms":["t"]}]}"#,
            3,
            5,
        );
        assert_eq!(intent.mode, Mode::Inquire);
    }

    #[test]
    fn a_model_that_ignored_format_still_produces_a_direct_response() {
        let intent = parse("Sure - you already have everything you need.", 3, 5);
        assert_eq!(intent.mode, Mode::Direct);
        assert_eq!(intent.response.as_deref(), Some("Sure - you already have everything you need."));
    }

    #[test]
    fn parses_a_fenced_block() {
        let intent = parse("```json\n{\"mode\":\"direct\",\"response\":\"hi\"}\n```", 3, 5);
        assert_eq!(intent.response.as_deref(), Some("hi"));
    }

    #[test]
    fn a_next_prompt_is_carried_through_alongside_a_direct_answer() {
        let intent = parse(
            r#"{"mode":"direct","response":"created the file","next_prompt":"verify the file was created and report its size"}"#,
            3,
            5,
        );
        assert_eq!(intent.next_prompt.as_deref(), Some("verify the file was created and report its size"));
    }

    #[test]
    fn an_empty_next_prompt_is_treated_as_absent() {
        let intent = parse(r#"{"mode":"direct","response":"ok","next_prompt":"   "}"#, 3, 5);
        assert_eq!(intent.next_prompt, None);
    }

    #[test]
    fn no_next_prompt_field_is_none_not_an_error() {
        let intent = parse(r#"{"mode":"direct","response":"ok"}"#, 3, 5);
        assert_eq!(intent.next_prompt, None);
    }

    #[test]
    fn an_unknown_kind_reads_as_a_document_inquiry_rather_than_being_dropped() {
        let intent = parse(
            r#"{"mode":"inquire","inquiries":[{"kind":"everything","question":"q","terms":["t"]}]}"#,
            3,
            5,
        );
        assert_eq!(intent.inquiries.len(), 1);
        assert!(!intent.inquiries[0].is_actions());
    }
}
