//! The session document: reading it for history, and rewriting it with this
//! turn appended.
//!
//! One session is one document, addressed by the caller as a full
//! `/path/name` reference. This is the first thing in `solx-inquiry` that
//! *writes*, and the write is deliberately the last thing an `instruct` does:
//! the results are already assembled by then, so a session that cannot be
//! written costs the caller a history entry, not the answer they asked for.
//!
//! The `InstructSession` type declares `title`, `lastInstruction` and
//! `turnCount`, and pointedly **does not declare `turns`**. `solx-docs`
//! computes a document's full-text content by walking only the fields its type
//! declares, so an undeclared `turns` is stored and returned but never enters
//! the FTS index. That matters because these documents are rewritten on every
//! instruction and would otherwise re-index an entire growing transcript each
//! time — the same reasoning behind `solx-agent`'s undeclared `messages`.
//! `title` and `summary` *are* indexed, which is what lets a session be found
//! later by what it was about.

use serde_json::{json, Value};

use crate::host::{split_ref, take_within_budget, truncate, Host};
use crate::instruct_params::{
    InstructParams, HISTORY_BLOCK_CAP, INSTRUCT_AUTHOR, SESSION_TURN_CAP, SESSION_TYPE_REF,
};

pub const DOCUMENT_GET_REF: &str = "/builtin/document/entity_get_document";
pub const DOCUMENT_SAVE_REF: &str = "/builtin/document/entity_save_document";

const TITLE_CAP: usize = 80;
const SUMMARY_CAP: usize = 500;
/// How much of a past turn's response survives into the next intent prompt.
/// History is orientation, not evidence — the inquiry phase re-searches for
/// anything that actually has to be relied on.
const HISTORY_TEXT_CAP: usize = 240;

#[derive(Debug, Clone, Default)]
pub struct Session {
    pub title: Option<String>,
    /// Prior turns, oldest first, exactly as they were stored.
    pub turns: Vec<Value>,
}

/// Read the session. A document that is not there yet is an empty session, not
/// an error: a caller naming a fresh session ref is the normal way to start
/// one, and failing would make every first instruction a two-step affair.
///
/// A *failed* read is also treated as empty rather than fatal, and says so in
/// the log — losing history is a worse outcome to cause than to tolerate.
pub fn load(host: &dyn Host, p: &InstructParams) -> Session {
    let Some((path, name)) = split_ref(&p.session) else {
        return Session::default();
    };
    let call = match host.exec(DOCUMENT_GET_REF, &json!({ "path": path, "name": name })) {
        Ok(c) if c.success => c.result,
        Ok(_) => return Session::default(),
        Err(e) => {
            // A session that does not exist yet is the normal way to start
            // one, and the host reports it as an `Err` rather than a
            // `success: false`. Logging that as a failure makes every first
            // run look broken, so only a *different* error is worth a line.
            if !is_not_found(&e) {
                host.log(&format!("solx-inquiry: could not read session {}: {e}", p.session));
            }
            return Session::default();
        }
    };

    Session {
        title: call.get("title").and_then(Value::as_str).map(str::to_string),
        turns: call
            .pointer("/contents/turns")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    }
}

/// `solx-surface`'s `SolxError::NotFound` renders as `not found: <what>`, and
/// the WIT boundary flattens it to that string with no code to switch on -
/// the same constraint `llm.rs` works around for `action_start`.
fn is_not_found(message: &str) -> bool {
    message.contains("not found")
}

