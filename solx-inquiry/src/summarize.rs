//! Phase 3: hand the inquiry plus the merged hits back to the LLM action and
//! ask for a final answer in plain text (no `format` — freeform prose is the
//! point here, unlike the term-generation phase).

use serde_json::{json, Value};

use crate::host::{Host, Outcome};
use crate::params::Params;
use crate::prompts;
use crate::search::{self, Hit};

pub fn summarize(host: &dyn Host, p: &Params, hits: &[Hit]) -> Result<String, Outcome> {
    let system = p
        .summary_prompt
        .clone()
        .unwrap_or_else(|| prompts::DEFAULT_SUMMARY_PROMPT.to_string());

    let user = build_user_message(p, hits);

    let payload = json!({
        "model": p.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "think": false,
        // As for every other call in this package. No `format`, though: prose
        // is the point here, unlike the term-generation phase.
        "options": { "temperature": 0 },
    });
    let payload = crate::params::apply_llm_overrides(payload, p);

    host.log(&format!("solx-inquiry: summarizing {} hit(s) via {}", hits.len(), p.llm_action_ref));

    let result = crate::llm::call(host, p, payload, "summary")?;

    let content = result.pointer("/message/content").and_then(Value::as_str).unwrap_or("").trim();
    if content.is_empty() {
        return Err(Outcome::fail(
            "bad_llm_output",
            "the model returned an empty summary",
            json!({ "stage": "summary" }),
        ));
    }

    Ok(content.to_string())
}

fn build_user_message(p: &Params, hits: &[Hit]) -> String {
    if hits.is_empty() {
        return format!(
            "Inquiry: {}\n\nNo matching documents or actions were found. Tell the user plainly that nothing was found, without inventing an answer.",
            p.inquiry
        );
    }

    format!("Inquiry: {}\n\nSearch results:\n{}", p.inquiry, search::context_block(hits))
}
