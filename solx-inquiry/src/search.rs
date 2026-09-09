//! Phase 2: run each search term against documents and/or actions (per
//! `scope`), normalize the two very different result shapes into one [`Hit`],
//! and merge duplicates hit by more than one term.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::host::{truncate, Host, Outcome};
use crate::params::{Params, Scope};

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

/// Everything [`run_search_with`] needs, lifted out of [`Params`] so a caller
/// that has no `inquire` params at all — `instruct`, whose inquiries each pick
/// their *own* scope — can drive the same search without inventing one.
/// `inquire` still goes through [`SearchSpec::from_params`], so its behaviour
/// is unchanged.
#[derive(Debug, Clone)]
pub struct SearchSpec {
    pub scope: Scope,
    pub max_results: usize,
    pub path_prefix: Option<String>,
    pub type_ref: Option<String>,
}

impl SearchSpec {
    pub fn from_params(p: &Params) -> Self {
        SearchSpec {
            scope: p.scope.clone(),
            max_results: p.max_results,
            path_prefix: p.path_prefix.clone(),
            type_ref: p.type_ref.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub source: &'static str,
    pub path: String,
    pub name: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    /// Fused relevance: the sum of this hit's reciprocal rank under every
    /// search term that found it (see [`merge`]). Comparable within one result
    /// set and unbounded above, not a 0..1 confidence.
    pub score: f32,
    pub matched_terms: Vec<String>,
    /// The document's `typeRef`, straight off the `search_documents` row.
    /// `None` for an action hit. Not surfaced in [`Hit::to_json`] - it exists
    /// so `instruct` can tell its own document kinds apart from evidence, and
    /// adding a field to `inquire`'s documented result shape for that would be
    /// a change nothing asked for.
    pub type_ref: Option<String>,
    /// A document's full `contents` (returned inline by `search_documents`,
    /// no separate fetch needed), or an action's category/phrases/
    /// paramTypeRef/resultTypeRef plus (once enriched — see [`enrich_hits`])
    /// the JSON Schema for each type reference under
    /// `paramSchema`/`resultSchema`. `None` when there was nothing beyond
    /// title/summary to add.
    pub details: Option<Value>,
}

impl Hit {
    /// The joined `/path/name` reference — how a hit is cited in a response
    /// and how a script step's `action_ref` is checked against what the
    /// search actually surfaced.
    pub fn reference(&self) -> String {
        format!("{}/{}", self.path, self.name)
    }

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

/// A type's JSON Schema, keyed by its `/path/name` reference, cached for the
/// lifetime of one [`TypeCache`] value.
///
/// `entity_get_type` is fetched per action hit per type-ref field
/// (`paramTypeRef`/`resultTypeRef`), and nothing before this cache stopped the
/// same reference from being fetched twice: two actions can share a param
/// type, and `instruct` calls [`run_search_with`] once per inquiry — up to
/// three times in one run — so an action two inquiries both surface (a common
/// helper like `search_documents` is a likely one) paid for its schema twice
/// over. `None` is cached too, so a reference that is absent or fails to
/// resolve is not retried for the rest of whatever scope this cache is built
/// for, rather than hitting `entity_get_type` again for every hit that names
/// it.
///
/// Scoped by the caller, not global: [`run_search`] builds a fresh one per
/// call (a single search phase has nothing to share across calls), while
/// `instruct::run` builds one and threads it through every inquiry's
/// [`crate::inquiry::prepare`], which is where the cross-inquiry sharing
/// actually pays off.
#[derive(Debug, Default)]
pub struct TypeCache(BTreeMap<String, Option<Value>>);

impl TypeCache {
    /// The schema for `type_ref`, fetching and caching it on a first miss.
    fn get_or_fetch(&mut self, host: &dyn Host, type_ref: &str) -> Option<Value> {
        if let Some(cached) = self.0.get(type_ref) {
            return cached.clone();
        }
        let schema = fetch_type_schema(host, type_ref);
        self.0.insert(type_ref.to_string(), schema.clone());
        schema
    }
}

pub fn run_search(host: &dyn Host, p: &Params, terms: &[String]) -> Result<Vec<Hit>, Outcome> {
    let mut cache = TypeCache::default();
    run_search_with(host, &SearchSpec::from_params(p), terms, &mut cache)
}

pub fn run_search_with(
    host: &dyn Host,
    spec: &SearchSpec,
    terms: &[String],
    type_cache: &mut TypeCache,
) -> Result<Vec<Hit>, Outcome> {
    // BTreeMap, not HashMap: the final sort is stable, but two hits can
    // still legitimately tie (e.g. two actions both ranked first for
    // different terms) - a HashMap's iteration order is arbitrary
    // (hash-randomized per process) and would make that tiebreak
    // unreproducible; a BTreeMap's is at least deterministic run to run.
    let mut merged: BTreeMap<(&'static str, String, String), Hit> = BTreeMap::new();

    for term in terms {
        if spec.scope.searches_documents() {
            let hits = search_documents(host, spec, term)?;
            merge(&mut merged, hits, term);
        }
        if spec.scope.searches_actions() {
            let hits = search_actions(host, spec, term)?;
            merge(&mut merged, hits, term);
        }
    }

    let mut all: Vec<Hit> = merged.into_values().collect();
    // Fused score first (descending), then how many distinct terms found it.
    // The second key is now only a tiebreak between hits whose fused scores
    // land on exactly the same number, because corroboration is already the
    // first key's doing - see `merge`.
    all.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.matched_terms.len().cmp(&a.matched_terms.len()))
    });
    all.truncate(spec.max_results);
    // Only for the final, already-capped set — fetching this per raw hit
    // before merging would multiply the cost by however many terms happened
    // to find it.
    enrich_hits(host, &mut all, type_cache);
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
fn enrich_hits(host: &dyn Host, hits: &mut [Hit], type_cache: &mut TypeCache) {
    for hit in hits.iter_mut() {
        if hit.source == "action" {
            enrich_action_schemas(host, hit, type_cache);
        }
    }
}

/// Fetch the JSON Schema for both `paramTypeRef` and `resultTypeRef`, when
/// present, folding them into `details` as `paramSchema`/`resultSchema`.
/// Independent and best-effort: a missing or failed fetch for one leaves the
/// other unaffected. Note `resultTypeRef` is documentation only — the host
/// never validates an action's actual output against it — so a present
/// `resultSchema` describes the *intended* shape, not a verified guarantee.
fn enrich_action_schemas(host: &dyn Host, hit: &mut Hit, type_cache: &mut TypeCache) {
    fetch_type_schema_into(host, hit, "paramTypeRef", "paramSchema", type_cache);
    fetch_type_schema_into(host, hit, "resultTypeRef", "resultSchema", type_cache);
}

fn fetch_type_schema_into(
    host: &dyn Host,
    hit: &mut Hit,
    ref_key: &str,
    schema_key: &str,
    type_cache: &mut TypeCache,
) {
    let Some(type_ref) = hit
        .details
        .as_ref()
        .and_then(|d| d.get(ref_key))
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let Some(schema) = type_cache.get_or_fetch(host, &type_ref) else {
        return;
    };
    if let Some(obj) = hit.details.as_mut().and_then(Value::as_object_mut) {
        obj.insert(schema_key.to_string(), schema);
    }
}

/// The actual `entity_get_type` round trip, uncached — [`TypeCache`] is the
/// only caller. Best-effort: a missing or failed fetch (deleted meanwhile, a
/// transient error) is logged and returns `None` rather than failing the
/// whole inquiry over what is strictly additional context.
fn fetch_type_schema(host: &dyn Host, type_ref: &str) -> Option<Value> {
    let (path, name) = crate::host::split_ref(type_ref)?;
    let payload = json!({ "path": path, "name": name });
    let call = match host.exec(TYPE_GET_REF, &payload) {
        Ok(c) if c.success => c,
        Ok(c) => {
            host.log(&format!(
                "solx-inquiry: entity_get_type {type_ref} failed: {}",
                c.message.unwrap_or_default()
            ));
            return None;
        }
        Err(e) => {
            host.log(&format!("solx-inquiry: entity_get_type {type_ref} failed: {e}"));
            return None;
        }
    };
    call.result.get("schema").filter(|v| !v.is_null()).cloned()
}

/// Fold one term's results into the running set — Reciprocal Rank Fusion: a
/// hit's score is the **sum** of its reciprocal rank under every term that
/// found it.
///
/// Taking the maximum instead (what this used to do) throws away corroboration
/// entirely: a document ranked #1 under one generic word scores 1.0 and beats
/// a document ranked #2 under the precise term at 0.5, however many other terms
/// also found the second one. That bites hardest right where the terms are
/// weakest, since [`crate::terms::expand_terms`] deliberately searches the
/// individual words of a phrase, and a common word like "list" will rank
/// something #1 whether or not it is relevant. Summing makes each additional
/// term that found a hit add evidence rather than being ignored, which is
/// exactly the property the old `matched_terms.len()` tiebreak was reaching
/// for and could only apply on an exact tie.
///
/// The score is therefore unbounded above rather than capped at 1.0: with three
/// terms a hit can reach ~1.83. It is a within-result-set comparison, not a
/// probability.
fn merge(merged: &mut BTreeMap<(&'static str, String, String), Hit>, hits: Vec<Hit>, term: &str) {
    for mut hit in hits {
        let key = (hit.source, hit.path.clone(), hit.name.clone());
        match merged.get_mut(&key) {
            Some(existing) => {
                // Guarded together so one term can never contribute twice,
                // whatever a search happens to return.
                if !existing.matched_terms.iter().any(|t| t == term) {
                    existing.score += hit.score;
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

fn search_documents(host: &dyn Host, spec: &SearchSpec, term: &str) -> Result<Vec<Hit>, Outcome> {
    let mut payload = json!({ "q": term, "limit": spec.max_results.max(MIN_SEARCH_LIMIT) });
    if let Some(prefix) = &spec.path_prefix {
        payload["pathPrefix"] = json!(prefix);
    }
    if let Some(type_ref) = &spec.type_ref {
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
                type_ref: d.get("typeRef").and_then(Value::as_str).map(str::to_string),
                details: d.get("contents").filter(|v| !v.is_null()).cloned(),
            })
        })
        .collect())
}

fn search_actions(host: &dyn Host, spec: &SearchSpec, term: &str) -> Result<Vec<Hit>, Outcome> {
    let mut payload = json!({ "q": term, "limit": spec.max_results.max(MIN_SEARCH_LIMIT), "excludeHidden": true });
    if let Some(prefix) = &spec.path_prefix {
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
                type_ref: None,
                details: action_details(a),
            })
        })
        .collect())
}

