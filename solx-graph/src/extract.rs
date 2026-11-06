//! `extract-document` `{path?, name, model?}`: read a solx document, ask
//! Ollama (through solx-ollama's `ollama-chat`) for the concepts in it and
//! how they relate, and replace that document's graph with them.
//!
//! Both calls go back through solx (`Call::host`), as the package's start
//! action, so they need its `actionConfig.scopes`
//! (`read:document`, `exec:/builtin/document/entity-get-document`,
//! `exec:/packages/solx-ollama/ollama-chat`).
//!
//! The prompt and the extraction schema are placeholders, to be replaced by
//! the Python implementation's.

use serde_json::{json, Value};
use solx_package_log::background::Call;

use crate::graph::{Concept, DocumentRef, Extraction, Graph, Relation};

const DOCUMENT_ACTION: (&str, &str) = ("/builtin/document", "entity-get-document");
const CHAT_ACTION: (&str, &str) = ("/packages/solx-ollama", "ollama-chat");

/// Used when the call names no `model` and `SOLX_GRAPH_MODEL` isn't set.
const DEFAULT_MODEL: &str = "llama3.2";

/// How much of a document the model is shown.
const MAX_DOCUMENT_CHARS: usize = 24_000;

const CHAT_TIMEOUT_SECS: u64 = 600;

const SYSTEM_PROMPT: &str = "You extract a knowledge graph from a document. \
List the distinct concepts it discusses (people, things, ideas, technologies), each with a \
short description, and the relations between them, each as from / relation / to, where from \
and to are names from your concept list and relation is a short verb phrase. Use the \
document's own terms. Answer with JSON only.";

pub async fn extract_document(graph: &Graph, call: &Call) -> Result<Value, String> {
    let name = call.params["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .ok_or("missing required param: name")?;
    let path = call.params["path"].as_str().unwrap_or("/");
    let reference = format!("{}/{name}", path.trim_end_matches('/'));
    let model = call.params["model"]
        .as_str()
        .map(str::to_string)
        .or_else(|| std::env::var("SOLX_GRAPH_MODEL").ok().filter(|m| !m.is_empty()))
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    let document = call
        .host()
        .exec(DOCUMENT_ACTION.0, DOCUMENT_ACTION.1, json!({ "path": path, "name": name }))
        .await
        .map_err(|e| format!("reading {reference}: {e}"))?;
    let text = document_text(&document);
    solx_package_log::info(&format!("extracting {reference} ({} chars) with {model}", text.len())).await;

    let answer = call
        .host()
        .exec(CHAT_ACTION.0, CHAT_ACTION.1, chat_request(&model, &text))
        .await
        .map_err(|e| format!("asking {model} about {reference}: {e}"))?;
    let content = answer["message"]["content"]
        .as_str()
        .ok_or_else(|| format!("{model} answered without message.content: {answer}"))?;
    let extraction = parse_extraction(content)?;

    let title = document["title"].as_str();
    let written = graph.replace_document(&DocumentRef { reference: &reference, title }, &extraction)?;
    solx_package_log::info(&format!(
        "{reference}: {} concepts, {} relations, {} triples",
        written.concepts, written.relations, written.triples
    ))
    .await;
    Ok(json!({
        "document": reference,
        "graph": format!("urn:solx:graph:{reference}"),
        "model": model,
        "concepts": written.concepts,
        "relations": written.relations,
        "triples": written.triples,
    }))
}

/// What the model reads: the title, summary and contents, the contents as
/// text when they're a string and as JSON otherwise, cut to
/// [`MAX_DOCUMENT_CHARS`].
fn document_text(document: &Value) -> String {
    let mut text = String::new();
    for field in ["title", "summary"] {
        if let Some(value) = document[field].as_str().filter(|v| !v.trim().is_empty()) {
            text.push_str(value);
            text.push_str("\n\n");
        }
    }
    match &document["contents"] {
        Value::String(s) => text.push_str(s),
        Value::Null => {}
        other => text.push_str(&serde_json::to_string_pretty(other).unwrap_or_default()),
    }
    match text.char_indices().nth(MAX_DOCUMENT_CHARS) {
        Some((cut, _)) => text[..cut].to_string(),
        None => text,
    }
}

/// The schema Ollama constrains the answer to (`format`).
fn extraction_schema() -> Value {
    json!({
        "type": "object",
        "required": ["concepts", "relations"],
        "properties": {
            "concepts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": { "type": "string" },
                        "description": { "type": "string" },
                    },
                },
            },
            "relations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["from", "relation", "to"],
                    "properties": {
                        "from": { "type": "string" },
                        "relation": { "type": "string" },
                        "to": { "type": "string" },
                    },
                },
            },
        },
    })
}

fn chat_request(model: &str, text: &str) -> Value {
    json!({
        "model": model,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": text },
        ],
        "format": extraction_schema(),
        "options": { "temperature": 0 },
        "timeout_secs": CHAT_TIMEOUT_SECS,
    })
}

