//! The `instruct` action: an instruction in, responses / memories / scripts
//! out.
//!
//! ```text
//! recall (skills + memories)      no llm
//! intent                          1 detached llm call
//!   direct? -> assemble, write the session, return
//! inquiry fan-out                 N <= 3 detached llm calls, in parallel
//! assemble                        no llm
//! session write
//! ```
//!
//! `1 + N` model calls, four at the cap. There is no result-synthesis phase:
//! each inquiry's findings are returned as they are, and a caller who wants
//! them reconciled into one answer adds that layer themselves. That is a
//! deliberate omission rather than a missing piece — a synthesis call would
//! cost a fifth round trip to re-say what the responses already say, and would
//! be the one place in this pipeline where a model could contradict its own
//! grounded output with nothing to check it against.
//!
//! `instruct` **saves nothing**. Memories come back as ready-to-save document
//! payloads and scripts as `.solx` text; deciding what to keep and what to run
//! is the caller's, which is what keeps a pipeline that writes model output
//! from also being the thing that acts on it. The one exception is the session
//! document, which is this action's own record of what it did.

use serde_json::{json, Value};

use crate::console;
use crate::fanout::{self, Job};
use crate::host::{truncate, Host, Outcome};
use crate::instruct_params::{
    self, InstructParams, INSTRUCT_AUTHOR, MEMORY_TEXT_CAP, MEMORY_TYPE_REF,
};
use crate::inquiry::{self, Prepared, Response};
use crate::intent::{self, Mode};
use crate::recall;
use crate::script::Script;
use crate::search;
use crate::session;