/// Turn a 0-based result position into one term's contribution: first place
/// contributes 1.0, second 0.5, third 0.33, ... Used for both
/// `search_documents` and `search_actions` now that neither exposes a numeric
/// relevance score on the row — both only guarantee the array is already
/// ordered best-match-first server-side (FTS5 `rank`). Position alone isn't
/// enough once hits from *different* search terms have to be merged into one
/// ranked list — [`merge`] sums these across terms, which is what this value
/// is actually for.
fn reciprocal_rank(position: usize) -> f32 {
    1.0 / (position as f32 + 1.0)
}

/// The capability tag solx-core enforces: a call to an action carrying it
/// stops for a human decision.
pub const DESTRUCTIVE_CAPABILITY: &str = "solx:destructive";

/// True when this action hit is one solx-core would treat as destructive, as
/// far as a wasm guest can tell.
///
/// `solx-config`'s `ToolPolicy::is_destructive` derives that from three
/// things: the `solx:destructive` tag, an `actionType` of `command` or
/// `webhook` (unconditionally - those are shell and outbound HTTP), and a
/// configured `tool_destructive` list. `search_actions` returns the first two
/// on the row, so both are checked here.
///
/// **The third is invisible from inside a guest.** It lives in
/// `solx-config.json` and there is no built-in action that exposes the policy,
/// so an action an operator listed there reads as non-destructive to this
/// check. That is why a script's `destructive` list is documented as "what
/// could be seen", not "everything that will stop" - and why the built-ins
/// are a live example of the gap: they are seeded with empty `capabilities`
/// and are `internal`, so `entity_delete_action` does not trip either of the
/// two checks available here.
pub fn is_destructive(hit: &Hit) -> bool {
    if hit.source != "action" {
        return false;
    }
    let Some(details) = hit.details.as_ref() else {
        return false;
    };
    let tagged = details
        .get("capabilities")
        .and_then(Value::as_array)
        .is_some_and(|caps| caps.iter().any(|c| c.as_str() == Some(DESTRUCTIVE_CAPABILITY)));
    let executable = matches!(
        details.get("actionType").and_then(Value::as_str),
        Some("command") | Some("webhook")
    );
    tagged || executable
}

