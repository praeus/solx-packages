//! Context documents: specific documents the caller names by reference,
//! fetched once and read into every prompt this run makes.
//!
//! Unlike a skill or a memory, nothing here decides whether one applies -
//! naming it in `context_documents` is the caller saying it does, so every
//! one that loads rides along with the intent call and every inquiry
//! regardless of kind. That is the difference from [`crate::recall`]: skills
//! are filtered by scope and memories are the model's own past output, but a
//! context document is the caller handing this run a fact to work from.
//!
//! Fetched with the same `entity-get-document` [`crate::session`] uses for
//! the session document, and best-effort in the same way - a document that
//! does not exist is not this pipeline's problem to fail over. The
//! difference is that the caller named this document specifically, so unlike
//! a failed skill or memory lookup (logged and otherwise silent), a missing
//! or malformed reference here is worth a `notes[]` entry: the caller is
//! more likely to have made a typo than to be indifferent to whether it
//! loaded.

use serde_json::{json, Value};

use crate::host::{join_within_budget, split_ref, truncate, Host};
use crate::params::multi::{MultiInquireParams, CONTEXT_BLOCK_CAP, CONTEXT_TEXT_CAP};
use crate::session::DOCUMENT_GET_REF;

#[derive(Debug, Clone)]
pub struct ContextDocument {
    pub reference: String,
    pub title: Option<String>,
    pub text: String,
}

/// Fetch every named document, in the order given. Returns what loaded and a
/// note for each one that did not, so a caller who mistyped a reference is
/// told rather than left to wonder why their document never showed up.
pub fn fetch(host: &dyn Host, p: &MultiInquireParams) -> (Vec<ContextDocument>, Vec<String>) {
    let mut docs = Vec::new();
    let mut notes = Vec::new();

    for reference in &p.context_documents {
        let Some((path, name)) = split_ref(reference) else {
            notes.push(format!(
                "context document {reference:?} is not a /path/name reference and was skipped"
            ));
            continue;
        };
        match host.exec(DOCUMENT_GET_REF, &json!({ "path": path, "name": name })) {
            Ok(c) if c.success => match to_context_document(reference, &c.result) {
                Some(doc) => docs.push(doc),
                None => notes.push(format!(
                    "context document {reference} has no contents and was skipped"
                )),
            },
            Ok(c) => notes.push(format!(
                "context document {reference} could not be loaded: {}",
                c.message.unwrap_or_else(|| "not found".to_string())
            )),
            Err(e) => notes.push(format!("context document {reference} could not be loaded: {e}")),
        }
    }

    (docs, notes)
}

/// A document has no fixed shape below `contents`, unlike a memory (which
/// this pipeline itself produces with a known `text` field). In order:
///
/// 1. **`contents.text`** - read the same way a memory is, for a context
///    document another `multi_inquire` run minted.
/// 2. **`contents` verbatim** - so a document written by hand or by another
///    tool comes through as whatever it actually holds rather than nothing
///    at all.
/// 3. **`summary`** - only when `contents` holds nothing. A summary is a
///    precis, and a caller who named this document in `context_documents`
///    asked for the document; letting the summary win wherever
///    `contents.text` happened to be absent silently replaced real content
///    with a one-line description of it. A session document is the case that
///    exposed it: its `contents` are the turns, and its `summary` is the
///    first response of the last turn, so naming one as context used to
///    yield a sentence in place of the history.
fn to_context_document(reference: &str, doc: &Value) -> Option<ContextDocument> {
    let title = doc.get("title").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let contents = doc.get("contents")?;
    let text = match contents.get("text").and_then(Value::as_str) {
        Some(text) => text.trim().to_string(),
        // Nothing under `contents` at all - the summary is all there is.
        // Checked before the string arm below, so a whitespace-only string
        // is absence rather than its own (empty) text.
        None if is_blank(contents) => doc
            .get("summary")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string(),
        // A bare string is already its own text; rendering it as JSON would
        // put the quotes in front of the model too.
        None if contents.is_string() => contents.as_str().unwrap_or_default().trim().to_string(),
        None => serde_json::to_string_pretty(contents).unwrap_or_default(),
    };
    if text.is_empty() {
        return None;
    }
    Some(ContextDocument {
        reference: reference.to_string(),
        title: title.map(str::to_string),
        text: truncate(&text, CONTEXT_TEXT_CAP),
    })
}

