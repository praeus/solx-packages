//! Phase 2 of `instruct`: one inquiry's search, its llm payload, and the
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
//! [`crate::script`] renders. The two differ only in prompt, schema, and how
//! their result is read.

use serde_json::{json, Value};

use crate::host::{Host, Outcome};
use crate::instruct_params::{InstructParams, MEMORY_TYPE_REF, SESSION_TYPE_REF, SKILL_TYPE_REF};
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
    /// What the search surfaced, in the form [`crate::script::render`] checks a
    /// proposed step against: which actions may be called, which of them are
    /// destructive, and each one's parameter schema. Empty for a document
    /// inquiry, which proposes no steps.
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
/// `type_cache` is the caller's, not this function's: `instruct::run` builds
/// one and passes the same one to every inquiry's `prepare`, so an action two
/// inquiries both surface (a common helper like `search_documents` is a likely
/// one) fetches its schema once rather than once per inquiry that finds it.
pub fn prepare(
    host: &dyn Host,
    p: &InstructParams,
    index: usize,
    inquiry: Inquiry,
    type_cache: &mut search::TypeCache,
) -> Result<Prepared, Outcome> {
    let spec = SearchSpec {
        scope: inquiry.kind.clone(),
        max_results: p.max_results,
        // Each inquiry picks its own scope, so it takes the path prefix that
        // matches: `document_path_prefix` and `action_path_prefix` are
        // independent because one `instruct` call can fan out inquiries of
        // both kinds with different reach in mind.
        path_prefix: if inquiry.is_actions() {
            p.action_path_prefix.clone()
        } else {
            p.document_path_prefix.clone()
        },
        // A document type filter must not be applied to an action inquiry:
        // `search_actions` ignores it anyway, and carrying it would only
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
        // Already on the hit: `search::enrich_hits` fetched it for the prompt,
        // and the renderer needs the same schema to decide whether a capture
        // reference is spelled quoted or bare. No extra call.
        param_schemas: hits
            .iter()
            .filter(|h| h.source == "action")
            .filter_map(|h| {
                let schema = h.details.as_ref()?.get("paramSchema")?.clone();
                Some((h.reference(), schema))
            })
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
/// `author` is stamped on everything `instruct` writes, but nothing here
/// branches on it: it is provenance for a human or a later tool reading the
/// document, not a control this pipeline depends on.
fn is_reference_material(p: &InstructParams, hit: &Hit) -> bool {
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
pub fn payload(p: &InstructParams, recalled: &Recalled, prepared: &Prepared) -> Value {
    let actions = prepared.inquiry.is_actions();

    let mut system = if actions {
        let base = p
            .action_prompt
            .clone()
            .unwrap_or_else(|| prompts::DEFAULT_ACTION_INQUIRY_PROMPT.to_string());
        format!("{base}\n\n{}", prompts::SOLX_SCRIPT_PRIMER)
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
    let lines: Vec<String> = prepared
        .hits
        .iter()
        .enumerate()
        .map(|(i, h)| h.to_context_line(i))
        .collect();
    let heading = if prepared.inquiry.is_actions() { "Available actions" } else { "Search results" };
    format!("Question: {question}\n\n{heading}:\n{}", lines.join("\n"))
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

/// Read an action inquiry's answer, rendering each proposed step list into
/// `.solx`. Returns the surviving scripts and any notes from scripts that did
/// not survive, so a caller is told *why* an inquiry produced nothing runnable
/// rather than just being handed an empty list.
pub fn parse_scripts(result: &Value, catalogue: &Catalogue) -> (Vec<Script>, Vec<String>) {
    let content = result.pointer("/message/content").and_then(Value::as_str).unwrap_or("").trim();
    let mut scripts = Vec::new();
    let mut notes = Vec::new();

    let Some(items) = parse_object(content).and_then(|v| v.get("scripts").and_then(Value::as_array).cloned())
    else {
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
        match script::render(title, model_notes.clone(), &steps, catalogue) {
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

fn parse_object(text: &str) -> Option<Value> {
    if let Ok(v @ Value::Object(_)) = serde_json::from_str(text) {
        return Some(v);
    }
    let text = text.strip_prefix("```")?;
    let text = text.strip_prefix("json").unwrap_or(text);
    let (body, _) = text.split_once("```")?;
    match serde_json::from_str(body.trim()) {
        Ok(v @ Value::Object(_)) => Some(v),
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

    fn params() -> InstructParams {
        crate::instruct_params::parse(&json!({
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
        let mut off = crate::instruct_params::parse(&json!({
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
                {"action_ref":"/builtin/document/search_documents","params":{"q":"auth"},"capture":"hits"}
            ]}]}"#,
        );
        let catalogue = allowing(&["/builtin/document/search_documents"]);
        let (scripts, notes) = parse_scripts(&result, &catalogue);
        assert_eq!(scripts.len(), 1);
        assert!(notes.is_empty());
        assert!(scripts[0].source.starts_with("$hits = exec /builtin/document/search_documents"));
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
}
