//! Phase 2 of `multi_inquire`: one inquiry's search, its llm payload, and the
//! parsing of what comes back.
//!
//! The searching happens *before* the fan-out and the parsing *after* it, so
//! this module deliberately does not call the model itself — [`crate::fanout`]
//! owns that, because running the calls concurrently is the whole point.
//! Splitting it this way also keeps every one of these functions pure enough
//! to test without a host.
//!
//! A document inquiry answers in prose (responses, some flagged as worth
//! remembering); an action inquiry answers in structured steps that
//! [`crate::script`] validates against the catalogue below and hands back as
//! JSON, for a caller to execute directly. The two differ only in prompt,
//! schema, and how their result is read.

use serde_json::{json, Value};

use crate::host::{Host, Outcome};
use crate::params::multi::{MultiInquireParams, MEMORY_TYPE_REF, SESSION_TYPE_REF, SKILL_TYPE_REF};
use crate::intent::Inquiry;
use crate::prompts;
use crate::recall::{memory_block, skill_block, skills_for, Recalled};
use crate::script::{self, Catalogue, Script};
use crate::search::{self, Hit, SearchSpec};

/// What one inquiry found, before its model call.
pub struct Prepared {
    pub index: usize,
    pub inquiry: Inquiry,
    pub hits: Vec<Hit>,
    /// What the search surfaced, in the form [`crate::script::assemble`] checks
    /// a proposed step against: which actions may be called and which of them
    /// are destructive. Empty for a document inquiry, which proposes no steps.
    pub catalogue: Catalogue,
}

/// One finding from a document inquiry.
#[derive(Debug, Clone)]
pub struct Response {
    pub text: String,
    pub title: Option<String>,
    pub memory: bool,
    pub tags: Vec<String>,
    pub citations: Vec<String>,
    /// Which inquiry produced it; `None` for a direct answer from the intent
    /// phase, which had no inquiry behind it.
    pub inquiry: Option<usize>,
}

impl Response {
    pub fn to_json(&self) -> Value {
        json!({
            "text": self.text,
            "title": self.title,
            "memory": self.memory,
            "tags": self.tags,
            "citations": self.citations,
            "inquiry": self.inquiry,
        })
    }
}

/// Run one inquiry's search. Reuses `inquire`'s own merge/rank/enrich path
/// verbatim through [`search::run_search_with`], so an action hit arrives with
/// its `paramSchema` already fetched — which is exactly what the action prompt
/// needs in order to ask for correct parameters rather than plausible ones.
///
/// `type_cache` is the caller's, not this function's: `multi::run` builds
/// one and passes the same one to every inquiry's `prepare`, so an action two
/// inquiries both surface (a common helper like `search-documents` is a likely
/// one) fetches its schema once rather than once per inquiry that finds it.
pub fn prepare(
    host: &dyn Host,
    p: &MultiInquireParams,
    index: usize,
    inquiry: Inquiry,
    type_cache: &mut search::TypeCache,
) -> Result<Prepared, Outcome> {
    let spec = SearchSpec {
        scope: inquiry.kind.clone(),
        max_results: p.max_results,
        // Each inquiry picks its own scope, so it takes the path prefix that
        // matches: `document_path_prefix` and `action_path_prefix` are
        // independent because one `multi_inquire` call can fan out inquiries of
        // both kinds with different reach in mind.
        path_prefix: if inquiry.is_actions() {
            p.action_path_prefix.clone()
        } else {
            p.document_path_prefix.clone()
        },
        // A document type filter must not be applied to an action inquiry:
        // `search-actions` ignores it anyway, and carrying it would only
        // suggest it did something.
        type_ref: if inquiry.is_actions() { None } else { p.type_ref.clone() },
    };
    let mut hits = search::run_search_with(host, &spec, &inquiry.terms, type_cache)?;
    // Skills and memories reach the model through `recall`, framed as
    // reference material. Letting them come back *again* as search hits would
    // double-count them and, worse, present them as evidence: a live MCP run
    // searched for "authentication", matched the seeded `memories` skill
    // (whose text includes a worked example of a well-written memory), and
    // reported that example as a finding, cited to the skill. Guidance is not
    // a document about the subject it is teaching.
    hits.retain(|h| !is_reference_material(p, h));
    let catalogue = Catalogue {
        allowed: hits
            .iter()
            .filter(|h| h.source == "action")
            .map(Hit::reference)
            .collect(),
        destructive: hits
            .iter()
            .filter(|h| search::is_destructive(h))
            .map(Hit::reference)
            .collect(),
    };
    Ok(Prepared { index, inquiry, hits, catalogue })
}

