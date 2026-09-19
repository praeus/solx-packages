//! Phase 1 of `multi_inquire`: decide what the instruction actually needs.
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
use crate::params::multi::MultiInquireParams;
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
    /// An instruction for a later, separate `multi_inquire` call, once this run's
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
    p: &MultiInquireParams,
    recalled: &Recalled,
    session: &Session,
    context_block: Option<&str>,
) -> Result<Intent, Outcome> {
    let mut system = p
        .intent_prompt
        .clone()
        .unwrap_or_else(|| prompts::DEFAULT_INTENT_PROMPT.to_string());

    // Named by the caller for this instruction specifically, so it rides
    // along unconditionally - unlike a skill or a memory, nothing here
    // decides whether it applies.
    if let Some(block) = context_block {
        system.push_str("\n\n");
        system.push_str(block);
    }
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

/// Strict JSON, then a ```-fenced block, then a hand-rolled XML parser for
/// `<intent>…</intent>`-shaped responses — three tiers, in that order,
/// because not every model honors `format` and not every model that ignores
/// `format` produces JSON rather than XML.
fn parse_json(text: &str) -> Option<Value> {
    if let Ok(v @ Value::Object(_)) = serde_json::from_str(text) {
        return Some(v);
    }
    if let Some(fenced) = strip_code_fence(text) {
        if let Ok(v @ Value::Object(_)) = serde_json::from_str(fenced) {
            return Some(v);
        }
    }
    parse_xml(text)
}

fn strip_code_fence(text: &str) -> Option<&str> {
    let text = text.strip_prefix("```")?;
    let text = text.strip_prefix("json").unwrap_or(text);
    let (body, _) = text.split_once("```")?;
    Some(body.trim())
}

/// Try to parse `<intent>…</intent>`-shaped XML into a `Value` matching the
/// JSON schema the model is normally asked to produce.
///
/// The shapes accepted are deliberately narrow: only element names that
/// match the schema field names are recognised (case-insensitive, since
/// `<Mode>` and `<MODE>` show up in the wild), attributes are ignored
/// (the JSON path uses string values, so XML must too), and text content is
/// treated as a string. Anything that does not look like the schema is
/// rejected — a freeform essay with an `<intent>` substring somewhere
/// should not be silently misread as an intent structure.
///
/// Recognised shape:
///
/// ```xml
/// <intent>
///   <mode>direct|inquire</mode>
///   <response>text</response>
///   <memory>true|false</memory>
///   <next_prompt>text</next_prompt>
///   <inquiries>
///     <inquiry>
///       <kind>documents|actions</kind>
///       <question>text</question>
///       <terms><term>word</term><term>word</term></terms>
///       <prompt>text</prompt>
///     </inquiry>
///   </inquiries>
/// </intent>
/// ```
///
/// The parser is hand-rolled to keep the wasm artifact flat: a small
/// recursive-descent walker over a slice of bytes, no allocation until a
/// `Value` is built.
fn parse_xml(text: &str) -> Option<Value> {
    let bytes = text.as_bytes();
    let mut p = Parser::new(bytes);
    // The intent shape is the only shape we accept: an outer `<intent>`
    // element. Anything else — a stray `<inquiries>` block, or a JSON-
    // looking envelope — gets passed through to the JSON tier instead.
    p.skip_ws();
    let elem = p.parse_element()?;
    if !elem.name.eq_ignore_ascii_case("intent") {
        return None;
    }
    Some(element_to_value(elem))
}

/// A single parsed XML element. `name` is the tag; `children` are the nested
/// elements and text content in source order; `attrs` are dropped on the
/// floor because the schema has no attribute-style inputs.
struct Element<'a> {
    name: &'a str,
    children: Vec<Node<'a>>,
}

