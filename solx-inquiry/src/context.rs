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
use crate::instruct_params::{InstructParams, CONTEXT_BLOCK_CAP, CONTEXT_TEXT_CAP};
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
pub fn fetch(host: &dyn Host, p: &InstructParams) -> (Vec<ContextDocument>, Vec<String>) {
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
/// this pipeline itself writes with a known `text` field). `contents.text` is
/// read the same way a memory is, for a context document another `instruct`
/// run minted; anything else is read as the document's contents verbatim, so
/// a context document written by hand or by another tool still comes through
/// as whatever it actually holds rather than nothing at all.
fn to_context_document(reference: &str, doc: &Value) -> Option<ContextDocument> {
    let title = doc.get("title").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let contents = doc.get("contents")?;
    let text = match contents.get("text").and_then(Value::as_str) {
        Some(text) => text.trim().to_string(),
        None => {
            let summary = doc.get("summary").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
            match summary {
                Some(summary) => summary.to_string(),
                // An empty object has nothing worth showing - falling
                // through to it would render the literal text "{}".
                None if contents.as_object().is_some_and(|m| m.is_empty()) => String::new(),
                None => serde_json::to_string_pretty(contents).unwrap_or_default(),
            }
        }
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

    // `fetch` itself is exercised end to end in `tests/instruct.rs`, against
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
    fn falls_back_to_summary_when_contents_has_no_text() {
        let doc = json!({ "contents": {}, "summary": "from summary" });
        let d = to_context_document("/notes/b", &doc).unwrap();
        assert_eq!(d.text, "from summary");
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
