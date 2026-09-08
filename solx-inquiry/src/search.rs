//! Phase 2: run each search term against documents and/or actions (per
//! `scope`), normalize the two very different result shapes into one [`Hit`],
//! and merge duplicates hit by more than one term.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::host::{truncate, Host, Outcome};
use crate::params::Params;

pub const DOCUMENT_SEARCH_REF: &str = "/builtin/document/search_documents";
pub const ACTION_SEARCH_REF: &str = "/builtin/action/search_actions";
pub const TYPE_GET_REF: &str = "/builtin/type/entity_get_type";

/// Longest serialized `details` blob folded into one hit's context line.
/// Keeps a rich document body (or an action's full descriptive fields, now
/// including its parameter JSON Schema) from dominating the summarizer
/// prompt — small local models have the least context budget to spare for
/// it. A little more generous than a bare document snippet would need,
/// since a real parameter schema (required fields, property types) is
/// exactly the detail a caller wanting a runnable action call needs intact.
const MAX_DETAILS_CHARS: usize = 1500;

/// Floor on the per-term, per-source search `limit` — independent of
/// `max_results`, which caps the *final*, merged-and-ranked hit list.
/// Passing `max_results` straight through as each call's own limit meant a
/// caller asking for a short result (e.g. `max_results: 2`) also fetched
/// only the top 2 candidates *per term*, so the actual best match across
/// every term could be excluded before merging ever saw it, no matter how
/// good the post-merge ranking was. Candidates are cheap (a local FTS5
/// query); only the final list needs to stay small.
const MIN_SEARCH_LIMIT: usize = 10;

#[derive(Debug, Clone)]
pub struct Hit {
    pub source: &'static str,
    pub path: String,
    pub name: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub score: f32,
    pub matched_terms: Vec<String>,
    /// A document's full `contents` (returned inline by `search_documents`,
    /// no separate fetch needed), or an action's category/phrases/
    /// paramTypeRef/resultTypeRef plus (once enriched — see [`enrich_hits`])
    /// the JSON Schema for each type reference under
    /// `paramSchema`/`resultSchema`. `None` when there was nothing beyond
    /// title/summary to add.
    pub details: Option<Value>,
}

impl Hit {
    pub fn to_json(&self) -> Value {
        json!({
            "source": self.source,
            "path": self.path,
            "name": self.name,
            "title": self.title,
            "summary": self.summary,
            "score": self.score,
            "matched_terms": self.matched_terms,
            "details": self.details,
        })
    }

    /// One line of context for the summarizer prompt.
    pub fn to_context_line(&self, index: usize) -> String {
        let label = self.title.as_deref().unwrap_or(&self.name);
        let mut line = format!(
            "{}. [{}] {}/{} — \"{}\" (score {:.2})",
            index + 1,
            self.source,
            self.path,
            self.name,
            label,
            self.score
        );
        if let Some(summary) = &self.summary {
            line.push_str(&format!("\n   {summary}"));
        }
        if let Some(details) = &self.details {
            line.push_str(&format!("\n   details: {}", truncate(&details.to_string(), MAX_DETAILS_CHARS)));
        }
        line
    }
}

pub fn run_search(host: &dyn Host, p: &Params, terms: &[String]) -> Result<Vec<Hit>, Outcome> {
    // BTreeMap, not HashMap: the final sort is stable, but two hits can
    // still legitimately tie (e.g. two actions both ranked first for
    // different terms) - a HashMap's iteration order is arbitrary
    // (hash-randomized per process) and would make that tiebreak
    // unreproducible; a BTreeMap's is at least deterministic run to run.
    let mut merged: BTreeMap<(&'static str, String, String), Hit> = BTreeMap::new();

    for term in terms {
        if p.scope.searches_documents() {
            let hits = search_documents(host, p, term)?;
            merge(&mut merged, hits, term);
        }
        if p.scope.searches_actions() {
            let hits = search_actions(host, p, term)?;
            merge(&mut merged, hits, term);
        }
    }

    let mut all: Vec<Hit> = merged.into_values().collect();
    // Score first (descending), then how many distinct terms found it
    // (descending). The second key matters in practice: several actions
    // can each rank #1 under their own single generic term (every action
    // hit's score is a reciprocal rank - see `search_actions` - so "first
    // place under some term" is a common tie, not a rare one), and without
    // this, that tie would fall back to BTreeMap key order - alphabetical
    // by path, no more meaningful than the HashMap-order bug this whole
    // scoring scheme replaced. A hit corroborated by more of the model's
    // search terms is genuinely stronger evidence than one that only ever
    // matched a single word.
    all.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.matched_terms.len().cmp(&a.matched_terms.len()))
    });
    all.truncate(p.max_results);
    // Only for the final, already-capped set — fetching this per raw hit
    // before merging would multiply the cost by however many terms happened
    // to find it.
    enrich_hits(host, &mut all);
    Ok(all)
}