pub fn run(host: &dyn Host, params: &Value) -> Outcome {
    let p = match instruct_params::parse(params) {
        Ok(p) => p,
        Err(outcome) => return outcome,
    };

    let recalled = recall::recall(host, &p);
    console::print(
        host,
        &console::phase_tag(console::PHASE_RECALL),
        &format!(
            "recalled {} skill(s) and {} memor(ies)",
            recalled.skills.len(),
            recalled.memories.len()
        ),
        recalled.to_json(),
    );

    let stored = session::load(host, &p);

    let intent = match intent::decide(host, &p, &recalled, &stored) {
        Ok(i) => i,
        Err(outcome) => return outcome,
    };
    console::print(
        host,
        &console::phase_tag(console::PHASE_INTENT),
        match intent.mode {
            Mode::Direct => "answering directly",
            Mode::Inquire => "inquiries proposed",
        },
        intent.to_json(),
    );

    let mut responses: Vec<Response> = Vec::new();
    let mut scripts: Vec<Script> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut hits: Vec<Value> = Vec::new();

    // A direct answer and a set of inquiries are not alternatives. A model that
    // says something *and* names what to look up has produced both, and the
    // mode is only a reading of whether there is work to do (see
    // `intent::parse`) - so keeping the answer costs nothing and discarding it
    // would throw away the one thing the intent call actually wrote.
    if let Some(text) = &intent.response {
        responses.push(Response {
            text: text.clone(),
            title: None,
            // Reported, never minted: the intent phase has searched nothing, so
            // this is model prior rather than a grounded finding. See
            // `mint_memories`.
            memory: intent.memory,
            tags: Vec::new(),
            citations: Vec::new(),
            inquiry: None,
        });
    }
    // Anything the intent phase answered directly. Whether the *inquiries*
    // produced anything is what decides a failed run below, so it cannot be
    // read off `responses.is_empty()` once a direct answer can sit alongside
    // them.
    let direct_responses = responses.len();

    if intent.mode == Mode::Inquire {
        // Search first, every inquiry, before any model call — the fan-out
        // wants all its payloads in hand so it can start them back to back.
        let mut prepared: Vec<Prepared> = Vec::new();
        // Shared across every inquiry in this run, not one per inquiry: two
        // inquiries can surface the same action (a common helper like
        // `search_documents` is a likely one), and without this each would
        // fetch its `paramSchema` separately. See `search::TypeCache`.
        let mut type_cache = search::TypeCache::default();
        for (index, one) in intent.inquiries.iter().enumerate() {
            console::print(
                host,
                &console::inquiry_step_tag(index, console::STEP_TERMS),
                &one.question,
                one.to_json(),
            );
            match inquiry::prepare(host, &p, index, one.clone(), &mut type_cache) {
                Ok(ready) => {
                    console::print(
                        host,
                        &console::inquiry_step_tag(index, console::STEP_HITS),
                        &format!("{} hit(s)", ready.hits.len()),
                        json!({
                            "count": ready.hits.len(),
                            "refs": ready.hits.iter().map(|h| Value::String(h.reference())).collect::<Vec<_>>(),
                        }),
                    );
                    prepared.push(ready);
                }
                // A failed search is that inquiry's failure, not the run's:
                // the others may still answer the instruction.
                Err(outcome) => {
                    fanout::log_job_failure(host, index, &outcome);
                    errors.push(error_entry(index, &outcome));
                }
            }
        }

        let jobs: Vec<Job> = prepared
            .iter()
            .map(|ready| Job {
                label: console::inquiry_tag(ready.index),
                payload: inquiry::payload(&p, &recalled, ready),
            })
            .collect();

        let results = match fanout::run(host, &p.llm, jobs) {
            Ok(results) => results,
            // Only cancellation aborts the whole fan-out. Whatever was
            // assembled before it is handed back under `partial`, so a
            // cancelled run is still worth something to its caller.
            Err(mut outcome) => {
                if let Some(obj) = outcome.output.as_object_mut() {
                    obj.insert(
                        "partial".to_string(),
                        json!({
                            "instruction": p.instruction,
                            "intent": intent.to_json(),
                            "errors": errors,
                        }),
                    );
                }
                return outcome;
            }
        };

        for (ready, result) in prepared.iter().zip(results) {
            let index = ready.index;
            match result {
                Ok(value) => {
                    if ready.inquiry.is_actions() {
                        let (found, said) = inquiry::parse_scripts(&value, &ready.catalogue);
                        console::print(
                            host,
                            &console::inquiry_step_tag(index, console::STEP_RESULT),
                            &format!("{} script(s)", found.len()),
                            json!({ "scripts": found.iter().map(Script::to_json).collect::<Vec<_>>(), "notes": said }),
                        );
                        scripts.extend(found);
                        notes.extend(said);
                    } else {
                        let found = inquiry::parse_responses(&value, index);
                        console::print(
                            host,
                            &console::inquiry_step_tag(index, console::STEP_RESULT),
                            &format!("{} response(s)", found.len()),
                            json!({ "responses": found.iter().map(Response::to_json).collect::<Vec<_>>() }),
                        );
                        responses.extend(found);
                    }
                }
                Err(outcome) => {
                    fanout::log_job_failure(host, index, &outcome);
                    errors.push(error_entry(index, &outcome));
                }
            }
            hits.extend(ready.hits.iter().map(|h| {
                let mut value = h.to_json();
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("inquiry".to_string(), json!(index));
                }
                value
            }));
        }

        // Every inquiry failing is a failed run, not an empty answer: the
        // caller asked for work that was attempted and did not happen, and
        // reporting that as success with no responses would hide it. Measured
        // against what the *inquiries* produced - a direct answer the intent
        // phase happened to give alongside them is not evidence that the work
        // it asked for happened.
        if !errors.is_empty() && responses.len() == direct_responses && scripts.is_empty() {
            return Outcome::fail(
                // Not `llm_error`: what failed may have been every *search*,
                // and each entry in `errors` already carries its own kind.
                "inquiry_error",
                "every inquiry failed",
                json!({ "stage": "inquiry", "errors": errors, "intent": intent.to_json() }),
            );
        }
    }

    let memories = mint_memories(&p, &responses);
    if p.memory_path.is_none() {
        let flagged = responses.iter().filter(|r| mintable(r)).count();
        if flagged > 0 {
            // Silence here would look like the model judged nothing worth
            // keeping, when in fact it did and nowhere was given to keep it.
            notes.push(format!(
                "{flagged} response(s) were flagged as worth remembering, but no memory_path \
                 was given, so no memory payloads were produced"
            ));
        }
    }
    // Same reasoning, different reason for the refusal: the caller is told the
    // model wanted to keep this, and why it was not kept anyway.
    if intent.memory && intent.response.is_some() {
        notes.push(
            "the direct answer was flagged as worth remembering, but the intent phase searches \
             nothing, so an ungrounded answer is never minted as a memory"
                .to_string(),
        );
    }

    let turn = json!({
        "instruction": p.instruction,
        "mode": match intent.mode { Mode::Direct => "direct", Mode::Inquire => "inquire" },
        "inquiries": intent.inquiries.iter().map(intent::Inquiry::to_json).collect::<Vec<_>>(),
        "responses": responses.iter().map(Response::to_json).collect::<Vec<_>>(),
        "scripts": scripts.iter().map(Script::to_json).collect::<Vec<_>>(),
        "memories": memories,
        "next_prompt": intent.next_prompt,
        "notes": notes,
        "errors": errors,
    });

    if let Err(warning) = session::save(host, &p, &stored, turn) {
        console::warn(
            host,
            &console::phase_tag(console::PHASE_RESULT),
            &warning,
            json!({ "session": p.session }),
        );
        warnings.push(warning);
    }

    console::print(
        host,
        &console::phase_tag(console::PHASE_RESULT),
        &format!(
            "{} response(s), {} memor(ies), {} script(s)",
            responses.len(),
            memories.len(),
            scripts.len()
        ),
        json!({
            "responses": responses.len(),
            "memories": memories.len(),
            "scripts": scripts.len(),
            "errors": errors.len(),
            "warnings": warnings,
        }),
    );

    Outcome::ok(json!({
        "instruction": p.instruction,
        "model": p.model,
        "session": p.session,
        "intent": intent.to_json(),
        "responses": responses.iter().map(Response::to_json).collect::<Vec<_>>(),
        "memories": memories,
        "scripts": scripts.iter().map(Script::to_json).collect::<Vec<_>>(),
        "next_prompt": intent.next_prompt,
        "hits": hits,
        "notes": notes,
        "errors": errors,
        "warnings": warnings,
    }))
}