/// True for a hit that is this package's own bookkeeping rather than something
/// the instruction asked about.
///
/// Two tests, one per escape route.
///
/// **Path** covers skills, and memories when a `memory_path` was given. It is
/// reliable because both locations are declared rather than guessed: the same
/// value a caller names is used to recall memories, to stamp the payloads
/// handed back, and here. The place they said memories live is the place this
/// stops treating as evidence.
///
/// **Type** covers everything the path check cannot reach: a session, which
/// has no path to filter on at all because the caller names its full
/// reference; and a memory saved by an *earlier* run, which must stay out of
/// evidence even on a run with memories switched off and no path to compare
/// against.
///
/// The failure both exist to prevent was observed live, twice in a row: a
/// fabricated sentence about session tokens was reported as a finding cited to
/// the seeded skill whose worked example contained it, and then, once that was
/// filtered, reported again cited to the session document that had recorded
/// the first answer. Model output must not become evidence by having been
/// written down.
///
/// `author` is stamped on every document payload `multi_inquire` produces,
/// but nothing here branches on it: it is provenance for a human or a later
/// tool reading the document, not a control this pipeline depends on.
fn is_reference_material(p: &MultiInquireParams, hit: &Hit) -> bool {
    if hit.source != "document" {
        return false;
    }
    if under(&hit.path, &p.skills_path) {
        return true;
    }
    if p.memory_path.as_deref().is_some_and(|root| under(&hit.path, root)) {
        return true;
    }
    matches!(
        hit.type_ref.as_deref(),
        Some(SESSION_TYPE_REF) | Some(MEMORY_TYPE_REF) | Some(SKILL_TYPE_REF)
    )
}

/// Prefix match on whole path segments, so `/solx-inquiry/skills` does not
/// also swallow a user's unrelated `/solx-inquiry/skills-research`.
fn under(path: &str, root: &str) -> bool {
    path == root || path.strip_prefix(root).is_some_and(|rest| rest.starts_with('/'))
}

/// The chat payload for one prepared inquiry.
pub fn payload(p: &MultiInquireParams, recalled: &Recalled, prepared: &Prepared, context_block: Option<&str>) -> Value {
    let actions = prepared.inquiry.is_actions();

    let mut system = if actions {
        let base = p
            .action_prompt
            .clone()
            .unwrap_or_else(|| prompts::DEFAULT_ACTION_INQUIRY_PROMPT.to_string());
        format!("{base}\n\n{}", prompts::ACTION_STEPS_PRIMER)
    } else {
        p.document_prompt
            .clone()
            .unwrap_or_else(|| prompts::DEFAULT_DOCUMENT_INQUIRY_PROMPT.to_string())
    };

    // Appended, never substituted for the default: the intent phase can steer
    // an inquiry, but it cannot talk the pipeline out of its own grounding
    // rules.
    if let Some(amendment) = &prepared.inquiry.amendment {
        system.push_str("\n\nFor this question in particular: ");
        system.push_str(amendment);
    }

    // Named by the caller for this instruction, so unlike a skill or a
    // memory it rides along with every inquiry regardless of kind - a
    // document supplied as context can bear on which action to call just as
    // much as it bears on a document inquiry's answer.
    if let Some(block) = context_block {
        system.push_str("\n\n");
        system.push_str(block);
    }

    let applicable = skills_for(&recalled.skills, actions, &prepared.catalogue.allowed);
    if let Some(block) = skill_block(&applicable) {
        system.push_str("\n\n");
        system.push_str(&block);
    }
    // Memories are prior findings, so they inform an answer about what is
    // known; they have nothing to say about which action to call.
    if !actions {
        if let Some(block) = memory_block(&recalled.memories) {
            system.push_str("\n\n");
            system.push_str(&block);
        }
    }

    let format = if actions { prompts::scripts_schema() } else { prompts::responses_schema() };

    let payload = json!({
        "model": p.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user_message(prepared) },
        ],
        "format": format,
        "think": false,
        "options": { "temperature": 0 },
    });
    crate::params::apply_llm_overrides(payload, &p.llm)
}