enum Node<'a> {
    Element(Element<'a>),
    Text(&'a str),
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    /// Parse a single `<name …>children</name>` or `<name …/>` element.
    /// Returns `None` if the input does not start with `<`.
    fn parse_element(&mut self) -> Option<Element<'a>> {
        self.skip_ws();
        if self.peek()? != b'<' {
            return None;
        }
        self.pos += 1; // consume '<'
        let name_start = self.pos;
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            if c == b'>' || c == b'/' || c.is_ascii_whitespace() {
                break;
            }
            self.pos += 1;
        }
        let name = std::str::from_utf8(&self.bytes[name_start..self.pos]).ok()?;
        // Skip attributes (we ignore them all).
        while self.pos < self.bytes.len() && self.bytes[self.pos] != b'>' {
            self.pos += 1;
        }
        if self.pos >= self.bytes.len() {
            return None;
        }
        if self.bytes[self.pos] == b'/' {
            // Self-closing tag <name/>.
            self.pos += 1;
            if self.bytes.get(self.pos) != Some(&b'>') {
                return None;
            }
            self.pos += 1;
            return Some(Element { name, children: Vec::new() });
        }
        self.pos += 1; // consume '>'

        // Children until matching close tag.
        let mut children: Vec<Node<'a>> = Vec::new();
        loop {
            // Look for end of element.
            if self.bytes.get(self.pos) == Some(&b'<')
                && self.bytes.get(self.pos + 1) == Some(&b'/')
            {
                self.pos += 2; // consume "</"
                let close_start = self.pos;
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'>' {
                    self.pos += 1;
                }
                let close = std::str::from_utf8(&self.bytes[close_start..self.pos]).ok()?;
                if !close.eq_ignore_ascii_case(name) {
                    return None; // mismatched close tag: bail
                }
                if self.bytes.get(self.pos) != Some(&b'>') {
                    return None;
                }
                self.pos += 1;
                return Some(Element { name, children });
            }
            if self.peek()? != b'<' {
                // Text node up to the next `<`.
                let text_start = self.pos;
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'<' {
                    self.pos += 1;
                }
                let text = std::str::from_utf8(&self.bytes[text_start..self.pos])
                    .ok()?
                    .trim();
                if !text.is_empty() {
                    children.push(Node::Text(text));
                }
            } else {
                if let Some(child) = self.parse_element() {
                    children.push(Node::Element(child));
                } else {
                    return None;
                }
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }
}

/// Convert an XML element to a `serde_json::Value`. Sub-elements recurse
/// into `element_to_value`; `<terms>` flattens to an array of strings;
/// everything else with no sub-elements is its text content as a string.
fn element_to_value(elem: Element<'_>) -> Value {
    let mut fields: std::collections::BTreeMap<String, Vec<Value>> =
        std::collections::BTreeMap::new();

    for child in elem.children {
        match child {
            Node::Text(t) => {
                // Plain text inside an element: stashed under a synthetic
                // "_text" key and dropped at the end. Keeps the field
                // grouping below simple.
                fields
                    .entry("_text".to_string())
                    .or_default()
                    .push(Value::String(t.to_string()));
            }
            Node::Element(e) => {
                let key = e.name.to_ascii_lowercase();
                fields.entry(key).or_default().push(element_child_value(e));
            }
        }
    }

    let mut out = serde_json::Map::new();
    for (key, values) in fields {
        if key == "_text" {
            continue;
        }
        match key.as_str() {
            "term" => {
                // `<term>` directly under an element (rare shape) — collect
                // into a `terms` array on the parent.
                let arr: Vec<Value> = values
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(Value::String(s)),
                        _ => None,
                    })
                    .collect();
                if !arr.is_empty() {
                    out.insert("terms".to_string(), Value::Array(arr));
                }
            }
            "terms" => {
                // `<terms>` containing repeated `<term>` children.
                let arr: Vec<Value> = values
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::Array(a) => Some(a),
                        _ => None,
                    })
                    .flat_map(|a| a.into_iter())
                    .collect();
                out.insert("terms".to_string(), Value::Array(arr));
            }
            "inquiry" => {
                // `<inquiry>` (singular). kimi uses this; gemma uses the
                // plural form. Each `<inquiry>` becomes one object in the
                // `inquiries` array.
                let arr: Vec<Value> = values
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::Object(_) => Some(v),
                        _ => None,
                    })
                    .collect();
                if !arr.is_empty() {
                    extend_or_insert_array(&mut out, "inquiries", arr);
                }
            }
            "inquiries" => {
                // `<inquiries>` containing repeated `<inquiry>` children.
                // The element produces its children as a flat array —
                // not another nested `inquiries` object — so that the
                // outer `<intent>` element can attach the array directly
                // under `inquiries` without double-wrapping.
                let arr: Vec<Value> = values
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::Array(a) => Some(a),
                        _ => None,
                    })
                    .flat_map(|a| a.into_iter())
                    .collect();
                if !arr.is_empty() {
                    extend_or_insert_array(&mut out, "inquiries", arr);
                }
            }
            "mode" | "response" | "next_prompt" | "question" | "kind" | "prompt" => {
                if let Some(Value::String(s)) = values.into_iter().next() {
                    out.insert(key, Value::String(s));
                }
            }
            "memory" => {
                let v = values
                    .into_iter()
                    .next()
                    .and_then(|v| match v {
                        Value::Bool(b) => Some(Value::Bool(b)),
                        Value::String(s) => {
                            Some(Value::Bool(s == "true" || s == "1"))
                        }
                        _ => None,
                    });
                if let Some(v) = v {
                    out.insert("memory".to_string(), v);
                }
            }
            _ => {
                // Unknown element: drop.
            }
        }
    }
    Value::Object(out)
}