/// True when `contents` has nothing worth putting in a prompt. Rendering one
/// of these verbatim would put the literal text `{}`, `[]` or `null` in front
/// of the model, which reads as content when it is the absence of any.
fn is_blank(contents: &Value) -> bool {
    match contents {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

/// The reference block carrying every loaded context document, joined under
/// [`CONTEXT_BLOCK_CAP`] in the order given - which is the order the caller
/// named them in, so a caller who put the most important one first keeps it
/// even if a later one has to be dropped.
pub fn context_block(docs: &[ContextDocument]) -> Option<String> {
    if docs.is_empty() {
        return None;
    }
    let lines: Vec<String> = docs
        .iter()
        .map(|d| match &d.title {
            Some(title) => format!("## {} ({})\n\n{}", title, d.reference, d.text),
            None => format!("## {}\n\n{}", d.reference, d.text),
        })
        .collect();
    Some(format!(
        "Documents supplied for this instruction. Treat them as given fact for this \
         instruction, not as something to verify.\n\n{}",
        join_within_budget(&lines, CONTEXT_BLOCK_CAP)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // `fetch` itself is exercised end to end in `tests/multi.rs`, against
    // that suite's `FakeHost` - these cover the pure logic: reading a
    // document's text out of whatever shape it actually has, and assembling
    // the block from what was read.

    #[test]
    fn reads_text_from_contents_text_first() {
        let doc = json!({ "title": "A", "contents": { "text": "from text" }, "summary": "ignored" });
        let d = to_context_document("/notes/a", &doc).unwrap();
        assert_eq!(d.text, "from text");
        assert_eq!(d.title.as_deref(), Some("A"));
    }

    #[test]
    fn falls_back_to_summary_only_when_contents_holds_nothing() {
        let doc = json!({ "contents": {}, "summary": "from summary" });
        let d = to_context_document("/notes/b", &doc).unwrap();
        assert_eq!(d.text, "from summary");
    }

    #[test]
    fn real_contents_beat_a_summary() {
        // The case that made naming a session document as context useless:
        // its contents are the turns, its summary is one response from the
        // last one, and the summary used to win.
        let doc = json!({
            "contents": { "turns": [{ "instruction": "how does auth work?" }], "turnCount": 1 },
            "summary": "Auth uses session tokens.",
        });
        let d = to_context_document("/sessions/s", &doc).unwrap();
        assert!(d.text.contains("how does auth work?"), "{}", d.text);
        assert!(!d.text.contains("Auth uses session tokens."), "{}", d.text);
    }

    #[test]
    fn a_string_contents_is_read_as_its_own_text() {
        let doc = json!({ "contents": "just a note" });
        let d = to_context_document("/notes/s", &doc).unwrap();
        assert_eq!(d.text, "just a note");
    }

    #[test]
    fn contents_shaped_like_absence_are_never_rendered_literally() {
        // `null`, `[]` and `""` all read as content when they are the
        // absence of it, so each falls through to the summary.
        for empty in [json!(null), json!([]), json!("   ")] {
            let doc = json!({ "contents": empty, "summary": "from summary" });
            let d = to_context_document("/notes/e", &doc).unwrap();
            assert_eq!(d.text, "from summary");
        }
    }

    #[test]
    fn falls_back_to_the_raw_contents_when_there_is_no_text_or_summary() {
        let doc = json!({ "contents": { "amount": 5 } });
        let d = to_context_document("/notes/c", &doc).unwrap();
        assert!(d.text.contains("\"amount\": 5"), "{}", d.text);
    }

    #[test]
    fn a_document_with_nothing_to_show_is_skipped() {
        assert!(to_context_document("/notes/empty", &json!({ "contents": {} })).is_none());
        assert!(to_context_document("/notes/no-contents", &json!({})).is_none());
    }

    #[test]
    fn context_block_is_none_when_nothing_loaded() {
        assert!(context_block(&[]).is_none());
    }

    #[test]
    fn context_block_carries_every_document_in_order() {
        let docs = vec![
            ContextDocument { reference: "/a/x".to_string(), title: Some("X".to_string()), text: "one".to_string() },
            ContextDocument { reference: "/a/y".to_string(), title: None, text: "two".to_string() },
        ];
        let block = context_block(&docs).unwrap();
        assert!(block.contains("## X (/a/x)\n\none"), "{block}");
        assert!(block.contains("## /a/y\n\ntwo"), "{block}");
        assert!(block.find("one").unwrap() < block.find("two").unwrap());
    }
}