fn error_entry(index: usize, outcome: &Outcome) -> Value {
    json!({
        "inquiry": index,
        "kind": outcome.output.get("kind").cloned().unwrap_or(Value::Null),
        "error": outcome.message.clone(),
    })
}

/// True for a response that may become a saved memory: the model flagged it,
/// **and** an inquiry produced it.
///
/// The second half is what keeps an ungrounded assertion from being written
/// down and recalled later as reference material. The intent phase sees skills,
/// memories and history but no search results at all, so a `direct` answer is
/// model prior - it cites nothing, and nothing checked it. A memory minted from
/// one would re-enter a later run's prompt looking exactly like a finding that
/// had been grounded in something, which is the same loop
/// [`crate::inquiry::is_reference_material`] closes from the other side.
///
/// The flag itself still rides on the response either way. It is the model's
/// judgement about the finding, and a `notes[]` entry says when one was refused
/// for being ungrounded, so nothing is silently dropped.
fn mintable(r: &Response) -> bool {
    r.memory && r.inquiry.is_some()
}

/// Turn every mintable response into a payload the caller can hand straight to
/// `entity_save_document`.
///
/// Empty when no `memory_path` was given: memories are off, so there is
/// nowhere to say they belong. The model's `memory` flag still rides on the
/// response either way - it is its judgement about the finding, and discarding
/// it would hide the one thing a caller needs in order to decide whether
/// turning memories on is worth it.
///
/// `author` records which action produced the note. It is provenance for
/// whoever reads the document later - a memory is model output, and a document
/// that does not say so looks exactly like something a person wrote. Nothing
/// in this pipeline reads it back; keeping memories out of a later inquiry's
/// evidence is the declared `memory_path`'s job.
///
/// The text goes into `summary` as well as `contents.text`, which is what
/// makes recall a single search with no follow-up reads (see
/// [`crate::recall`]). The name is a slug plus a short hash of the text:
/// deterministic, because a wasm guest has no random source, and useful,
/// because `entity_save_document` is an upsert on `(path, name)` — so
/// re-deriving the same memory overwrites itself instead of accumulating near
/// duplicates every time the instruction is repeated.
fn mint_memories(p: &InstructParams, responses: &[Response]) -> Vec<Value> {
    let Some(memory_path) = p.memory_path.as_deref() else {
        return Vec::new();
    };
    responses
        .iter()
        .filter(|r| mintable(r))
        .map(|r| {
            let text = truncate(&r.text, MEMORY_TEXT_CAP);
            let name = memory_name(r.title.as_deref().unwrap_or(&r.text), &text);
            json!({
                "path": memory_path,
                "name": name,
                "typeRef": MEMORY_TYPE_REF,
                "author": INSTRUCT_AUTHOR,
                "title": r.title.clone().unwrap_or_else(|| "Inquiry memory".to_string()),
                "summary": text,
                "contents": {
                    "text": text,
                    "tags": r.tags,
                    "instruction": p.instruction,
                    "session": p.session,
                },
            })
        })
        .collect()
}