/// Convert one XML element to a single `Value`. Sub-elements recurse into
/// `element_to_value`; `<terms>` flattens to an array of strings; everything
/// else with no sub-elements is its text content as a string. With sub-
/// elements, the result is an object built from those sub-elements.
fn element_child_value(elem: Element<'_>) -> Value {
    let mut text_acc = String::new();
    let mut element_children: Vec<Element<'_>> = Vec::new();
    for child in elem.children {
        match child {
            Node::Text(t) => text_acc.push_str(t),
            Node::Element(e) => element_children.push(e),
        }
    }

    if element_children.is_empty() {
        let s = text_acc.trim();
        if s.is_empty() {
            Value::Null
        } else {
            Value::String(s.to_string())
        }
    } else if elem.name.eq_ignore_ascii_case("terms") {
        // `<terms>` -> array of strings from its `<term>` children.
        let terms: Vec<Value> = element_children
            .into_iter()
            .filter(|e| e.name.eq_ignore_ascii_case("term"))
            .map(|e| {
                let s = e
                    .children
                    .into_iter()
                    .filter_map(|c| match c {
                        Node::Text(t) => Some(t.to_string()),
                        _ => None,
                    })
                    .collect::<String>()
                    .trim()
                    .to_string();
                Value::String(s)
            })
            .filter(|v| !matches!(v, Value::String(s) if s.is_empty()))
            .collect();
        Value::Array(terms)
    } else if elem.name.eq_ignore_ascii_case("inquiries") {
        // `<inquiries>` -> flat array of inquiry objects. Same idea as
        // `<terms>`: the children are already shaped as JSON values, just
        // collect them in order.
        let mut arr: Vec<Value> = Vec::new();
        for child in element_children {
            if child.name.eq_ignore_ascii_case("inquiry") {
                arr.push(element_to_value(child));
            }
        }
        Value::Array(arr)
    } else {
        element_to_value(Element {
            name: elem.name,
            children: element_children
                .into_iter()
                .map(Node::Element)
                .collect(),
        })
    }
}