/// For each action hit in the final, already-capped list, fetch the JSON
/// Schema for its `paramTypeRef`/`resultTypeRef`, when present, folding
/// them into `details` as `paramSchema`/`resultSchema` — a bare reference
/// string isn't enough to construct a genuinely correct call or parse its
/// result, only to name where each shape lives. Document hits need no such
/// step: `search_documents` already returns full `contents` inline (see
/// `search_documents` below). Every fetch is independent and best-effort: a
/// failure (deleted meanwhile, transient error) just leaves that piece of
/// `details` missing rather than failing the whole inquiry over what is
/// strictly additional context.
fn enrich_hits(host: &dyn Host, hits: &mut [Hit]) {
    for hit in hits.iter_mut() {
        if hit.source == "action" {
            enrich_action_schemas(host, hit);
        }
    }
}

/// Fetch the JSON Schema for both `paramTypeRef` and `resultTypeRef`, when
/// present, folding them into `details` as `paramSchema`/`resultSchema`.
/// Independent and best-effort: a missing or failed fetch for one leaves the
/// other unaffected. Note `resultTypeRef` is documentation only — the host
/// never validates an action's actual output against it — so a present
/// `resultSchema` describes the *intended* shape, not a verified guarantee.
fn enrich_action_schemas(host: &dyn Host, hit: &mut Hit) {
    fetch_type_schema_into(host, hit, "paramTypeRef", "paramSchema");
    fetch_type_schema_into(host, hit, "resultTypeRef", "resultSchema");
}

fn fetch_type_schema_into(host: &dyn Host, hit: &mut Hit, ref_key: &str, schema_key: &str) {
    let Some(type_ref) = hit
        .details
        .as_ref()
        .and_then(|d| d.get(ref_key))
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let Some((path, name)) = crate::host::split_ref(&type_ref) else {
        return;
    };
    let payload = json!({ "path": path, "name": name });
    let call = match host.exec(TYPE_GET_REF, &payload) {
        Ok(c) if c.success => c,
        Ok(c) => {
            host.log(&format!(
                "solx-inquiry: entity_get_type {type_ref} failed: {}",
                c.message.unwrap_or_default()
            ));
            return;
        }
        Err(e) => {
            host.log(&format!("solx-inquiry: entity_get_type {type_ref} failed: {e}"));
            return;
        }
    };
    let Some(schema) = call.result.get("schema").filter(|v| !v.is_null()).cloned() else {
        return;
    };
    if let Some(obj) = hit.details.as_mut().and_then(Value::as_object_mut) {
        obj.insert(schema_key.to_string(), schema);
    }
}