/// An action's descriptive fields, already present on the `search_actions`
/// row — no extra call needed, unlike a document's `contents`. `None` when all
/// of them are absent, so a bare action carries no empty `{}` noise into the
/// prompt.
///
/// `capabilities` and `actionType` are here for one reason in particular:
/// together they are as much of solx-core's destructive test as a guest can
/// see (`solx:destructive`, plus `command`/`webhook` being unconditionally
/// destructive). An action that will stop for a human decision has to be
/// visible as such to anything choosing between actions, and to anyone handed
/// a script that calls one. `actionType` also just says how an action runs,
/// which is useful context in its own right.
fn action_details(a: &Value) -> Option<Value> {
    let mut details = serde_json::Map::new();
    if let Some(v) = a.get("category").filter(|v| !v.is_null()) {
        details.insert("category".to_string(), v.clone());
    }
    if let Some(v) = a.get("capabilities").filter(|v| v.as_array().is_some_and(|a| !a.is_empty())) {
        details.insert("capabilities".to_string(), v.clone());
    }
    if let Some(v) = a.get("actionType").filter(|v| !v.is_null()) {
        details.insert("actionType".to_string(), v.clone());
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
    fn action_details_carries_what_the_destructive_test_needs() {
        let a = json!({ "category": "ops", "capabilities": ["solx:destructive", "write"], "actionType": "command" });
        let details = action_details(&a).unwrap();
        assert_eq!(details["capabilities"], json!(["solx:destructive", "write"]));
        assert_eq!(details["actionType"], json!("command"));
    }

    #[test]
    fn is_destructive_reads_the_tag_and_the_executable_action_types() {
        let hit = |details: Option<Value>| Hit {
            source: "action",
            path: "/p".into(),
            name: "n".into(),
            title: None,
            summary: None,
            score: 1.0,
            matched_terms: vec![],
            type_ref: None,
            details,
        };
        assert!(is_destructive(&hit(Some(json!({ "capabilities": ["solx:destructive"] })))));
        // command/webhook are unconditionally destructive to solx-core: shell
        // and outbound HTTP, whatever their capabilities say.
        assert!(is_destructive(&hit(Some(json!({ "actionType": "command" })))));
        assert!(is_destructive(&hit(Some(json!({ "actionType": "webhook" })))));
        assert!(!is_destructive(&hit(Some(json!({ "actionType": "wasm" })))));
        // An unprefixed tag is free-form description, not the reserved one.
        assert!(!is_destructive(&hit(Some(json!({ "capabilities": ["destructive"] })))));
        assert!(!is_destructive(&hit(Some(json!({ "category": "ops" })))));
        assert!(!is_destructive(&hit(None)));

        // A *document* is never destructive, whatever its contents happen to
        // say - `details` for a document hit is the document body, which is
        // model-adjacent text rather than a catalogue field.
        let mut doc = hit(Some(json!({ "capabilities": ["solx:destructive"] })));
        doc.source = "document";
        assert!(!is_destructive(&doc));
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
            type_ref: None,
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
            type_ref: None,
            details: None,
        };
        assert!(!hit.to_context_line(0).contains("details:"));
    }
}
