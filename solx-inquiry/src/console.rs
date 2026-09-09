//! Tagged console output for `instruct`.
//!
//! Every milestone of an `instruct` run is printed to the action's own console
//! with **both** a human-readable tagged message and a machine-readable `data`
//! object, so a caller can reconstruct the run afterwards from
//! `/builtin/console/read` without re-running anything. The point of the tag
//! grammar is that it is parseable: `[instruct:<phase>]` or
//! `[instruct:inquiry:<index>:<step>]`, one vocabulary, defined here and
//! nowhere else.
//!
//! Printing is best-effort throughout. A console hiccup is an observability
//! problem, never a reason to fail a pipeline that is otherwise producing
//! correct results — the same contract `llm::echo_console` already keeps.

use serde_json::{json, Value};

use crate::host::Host;
use crate::llm::CONSOLE_PRINT_REF;

/// Root of every tag this package prints. A caller filtering console entries
/// for an `instruct` run matches on this prefix.
pub const TAG_ROOT: &str = "instruct";

pub const PHASE_RECALL: &str = "recall";
pub const PHASE_INTENT: &str = "intent";
pub const PHASE_RESULT: &str = "result";

pub const STEP_TERMS: &str = "terms";
pub const STEP_HITS: &str = "hits";
pub const STEP_RESULT: &str = "result";
pub const STEP_ERROR: &str = "error";

/// `[instruct:<phase>]`.
pub fn phase_tag(phase: &str) -> String {
    format!("{TAG_ROOT}:{phase}")
}

/// `[instruct:inquiry:<index>]` — also the prefix echoed child console lines
/// carry, which is what makes live model output attributable to one inquiry.
pub fn inquiry_tag(index: usize) -> String {
    format!("{TAG_ROOT}:inquiry:{index}")
}

/// `[instruct:inquiry:<index>:<step>]`.
pub fn inquiry_step_tag(index: usize, step: &str) -> String {
    format!("{}:{}", inquiry_tag(index), step)
}

/// Print one tagged milestone. `data` is the machine-readable half; the
/// message is what an operator tailing the console reads.
pub fn print(host: &dyn Host, tag: &str, message: &str, data: Value) {
    let _ = host.exec(
        CONSOLE_PRINT_REF,
        &json!({ "level": "info", "message": format!("[{tag}] {message}"), "data": data }),
    );
}

/// Same, at `warn` — used for a degraded-but-continuing outcome (a failed
/// inquiry among several that succeeded, a session write that did not land).
pub fn warn(host: &dyn Host, tag: &str, message: &str, data: Value) {
    let _ = host.exec(
        CONSOLE_PRINT_REF,
        &json!({ "level": "warn", "message": format!("[{tag}] {message}"), "data": data }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_parseable_and_stable() {
        assert_eq!(phase_tag(PHASE_INTENT), "instruct:intent");
        assert_eq!(inquiry_tag(0), "instruct:inquiry:0");
        assert_eq!(inquiry_step_tag(2, STEP_HITS), "instruct:inquiry:2:hits");
    }
}