/// The history block for the intent prompt: one line per prior turn, oldest
/// first, capped at `history_limit` turns and [`HISTORY_TEXT_CAP`] characters
/// each, and at [`HISTORY_BLOCK_CAP`] characters combined. `None` for a fresh
/// session, so a first instruction carries no empty heading.
///
/// The combined cap drops the *oldest* surviving turns first when even
/// `history_limit` of them do not fit — the reverse of
/// [`recall::memory_block`]'s direction, because these `lines` are built
/// oldest-first while memories are recalled newest-first. So this reverses to
/// newest-first before calling [`take_within_budget`] (making the newest turn
/// the unconditional survivor and the oldest what gets dropped), then
/// reverses the kept prefix back to oldest-first for display. History is
/// explicitly framed below as orientation, not evidence — the first thing to
/// give way.
///
/// [`recall::memory_block`]: crate::recall::memory_block
pub fn history_block(session: &Session, history_limit: usize) -> Option<String> {
    if session.turns.is_empty() {
        return None;
    }
    let start = session.turns.len().saturating_sub(history_limit);
    let lines: Vec<String> = session.turns[start..]
        .iter()
        .filter_map(|turn| {
            let instruction = turn.get("instruction").and_then(Value::as_str)?;
            let answer = turn
                .pointer("/responses/0/text")
                .and_then(Value::as_str)
                .unwrap_or("(no response)");
            Some(format!(
                "- asked: {}\n  answered: {}",
                truncate(instruction, HISTORY_TEXT_CAP),
                truncate(answer, HISTORY_TEXT_CAP)
            ))
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    let mut newest_first = lines;
    newest_first.reverse();
    let mut kept: Vec<String> =
        take_within_budget(&newest_first, HISTORY_BLOCK_CAP).into_iter().map(str::to_string).collect();
    kept.reverse();
    Some(format!(
        "Earlier in this session. Context for what the user is asking now, not \
         an answer to it and not evidence for one.\n\n{}",
        kept.join("\n")
    ))
}

/// Append this turn and write the document back.
///
/// `Err` carries a warning string rather than an `Outcome`: the caller records
/// it in `warnings[]` and still returns the results it already has. A session
/// write that did not land is worth telling the caller about; it is not worth
/// discarding a completed instruction over.
pub fn save(host: &dyn Host, p: &InstructParams, session: &Session, turn: Value) -> Result<(), String> {
    let Some((path, name)) = split_ref(&p.session) else {
        return Err(format!("session {} is not a /path/name reference", p.session));
    };

    let mut turns = session.turns.clone();
    turns.push(turn.clone());
    // Oldest first out, so a long-lived session stays a bounded document
    // rather than growing forever under a path nothing prunes.
    if turns.len() > SESSION_TURN_CAP {
        turns.drain(..turns.len() - SESSION_TURN_CAP);
    }

    let title = session
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| truncate(&p.instruction, TITLE_CAP));
    let summary = turn
        .pointer("/responses/0/text")
        .and_then(Value::as_str)
        .map(|t| truncate(t, SUMMARY_CAP))
        .unwrap_or_else(|| truncate(&p.instruction, SUMMARY_CAP));

    let payload = json!({
        "path": path,
        "name": name,
        "typeRef": SESSION_TYPE_REF,
        // Provenance, not a control: this document is a record of model
        // output, and should say so to anything that reads it later.
        "author": INSTRUCT_AUTHOR,
        "title": title,
        "summary": summary,
        "contents": {
            "turns": turns,
            "turnCount": turns.len(),
            "lastInstruction": p.instruction,
        },
    });

    match host.exec(DOCUMENT_SAVE_REF, &payload) {
        Ok(c) if c.success => Ok(()),
        Ok(c) => Err(format!(
            "could not write session {}: {}",
            p.session,
            c.message.unwrap_or_else(|| "no message".to_string())
        )),
        Err(e) => Err(format!("could not write session {}: {e}", p.session)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(instruction: &str, answer: &str) -> Value {
        json!({ "instruction": instruction, "responses": [{ "text": answer }] })
    }

    #[test]
    fn a_missing_session_is_recognized_rather_than_reported_as_a_failure() {
        assert!(is_not_found("execution error: not found: document /solx-inquiry/sessions/smoke"));
        assert!(!is_not_found("db is locked"));
    }

    #[test]
    fn history_is_none_for_a_fresh_session() {
        assert!(history_block(&Session::default(), 6).is_none());
    }

    #[test]
    fn history_keeps_the_most_recent_turns_oldest_first() {
        let session = Session {
            title: None,
            turns: vec![turn("first", "a"), turn("second", "b"), turn("third", "c")],
        };
        let block = history_block(&session, 2).unwrap();
        assert!(!block.contains("first"), "{block}");
        let second = block.find("second").unwrap();
        let third = block.find("third").unwrap();
        assert!(second < third, "oldest of the kept turns must come first: {block}");
    }

    #[test]
    fn history_drops_the_oldest_surviving_turns_once_the_budget_runs_out() {
        // history_limit already bounds how many turns are considered; this
        // bounds their combined size once assembled. Each turn's instruction
        // and answer sit right at HISTORY_TEXT_CAP, so history_limit alone
        // would keep all 15 turns here - HISTORY_BLOCK_CAP is what has to
        // trim it further, and it must drop from the oldest end, keeping
        // oldest-first order in whatever survives.
        let turns: Vec<Value> = (0..15)
            .map(|i| turn(&format!("q{i:02} {}", "x".repeat(HISTORY_TEXT_CAP)), &"a".repeat(HISTORY_TEXT_CAP)))
            .collect();
        let session = Session { title: None, turns };
        let block = history_block(&session, 15).unwrap();
        assert!(block.len() <= HISTORY_BLOCK_CAP, "block was {} chars", block.len());
        assert!(block.contains("q14 "), "the newest turn must survive: {block}");
        assert!(!block.contains("q00 "), "the oldest turn should have been dropped: {block}");
        // Whatever survived stays in oldest-first order.
        let has = |i: i32| block.find(&format!("q{i:02} ", i = i));
        let survivors: Vec<i32> = (0..15).filter(|&i| has(i).is_some()).collect();
        for pair in survivors.windows(2) {
            assert!(
                has(pair[0]) < has(pair[1]),
                "q{:02} must appear before q{:02}: {block}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn history_keeps_the_newest_turn_even_alone_over_budget() {
        let session = Session {
            title: None,
            turns: vec![turn(&"x".repeat(HISTORY_BLOCK_CAP * 2), "a")],
        };
        let block = history_block(&session, 1).unwrap();
        assert!(block.contains(&"x".repeat(HISTORY_TEXT_CAP)), "{block}");
    }
}