/// How many words of the title/text go into a memory's name before the hash.
const SLUG_WORDS: usize = 6;
const SLUG_CAP: usize = 48;

fn memory_name(label: &str, text: &str) -> String {
    let slug: Vec<String> = label
        .split_whitespace()
        .take(SLUG_WORDS)
        .map(|w| {
            w.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect();
    let mut slug = slug.join("-");
    slug.truncate(SLUG_CAP);
    if slug.is_empty() {
        slug.push_str("memory");
    }
    format!("{slug}-{:08x}", fnv1a(text))
}

/// FNV-1a, 32-bit. A hash, not a checksum: all it has to do is make two
/// different memories very unlikely to collide on one name, while keeping the
/// *same* memory's name stable across runs.
fn fnv1a(s: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in s.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn params() -> InstructParams {
        params_with(Some("/notes/memories"))
    }

    fn params_with(memory_path: Option<&str>) -> InstructParams {
        let mut raw = json!({
            "instruction": "what about auth?",
            "model": "m",
            "session": "/solx-inquiry/sessions/s",
        });
        if let Some(path) = memory_path {
            raw["memory_path"] = json!(path);
        }
        instruct_params::parse(&raw).unwrap()
    }

    fn response(text: &str, memory: bool) -> Response {
        Response {
            text: text.to_string(),
            title: None,
            memory,
            tags: vec!["auth".to_string()],
            citations: Vec::new(),
            inquiry: Some(0),
        }
    }

    #[test]
    fn only_flagged_responses_become_memories() {
        let responses = vec![response("durable fact", true), response("just this run", false)];
        let memories = mint_memories(&params(), &responses);
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0]["summary"], json!("durable fact"));
        assert_eq!(memories[0]["contents"]["text"], json!("durable fact"));
        assert_eq!(memories[0]["path"], json!("/notes/memories"));
        assert_eq!(memories[0]["author"], json!(INSTRUCT_AUTHOR));
        assert_eq!(memories[0]["typeRef"], json!(MEMORY_TYPE_REF));
    }

    #[test]
    fn no_memory_path_mints_nothing_however_the_model_flagged_things() {
        let responses = vec![response("durable fact", true), response("also durable", true)];
        assert!(mint_memories(&params_with(None), &responses).is_empty());
    }

    #[test]
    fn the_text_is_in_the_summary_so_recall_needs_no_second_read() {
        let memories = mint_memories(&params(), &[response("tokens expire hourly", true)]);
        assert_eq!(memories[0]["summary"], memories[0]["contents"]["text"]);
    }

    #[test]
    fn the_same_memory_mints_the_same_name_so_a_repeat_upserts_itself() {
        let a = mint_memories(&params(), &[response("auth uses session tokens", true)]);
        let b = mint_memories(&params(), &[response("auth uses session tokens", true)]);
        assert_eq!(a[0]["name"], b[0]["name"]);
        let c = mint_memories(&params(), &[response("auth uses api keys", true)]);
        assert_ne!(a[0]["name"], c[0]["name"]);
    }

    #[test]
    fn a_name_is_a_readable_slug_not_just_a_hash() {
        let memories = mint_memories(&params(), &[response("Auth uses session tokens!", true)]);
        let name = memories[0]["name"].as_str().unwrap();
        assert!(name.starts_with("auth-uses-session-tokens-"), "{name}");
    }

    #[test]
    fn a_memory_with_no_usable_words_still_gets_a_name() {
        let memories = mint_memories(&params(), &[response("!!! ???", true)]);
        let name = memories[0]["name"].as_str().unwrap();
        assert!(name.starts_with("memory-"), "{name}");
    }
}