/// Append `items` to the array under `key` in `out`, or insert a fresh
/// array if the key isn't yet present. Used by both `<inquiry>` (singular)
/// and `<inquiries>` (plural) so they can each contribute to one combined
/// `inquiries` array on the parent element.
fn extend_or_insert_array(
    out: &mut serde_json::Map<String, Value>,
    key: &str,
    items: Vec<Value>,
) {
    match out.get_mut(key) {
        Some(Value::Array(existing)) => existing.extend(items),
        _ => {
            out.insert(key.to_string(), Value::Array(items));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xml_direct_response() {
        let intent = parse(
            "<intent><mode>direct</mode><response>Paris is the capital of France.</response></intent>",
            3,
            5,
        );
        assert_eq!(intent.mode, Mode::Direct);
        assert_eq!(
            intent.response.as_deref(),
            Some("Paris is the capital of France.")
        );
        assert!(intent.inquiries.is_empty());
    }

    #[test]
    fn parses_xml_with_uppercase_tags_and_whitespace() {
        let intent = parse(
            "  <Intent>\n  <Mode>direct</Mode>\n  <Response>ok</Response>\n  </Intent>",
            3,
            5,
        );
        assert_eq!(intent.mode, Mode::Direct);
        assert_eq!(intent.response.as_deref(), Some("ok"));
    }

    #[test]
    fn parses_xml_inquiries_with_terms_wrapper() {
        let intent = parse(
            "<intent>\
<mode>inquire</mode>\
<inquiries>\
<inquiry>\
<kind>documents</kind>\
<question>what is auth?</question>\
<terms><term>auth</term><term>session</term></terms>\
</inquiry>\
</inquiries>\
</intent>",
            3,
            5,
        );
        assert_eq!(intent.mode, Mode::Inquire);
        assert_eq!(intent.inquiries.len(), 1);
        assert!(!intent.inquiries[0].is_actions());
        assert_eq!(intent.inquiries[0].terms, vec!["auth", "session"]);
    }

    #[test]
    fn parses_xml_with_singular_inquiry_tag() {
        // gemma's actual emitted shape uses `<inquiry>` repeated instead of
        // `<inquiries><inquiry>...</inquiry></inquiries>`.
        let intent = parse(
            "<intent>\
<mode>inquire</mode>\
<inquiry>\
<kind>documents</kind>\
<question>solx-system</question>\
<terms><term>solx-system</term><term>multi_inquire</term></terms>\
</inquiry>\
<inquiry>\
<kind>actions</kind>\
<question>how to deploy</question>\
<terms><term>deploy</term></terms>\
<prompt>prefer dry runs</prompt>\
</inquiry>\
</intent>",
            3,
            5,
        );
        assert_eq!(intent.mode, Mode::Inquire);
        assert_eq!(intent.inquiries.len(), 2);
        assert!(!intent.inquiries[0].is_actions());
        assert_eq!(intent.inquiries[0].terms, vec!["solx-system", "multi_inquire"]);
        assert!(intent.inquiries[1].is_actions());
        assert_eq!(
            intent.inquiries[1].amendment.as_deref(),
            Some("prefer dry runs")
        );
    }

    #[test]
    fn parses_xml_unknown_kind_defaults_to_documents() {
        // Mirrors the JSON path's silent default: a typo'd kind still
        // produces a useful document inquiry rather than dropping the
        // inquiry entirely.
        let intent = parse(
            "<intent>\
<mode>inquire</mode>\
<inquiry>\
<kind>everything</kind>\
<question>q</question>\
<terms><term>t</term></terms>\
</inquiry>\
</intent>",
            3,
            5,
        );
        assert_eq!(intent.inquiries.len(), 1);
        assert!(!intent.inquiries[0].is_actions());
    }

    #[test]
    fn parses_xml_with_memory_and_next_prompt() {
        let intent = parse(
            "<intent>\
<mode>direct</mode>\
<response>created the file</response>\
<memory>true</memory>\
<next_prompt>verify the file was created</next_prompt>\
</intent>",
            3,
            5,
        );
        assert!(intent.memory);
        assert_eq!(
            intent.next_prompt.as_deref(),
            Some("verify the file was created")
        );
    }

    #[test]
    fn xml_inquiries_are_capped_at_max_inquiries() {
        let intent = parse(
            "<intent>\
<mode>inquire</mode>\
<inquiry>\
<kind>documents</kind>\
<question>a</question>\
<terms><term>a</term></terms>\
</inquiry>\
<inquiry>\
<kind>documents</kind>\
<question>b</question>\
<terms><term>b</term></terms>\
</inquiry>\
<inquiry>\
<kind>documents</kind>\
<question>c</question>\
<terms><term>c</term></terms>\
</inquiry>\
</intent>",
            2,
            5,
        );
        assert_eq!(intent.inquiries.len(), 2);
        assert_eq!(intent.inquiries[0].question, "a");
        assert_eq!(intent.inquiries[1].question, "b");
    }

    #[test]
    fn empty_inquiries_with_inquire_mode_falls_back_to_direct() {
        // Mirrors the JSON tier's behaviour: declared "inquire" with no
        // inquiries is silently "direct" because there is nothing to look
        // up.
        let intent = parse(
            "<intent><mode>inquire</mode><response>nothing to look up</response></intent>",
            3,
            5,
        );
        assert_eq!(intent.mode, Mode::Direct);
        assert_eq!(intent.response.as_deref(), Some("nothing to look up"));
    }

    #[test]
    fn xml_with_real_terms_list_still_falls_back_when_empty() {
        // Empty <terms></terms> -> terms are computed locally from the
        // question, not silently dropped.
        let intent = parse(
            "<intent>\
<mode>inquire</mode>\
<inquiry>\
<kind>documents</kind>\
<question>how does deployment work?</question>\
<terms></terms>\
</inquiry>\
</intent>",
            3,
            5,
        );
        assert_eq!(intent.inquiries[0].terms, vec!["deployment", "work"]);
    }

    #[test]
    fn rejects_xml_without_intent_root() {
        // Stray `<inquiries>` not wrapped in `<intent>` should be passed
        // through to the "treat as text" fallback, not silently turned
        // into an intent object.
        let intent = parse(
            "<inquiries><inquiry><kind>documents</kind></inquiry></inquiries>",
            3,
            5,
        );
        assert_eq!(intent.mode, Mode::Direct);
        assert_eq!(
            intent.response.as_deref(),
            Some("<inquiries><inquiry><kind>documents</kind></inquiry></inquiries>")
        );
    }

    #[test]
    fn rejects_mismatched_close_tag() {
        let intent = parse(
            "<intent><mode>direct</mode><response>oops</wrong>",
            3,
            5,
        );
        // Falls back to "treat as text": the close tag doesn't match.
        assert_eq!(intent.mode, Mode::Direct);
        assert_eq!(
            intent.response.as_deref(),
            Some("<intent><mode>direct</mode><response>oops</wrong>")
        );
    }

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