fn user_message(prepared: &Prepared) -> String {
    let question = &prepared.inquiry.question;
    if prepared.hits.is_empty() {
        return if prepared.inquiry.is_actions() {
            format!(
                "Question: {question}\n\nNo matching actions were found. Return no scripts and \
                 say plainly in \"notes\" that nothing available can do this, without inventing \
                 an action."
            )
        } else {
            format!(
                "Question: {question}\n\nNo matching documents were found. Say plainly that \
                 nothing was found, without inventing an answer."
            )
        };
    }
    let heading = if prepared.inquiry.is_actions() { "Available actions" } else { "Search results" };
    format!("Question: {question}\n\n{heading}:\n{}", search::context_block(&prepared.hits))
}

/// Read a document inquiry's answer.
///
/// A model that ignored `format` and answered in prose still answered, so its
/// content becomes a single uncited response rather than an error. The cost of
/// being strict here is a whole inquiry's work discarded over formatting.
pub fn parse_responses(result: &Value, index: usize) -> Vec<Response> {
    let content = result.pointer("/message/content").and_then(Value::as_str).unwrap_or("").trim();
    if content.is_empty() {
        return Vec::new();
    }

    let parsed = parse_object(content)
        .and_then(|v| v.get("responses").and_then(Value::as_array).cloned());

    let Some(items) = parsed else {
        return vec![Response {
            text: content.to_string(),
            title: None,
            memory: false,
            tags: Vec::new(),
            citations: Vec::new(),
            inquiry: Some(index),
        }];
    };

    items
        .iter()
        .filter_map(|item| {
            let text = item.get("text").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())?;
            Some(Response {
                text: text.to_string(),
                title: item
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                memory: item.get("memory").and_then(Value::as_bool).unwrap_or(false),
                tags: string_array(item.get("tags")),
                citations: string_array(item.get("citations")),
                inquiry: Some(index),
            })
        })
        .collect()
}

/// Read an action inquiry's answer, validating each proposed step list
/// against the catalogue. Returns the surviving scripts and any notes from
/// scripts that did not survive, so a caller is told *why* an inquiry
/// produced nothing runnable rather than just being handed an empty list.
///
/// Accepts three shapes the model might reasonably produce:
///
/// * `{"scripts": [{"title": ..., "steps": [...]}, ...]}` — the documented shape
/// * `[{"title": ..., "steps": [...]}, ...]` — a bare array, what many cloud
///   models actually emit when the schema's `format` constraint is treated
///   as advisory. Treated as one implicit-script list (the model's
///   "run these in order" answer), wrapped to the documented form.
/// * `[step, step, ...]` — a bare array of *step objects* (each with
///   `action_ref`/`params`), what a model that skipped the script wrapper
///   entirely emits. Wrapped into one script whose steps are those steps.
pub fn parse_scripts(result: &Value, catalogue: &Catalogue) -> (Vec<Script>, Vec<String>) {
    let content = result.pointer("/message/content").and_then(Value::as_str).unwrap_or("").trim();
    let mut scripts = Vec::new();
    let mut notes = Vec::new();

    let Some(items) = extract_scripts_items(content) else {
        // No structured answer at all. Prose from an action inquiry is not a
        // script and must not be presented as one - it is reported as a note.
        if !content.is_empty() {
            notes.push(content.to_string());
        }
        return (scripts, notes);
    };

    for item in &items {
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let model_notes = item
            .get("notes")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let steps = script::parse_steps(item);
        match script::assemble(title, model_notes.clone(), &steps, catalogue) {
            Some(rendered) => scripts.push(rendered),
            None => {
                // A "script" with nothing runnable in it is how the prompt
                // asks the model to say "these actions cannot do that", so its
                // note is the useful part.
                if let Some(note) = model_notes {
                    notes.push(note);
                } else if !steps.is_empty() {
                    notes.push(
                        "a proposed script named only actions that were not among the search \
                         results, so it was discarded"
                            .to_string(),
                    );
                }
            }
        }
    }

    (scripts, notes)
}