fn merge(merged: &mut BTreeMap<(&'static str, String, String), Hit>, hits: Vec<Hit>, term: &str) {
    for mut hit in hits {
        let key = (hit.source, hit.path.clone(), hit.name.clone());
        match merged.get_mut(&key) {
            Some(existing) => {
                if hit.score > existing.score {
                    existing.score = hit.score;
                }
                if !existing.matched_terms.iter().any(|t| t == term) {
                    existing.matched_terms.push(term.to_string());
                }
            }
            None => {
                hit.matched_terms = vec![term.to_string()];
                merged.insert(key, hit);
            }
        }
    }
}

fn search_documents(host: &dyn Host, p: &Params, term: &str) -> Result<Vec<Hit>, Outcome> {
    let mut payload = json!({ "q": term, "limit": p.max_results.max(MIN_SEARCH_LIMIT) });
    if let Some(prefix) = &p.path_prefix {
        payload["pathPrefix"] = json!(prefix);
    }
    if let Some(type_ref) = &p.type_ref {
        payload["typeRef"] = json!(type_ref);
    }

    host.log(&format!("solx-inquiry: search_documents q={term:?}"));
    let call = host
        .exec(DOCUMENT_SEARCH_REF, &payload)
        .map_err(|e| search_dispatch_failure(DOCUMENT_SEARCH_REF, term, e))?;
    if !call.success {
        return Err(search_failure(DOCUMENT_SEARCH_REF, term, call.message, call.result));
    }

    // `search_documents` returns full `Document` rows (`items`), already
    // ordered by FTS5 rank server-side, same as `search_actions` — no
    // separate `entity_get_document` round trip needed to read a hit's
    // `contents`, and (like actions) no numeric score on the row either, so
    // score is this hit's reciprocal rank by position.
    let items = call
        .result
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(items
        .iter()
        .enumerate()
        .filter_map(|(i, d)| {
            Some(Hit {
                source: "document",
                path: d.get("path")?.as_str()?.to_string(),
                name: d.get("name")?.as_str()?.to_string(),
                title: d.get("title").and_then(Value::as_str).map(str::to_string),
                summary: d.get("summary").and_then(Value::as_str).map(str::to_string),
                score: reciprocal_rank(i),
                matched_terms: Vec::new(),
                details: d.get("contents").filter(|v| !v.is_null()).cloned(),
            })
        })
        .collect())
}

fn search_actions(host: &dyn Host, p: &Params, term: &str) -> Result<Vec<Hit>, Outcome> {
    let mut payload = json!({ "q": term, "limit": p.max_results.max(MIN_SEARCH_LIMIT), "excludeHidden": true });
    if let Some(prefix) = &p.path_prefix {
        payload["pathPrefix"] = json!(prefix);
    }

    host.log(&format!("solx-inquiry: search_actions q={term:?}"));
    let call = host
        .exec(ACTION_SEARCH_REF, &payload)
        .map_err(|e| search_dispatch_failure(ACTION_SEARCH_REF, term, e))?;
    if !call.success {
        return Err(search_failure(ACTION_SEARCH_REF, term, call.message, call.result));
    }

    let items = call
        .result
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Ordered by FTS5 rank server-side, same as `search_documents` (see
    // `reciprocal_rank`'s doc comment for why score comes from position).
    Ok(items
        .iter()
        .enumerate()
        .filter_map(|(i, a)| {
            Some(Hit {
                source: "action",
                path: a.get("path")?.as_str()?.to_string(),
                name: a.get("name")?.as_str()?.to_string(),
                title: a.get("caption").and_then(Value::as_str).map(str::to_string),
                summary: a.get("description").and_then(Value::as_str).map(str::to_string),
                score: reciprocal_rank(i),
                matched_terms: Vec::new(),
                details: action_details(a),
            })
        })
        .collect())
}

/// Turn a 0-based result position into a comparable score: first place
/// scores 1.0, second 0.5, third 0.33, ... Used for both `search_documents`
/// and `search_actions` now that neither exposes a numeric relevance score
/// on the row — both only guarantee the array is already ordered
/// best-match-first server-side (FTS5 `rank`). Position alone isn't enough
/// once hits from *different* search terms have to be merged into one
/// ranked list — see `run_search`'s merge — which is what this score is
/// actually for.
fn reciprocal_rank(position: usize) -> f32 {
    1.0 / (position as f32 + 1.0)
}

/// An action's category/phrases/paramTypeRef/resultTypeRef, already present
/// on the `search_actions` row — no extra call needed, unlike a document's
/// `contents`. `None` when all four are absent, so a bare action carries no
/// empty `{}` noise into the summarizer prompt.
fn action_details(a: &Value) -> Option<Value> {
    let mut details = serde_json::Map::new();
    if let Some(v) = a.get("category").filter(|v| !v.is_null()) {
        details.insert("category".to_string(), v.clone());
    }
    if let Some(v) = a.get("phrases").filter(|v| v.as_array().is_some_and(|a| !a.is_empty())) {
        details.insert("phrases".to_string(), v.clone());
    }
    if let Some(v) = a.get("paramTypeRef").filter(|v| !v.is_null()) {
        details.insert("paramTypeRef".to_string(), v.clone());
    }
    if let Some(v) = a.get("resultTypeRef").filter(|v| !v.is_null()) {
        details.insert("resultTypeRef".to_string(), v.clone());
    }
    if details.is_empty() {
        None
    } else {
        Some(Value::Object(details))
    }
}

fn search_dispatch_failure(action_ref: &str, term: &str, err: String) -> Outcome {
    Outcome::fail(
        "dispatch_error",
        format!("could not call {action_ref} for term {term:?}: {err}"),
        json!({ "stage": "search", "action_ref": action_ref, "term": term }),
    )
}

fn search_failure(action_ref: &str, term: &str, message: Option<String>, inner: Value) -> Outcome {
    Outcome::fail(
        "search_error",
        format!(
            "{action_ref} failed for term {term:?}: {}",
            message.unwrap_or_else(|| "no message".to_string())
        ),
        json!({ "stage": "search", "action_ref": action_ref, "term": term, "inner": inner }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_details_collects_present_fields() {
        let a = json!({ "category": "llm", "phrases": ["ask a question"], "paramTypeRef": "/x/Y", "resultTypeRef": "/x/Z" });
        assert_eq!(
            action_details(&a),
            Some(json!({ "category": "llm", "phrases": ["ask a question"], "paramTypeRef": "/x/Y", "resultTypeRef": "/x/Z" }))
        );
    }

    #[test]
    fn action_details_omits_absent_and_empty_fields() {
        // No category/phrases/paramTypeRef/resultTypeRef at all.
        assert_eq!(action_details(&json!({ "caption": "x" })), None);
        // An empty phrases array is the same as absent - no point citing it.
        assert_eq!(
            action_details(&json!({ "category": "llm", "phrases": [] })),
            Some(json!({ "category": "llm" }))
        );
    }

    #[test]
    fn context_line_truncates_a_large_details_blob() {
        let hit = Hit {
            source: "document",
            path: "/p".into(),
            name: "n".into(),
            title: None,
            summary: None,
            score: 1.0,
            matched_terms: vec![],
            details: Some(json!({ "body": "x".repeat(2000) })),
        };
        let line = hit.to_context_line(0);
        assert!(line.contains("bytes total"), "{line}");
        assert!(line.len() < MAX_DETAILS_CHARS + 200, "line was {} chars", line.len());
    }

    #[test]
    fn context_line_omits_details_when_none() {
        let hit = Hit {
            source: "action",
            path: "/p".into(),
            name: "n".into(),
            title: None,
            summary: Some("does a thing".into()),
            score: 1.0,
            matched_terms: vec![],
            details: None,
        };
        assert!(!hit.to_context_line(0).contains("details:"));
    }
}