/// The model's answer as an [`Extraction`]. Entries with no name are
/// dropped, and so are relations whose ends aren't among the concepts:
/// a model sometimes relates things it never listed.
fn parse_extraction(content: &str) -> Result<Extraction, String> {
    let answer: Value = serde_json::from_str(content.trim())
        .map_err(|e| format!("the model's answer isn't JSON ({e}): {}", preview(content)))?;
    let text = |v: &Value, key: &str| v[key].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);

    let concepts: Vec<Concept> = answer["concepts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some(Concept {
                name: text(c, "name")?,
                description: text(c, "description"),
            })
        })
        .collect();
    let known: std::collections::HashSet<String> = concepts.iter().map(|c| crate::graph::slug(&c.name)).collect();
    let relations = answer["relations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            Some(Relation {
                from: text(r, "from")?,
                relation: text(r, "relation")?,
                to: text(r, "to")?,
            })
        })
        .filter(|r| known.contains(&crate::graph::slug(&r.from)) && known.contains(&crate::graph::slug(&r.to)))
        .collect();
    Ok(Extraction { concepts, relations })
}

fn preview(s: &str) -> String {
    s.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use solx_package_log::background::{serve_with, Concurrency};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};

    async fn send(to_bg: &mut DuplexStream, message: Value) {
        to_bg.write_all(format!("{message}
").as_bytes()).await.unwrap();
    }

    async fn next(from_bg: &mut Lines<BufReader<DuplexStream>>) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(5), from_bg.next_line())
            .await
            .expect("the background should have written a line")
            .unwrap()
            .expect("the background closed stdout");
        serde_json::from_str(&line).unwrap()
    }

    /// The next message that isn't logging (a `console.print` request).
    async fn next_call(from_bg: &mut Lines<BufReader<DuplexStream>>) -> Value {
        loop {
            let message = next(from_bg).await;
            if message["op"] != "console.print" {
                return message;
            }
        }
    }

    #[test]
    fn document_text_uses_title_summary_and_contents() {
        let doc = json!({"title": "T", "summary": "S", "contents": {"body": "B"}});
        let text = document_text(&doc);
        assert!(text.starts_with("T\n\nS\n\n"), "{text}");
        assert!(text.contains("\"body\": \"B\""), "{text}");
        let long = json!({"contents": "x".repeat(MAX_DOCUMENT_CHARS + 10)});
        assert_eq!(document_text(&long).chars().count(), MAX_DOCUMENT_CHARS);
    }

    #[test]
    fn relations_to_unlisted_concepts_are_dropped() {
        let e = parse_extraction(
            r#"{"concepts": [{"name": "Oxigraph", "description": "an RDF store"}, {"name": "  "}, {"name": "RDF"}],
                "relations": [{"from": "oxigraph", "relation": "stores", "to": "RDF"},
                              {"from": "Oxigraph", "relation": "uses", "to": "RocksDB"}]}"#,
        )
        .unwrap();
        assert_eq!(e.concepts.len(), 2);
        assert_eq!(e.relations, [Relation { from: "oxigraph".into(), relation: "stores".into(), to: "RDF".into() }]);
        assert!(parse_extraction("not json").unwrap_err().contains("isn't JSON"));
    }

    /// The whole operation over the background protocol, with a fake host
    /// answering the document read and the chat.
    #[tokio::test]
    async fn extract_document_reads_asks_and_writes() {
        let graph = Arc::new(Graph::in_memory());
        let (mut to_bg, bg_in) = tokio::io::duplex(1 << 16);
        let (bg_out, from_bg) = tokio::io::duplex(1 << 16);
        let mut from_bg = BufReader::new(from_bg).lines();
        let served = {
            let graph = graph.clone();
            tokio::spawn(serve_with(bg_in, bg_out, Concurrency::OneAtATime, move |call| {
                let graph = graph.clone();
                async move { extract_document(&graph, &call).await }
            }))
        };
        send(&mut to_bg, json!({"kind": "request", "id": 1, "op": "extract-document",
                               "params": {"path": "/notes", "name": "graphs", "model": "m"}})).await;

        let read = next_call(&mut from_bg).await;
        assert_eq!(read["params"], json!({"path": "/builtin/document", "name": "entity-get-document",
                                           "params": {"path": "/notes", "name": "graphs"}}));
        let doc = json!({"action": "/builtin/document/entity-get-document", "success": true,
                         "result": {"title": "Graphs", "contents": "Oxigraph stores RDF."}});
        send(&mut to_bg, json!({"kind": "response", "id": read["id"], "ok": true, "result": doc})).await;

        let chat = next_call(&mut from_bg).await;
        assert_eq!(chat["params"]["path"], "/packages/solx-ollama");
        assert_eq!(chat["params"]["params"]["model"], "m");
        let content = r#"{"concepts": [{"name": "Oxigraph"}, {"name": "RDF"}],
                          "relations": [{"from": "Oxigraph", "relation": "stores", "to": "RDF"}]}"#;
        let answer = json!({"action": "/packages/solx-ollama/ollama-chat", "success": true,
                            "result": {"message": {"role": "assistant", "content": content}}});
        send(&mut to_bg, json!({"kind": "response", "id": chat["id"], "ok": true, "result": answer})).await;

        let done = next_call(&mut from_bg).await;
        assert_eq!(done["id"], 1);
        assert_eq!(done["ok"], true, "{done}");
        assert_eq!(done["result"]["concepts"], 2);
        assert_eq!(done["result"]["relations"], 1);
        assert_eq!(graph.store().len().unwrap() as u64, done["result"]["triples"].as_u64().unwrap());

        drop(to_bg);
        served.await.unwrap().unwrap();
    }
}