/// Reduce the model's answer string to a list of script-shaped objects
/// (each with optional `title`, `notes`, and a `steps` array). See
/// [`parse_scripts`] for the accepted shapes.
fn extract_scripts_items(content: &str) -> Option<Vec<Value>> {
    let parsed = parse_object(content)?;
    let items = match &parsed {
        Value::Object(_) => parsed.get("scripts").and_then(Value::as_array).cloned()?,
        // Bare array of script objects: `[{title, steps, ...}, ...]`.
        Value::Array(items) if items.iter().all(|v| v.is_object() && v.get("steps").is_some()) => {
            items.clone()
        }
        // Bare array of step objects: `[{action_ref, params}, ...]` — wrap
        // into one implicit script so the rest of the pipeline can treat it
        // uniformly.
        Value::Array(items) if items.iter().all(|v| v.get("action_ref").is_some()) => vec![json!({
            "title": null,
            "notes": null,
            "steps": items.clone(),
        })],
        // Anything else (array of primitives, mixed shapes) is treated as
        // "no structured answer".
        _ => return None,
    };
    Some(items)
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse the model's answer string into a JSON value, accepting both the
/// bare shape and a markdown fence. Returns `None` if neither form yields
/// a JSON object OR array — prose without a JSON envelope is not a script.
///
/// An array is accepted (not just an object) because the schema constraint
/// `format` in ollama is advisory: many models emit a bare JSON array of
/// steps as their "scripts answer", without the `{"scripts": [...]}` wrap.
/// That output is still parseable as `Value::Array`, and the caller of
/// `parse_object` (`parse_scripts`) treats an array as a single implicit
/// script — a model that returns `[step1, step2]` is plainly saying "run
/// these in order", which is what `scripts[]` would say anyway. Rejecting
/// that shape here is why action inquiries have been returning 0 scripts
/// against cloud models.
fn parse_object(text: &str) -> Option<Value> {
    if let Ok(v @ (Value::Object(_) | Value::Array(_))) = serde_json::from_str(text) {
        return Some(v);
    }
    let text = text.strip_prefix("```")?;
    let text = text.strip_prefix("json").unwrap_or(text);
    let (body, _) = text.split_once("```")?;
    match serde_json::from_str(body.trim()) {
        Ok(v @ (Value::Object(_) | Value::Array(_))) => Some(v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(text: &str) -> Value {
        json!({ "message": { "role": "assistant", "content": text } })
    }

    fn doc_hit(path: &str) -> Hit {
        typed_hit(path, None)
    }

    fn typed_hit(path: &str, type_ref: Option<&str>) -> Hit {
        Hit {
            source: "document",
            path: path.to_string(),
            name: "x".into(),
            title: None,
            summary: None,
            score: 1.0,
            matched_terms: vec![],
            type_ref: type_ref.map(str::to_string),
            details: None,
        }
    }

    /// A catalogue that allows every named reference and knows nothing about
    /// their parameter shapes.
    fn allowing(refs: &[&str]) -> Catalogue {
        Catalogue {
            allowed: refs.iter().map(|r| r.to_string()).collect(),
            ..Catalogue::default()
        }
    }

    fn params() -> MultiInquireParams {
        crate::params::multi::parse(&json!({
            "instruction": "i", "model": "m", "session": "/s/one",
            "memory_path": "/solx-inquiry/memories",
        }))
        .unwrap()
    }

    #[test]
    fn recall_material_is_not_also_admitted_as_evidence() {
        // A live MCP run searched "authentication", matched the seeded
        // `memories` skill (which contains a worked example of a good memory),
        // and reported that example as a finding cited to the skill.
        let p = params();
        assert!(is_reference_material(&p, &doc_hit("/solx-inquiry/skills")));
        assert!(is_reference_material(&p, &doc_hit("/solx-inquiry/memories")));
        assert!(is_reference_material(&p, &doc_hit("/solx-inquiry/skills/nested")));
        // Ordinary documents are untouched, including ones that merely share a
        // prefix with the skills path.
        assert!(!is_reference_material(&p, &doc_hit("/notes")));
        assert!(!is_reference_material(&p, &doc_hit("/solx-inquiry/skills-research")));
    }

    #[test]
    fn a_stored_memory_stays_out_of_evidence_even_with_memories_switched_off() {
        // Turning memories off stops this run reading and writing them; it
        // must not turn what earlier runs wrote into fair game as evidence.
        // With no memory_path there is no path to compare against, so the type
        // check is the only thing standing between a past answer and being
        // cited as fact.
        let mut off = crate::params::multi::parse(&json!({
            "instruction": "i", "model": "m", "session": "/s/one",
        }))
        .unwrap();
        assert_eq!(off.memory_path, None);
        assert!(is_reference_material(&off, &typed_hit("/notes", Some(MEMORY_TYPE_REF))));
        // An ordinary note is still evidence.
        assert!(!is_reference_material(&off, &typed_hit("/notes", Some("/types/docs/Document"))));
        off.memory_path = Some("/notes/memories".to_string());
        assert!(is_reference_material(&off, &doc_hit("/notes/memories")));
    }

    #[test]
    fn a_session_document_is_never_evidence_wherever_it_lives() {
        // The self-reinforcing case: a run records its own responses in a
        // session whose title and summary are indexed, so a later inquiry
        // would otherwise find it and cite a previous run's model output as
        // fact. Sessions are addressed by full reference, so there is no path
        // to filter on - only the type.
        let p = params();
        assert!(is_reference_material(&p, &typed_hit("/anywhere/at/all", Some(SESSION_TYPE_REF))));
        assert!(is_reference_material(&p, &typed_hit("/notes", Some(MEMORY_TYPE_REF))));
        assert!(is_reference_material(&p, &typed_hit("/notes", Some(SKILL_TYPE_REF))));
        assert!(!is_reference_material(&p, &typed_hit("/notes", Some("/types/docs/Document"))));
        assert!(!is_reference_material(&p, &typed_hit("/notes", None)));
    }

    #[test]
    fn an_action_hit_is_never_reference_material() {
        // Only documents are recalled, so an action is always evidence -
        // and dropping one would silently shrink the catalogue and make its
        // steps look invented.
        let p = params();
        let mut action = doc_hit("/solx-inquiry/skills");
        action.source = "action";
        assert!(!is_reference_material(&p, &action));
    }

    #[test]
    fn parses_several_responses_with_their_flags() {
        let result = content(
            r#"{"responses":[
                {"text":"auth uses tokens","memory":true,"tags":["auth"],"citations":["/notes/auth"]},
                {"text":"tokens expire hourly"}
            ]}"#,
        );
        let responses = parse_responses(&result, 1);
        assert_eq!(responses.len(), 2);
        assert!(responses[0].memory);
        assert_eq!(responses[0].citations, vec!["/notes/auth"]);
        assert!(!responses[1].memory);
        assert_eq!(responses[1].inquiry, Some(1));
    }

    #[test]
    fn prose_from_a_document_inquiry_becomes_one_uncited_response() {
        let responses = parse_responses(&content("Authentication uses session tokens."), 0);
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].text, "Authentication uses session tokens.");
        assert!(!responses[0].memory);
        assert!(responses[0].citations.is_empty());
    }

    #[test]
    fn an_empty_answer_yields_no_responses() {
        assert!(parse_responses(&content("   "), 0).is_empty());
        assert!(parse_responses(&json!({}), 0).is_empty());
    }

    #[test]
    fn parses_scripts_and_renders_them() {
        let result = content(
            r#"{"scripts":[{"title":"Find auth","steps":[
                {"action_ref":"/builtin/document/search-documents","params":{"q":"auth"},"capture":"hits"}
            ]}]}"#,
        );
        let catalogue = allowing(&["/builtin/document/search-documents"]);
        let (scripts, notes) = parse_scripts(&result, &catalogue);
        assert_eq!(scripts.len(), 1);
        assert!(notes.is_empty());
        assert_eq!(scripts[0].steps.len(), 1);
        assert_eq!(scripts[0].steps[0].action_ref, "/builtin/document/search-documents");
        assert_eq!(scripts[0].steps[0].capture.as_deref(), Some("hits"));
    }

    #[test]
    fn prose_from_an_action_inquiry_is_a_note_never_a_script() {
        // The dangerous failure would be handing a caller free text labelled
        // as something to run.
        let (scripts, notes) =
            parse_scripts(&content("I could not find a way to do that."), &Catalogue::default());
        assert!(scripts.is_empty());
        assert_eq!(notes, vec!["I could not find a way to do that."]);
    }

    #[test]
    fn a_script_of_invented_actions_is_dropped_and_explained() {
        let result = content(
            r#"{"scripts":[{"steps":[{"action_ref":"/invented/thing","params":{}}]}]}"#,
        );
        let (scripts, notes) = parse_scripts(&result, &allowing(&["/real/thing"]));
        assert!(scripts.is_empty());
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("not among the search results"), "{notes:?}");
    }

    #[test]
    fn an_empty_script_carrying_only_notes_keeps_the_explanation() {
        let result = content(r#"{"scripts":[{"notes":"nothing installed can transcode video","steps":[]}]}"#);
        let (scripts, notes) = parse_scripts(&result, &Catalogue::default());
        assert!(scripts.is_empty());
        assert_eq!(notes, vec!["nothing installed can transcode video"]);
    }

    /// A bare JSON array of script objects — what cloud models emit when
    /// they ignore the `format` constraint's `{scripts: [...]}` wrap and
    /// produce just the array. Treated as one implicit-script list.
    #[test]
    fn a_bare_array_of_script_objects_is_accepted() {
        let result = content(
            r#"[{"title":"find","steps":[{"action_ref":"/builtin/document/search-documents","params":{"q":"auth"},"capture":"hits"}]}]"#,
        );
        let catalogue = allowing(&["/builtin/document/search-documents"]);
        let (scripts, notes) = parse_scripts(&result, &catalogue);
        assert_eq!(scripts.len(), 1, "notes={notes:?}");
        assert!(notes.is_empty());
        assert_eq!(scripts[0].title.as_deref(), Some("find"));
    }

    /// A bare JSON array of step objects (no script wrapper) — what a model
    /// that skipped the `scripts` layer entirely emits. Wrapped into one
    /// implicit script.
    #[test]
    fn a_bare_array_of_step_objects_is_wrapped_into_one_script() {
        let result = content(
            r#"[{"action_ref":"/builtin/document/search-documents","params":{"q":"auth"}},{"action_ref":"/builtin/document/get-field","params":{"path":"/n","name":"a","field":"x"},"capture":"x"}]"#,
        );
        let catalogue = allowing(&[
            "/builtin/document/search-documents",
            "/builtin/document/get-field",
        ]);
        let (scripts, notes) = parse_scripts(&result, &catalogue);
        assert_eq!(scripts.len(), 1, "notes={notes:?}");
        assert_eq!(scripts[0].steps.len(), 2);
        assert_eq!(scripts[0].steps[1].capture.as_deref(), Some("x"));
    }

    /// A markdown-fenced bare array is also accepted — ollama-style
    /// ```json\n[...]\n``` envelopes were getting rejected before the fix.
    #[test]
    fn a_fenced_bare_array_is_accepted() {
        let result = content(
            "```json\n[{\"action_ref\":\"/real/thing\",\"params\":{}}]\n```",
        );
        let (scripts, notes) = parse_scripts(&result, &allowing(&["/real/thing"]));
        assert_eq!(scripts.len(), 1, "notes={notes:?}");
        assert_eq!(scripts[0].steps[0].action_ref, "/real/thing");
    }

    /// A bare array of primitives (no objects) is *not* a script — falls
    /// through to "no structured answer" and the content becomes a note.
    #[test]
    fn a_bare_array_of_primitives_is_a_note() {
        let (scripts, notes) =
            parse_scripts(&content("[1, 2, 3]"), &Catalogue::default());
        assert!(scripts.is_empty());
        assert_eq!(notes, vec!["[1, 2, 3]"]);
    }
}
