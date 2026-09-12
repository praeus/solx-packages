//! Parse and default an `instruct` call's params.
//!
//! Separate from [`crate::params`] rather than folded into it: the two actions
//! genuinely disagree about what a field *means*. `inquire` takes one `scope`
//! for the whole call; `instruct` has no call-wide scope at all, because each
//! inquiry the intent phase proposes picks its own. Merging them would have
//! meant one struct with a scope field that is meaningless for half its
//! callers.
//!
//! The connection-related fields are the exception and are *not* duplicated:
//! `instruct` builds a [`Params`] to carry them, so
//! [`crate::params::apply_llm_overrides`] and [`crate::llm::call`] keep working
//! unchanged for both actions.

use serde_json::Value;

use crate::host::{str_param, Outcome};
use crate::params::{Params, Scope, DEFAULT_LLM_ACTION_REF, MAX_LLM_TIMEOUT};

/// Where this package's own documents live. Skills are seeded by
/// `install.solx` at a fixed location, so `skills_path` has a default worth
/// having; sessions and memories are both addressed by the caller instead.
pub const INQUIRY_ROOT: &str = "/solx-inquiry";
pub const DEFAULT_SKILLS_PATH: &str = "/solx-inquiry/skills";

pub const SKILL_TYPE_REF: &str = "/packages/solx-inquiry/InquirySkill";

/// Stamped as `author` on every document `instruct` writes - the session, and
/// the memory payloads handed back for a caller to save.
///
/// Provenance for whoever reads the document later: it says a model produced
/// this, and which action did. Nothing in this pipeline branches on it. What
/// keeps model output from re-entering as evidence is the *declared* memory
/// path (see [`InstructParams::memory_path`]), not an inference from a field a
/// caller is free to change.
pub const INSTRUCT_AUTHOR: &str = "/packages/solx-inquiry/instruct";
pub const MEMORY_TYPE_REF: &str = "/packages/solx-inquiry/InquiryMemory";
pub const SESSION_TYPE_REF: &str = "/packages/solx-inquiry/InstructSession";

/// Hard ceiling on how many inquiries one `instruct` may fan out.
///
/// Not a tuning knob a caller can raise: each inquiry is a detached llm call
/// whose console output is echoed into this action's own console, and the
/// whole run is bounded by one `timeout_secs` on the action row. Three is what
/// that budget was sized for.
pub const MAX_INQUIRIES_CEILING: usize = 3;
pub const DEFAULT_MAX_INQUIRIES: usize = 3;

pub const MAX_TERMS_CEILING: usize = 10;
pub const DEFAULT_MAX_TERMS: usize = 5;
pub const MAX_RESULTS_CEILING: usize = 50;
pub const DEFAULT_MAX_RESULTS: usize = 10;

pub const MAX_RECALL_LIMIT: usize = 20;
pub const DEFAULT_RECALL_LIMIT: usize = 5;
pub const MAX_HISTORY_LIMIT: usize = 20;
pub const DEFAULT_HISTORY_LIMIT: usize = 6;

/// How many skill documents to fetch as candidates. A candidate fetch, not a
/// filter - `recall` passes no `q` (see [`crate::recall`]), so this only has
/// to exceed the number of skills that live under the skills path.
pub const SKILL_SEARCH_LIMIT: usize = 40;

/// Prompt budget, and the reason each seeded skill is kept short. A 4B model
/// drowns long before it runs out of context, so the total is what decides how
/// many skills can ride along in one inquiry.
pub const SKILL_INSTRUCTIONS_CAP: usize = 4000;
pub const SKILL_TOTAL_CAP: usize = 8000;

/// Cap on one memory's text, in the payload and in the `summary` that carries
/// it (which is what makes recall a single search with no follow-up reads).
pub const MEMORY_TEXT_CAP: usize = 1000;

/// Prompt budget on the whole recalled-memories block, mirroring
/// [`SKILL_TOTAL_CAP`] for the same reason: [`MEMORY_TEXT_CAP`] bounds one
/// memory, not `recall_limit` of them together, so raising `recall_limit`
/// toward [`MAX_RECALL_LIMIT`] could otherwise put up to 20,000 characters of
/// memories into a prompt that also carries skills and, for the intent
/// phase, history. Memories are recalled most-recent-first (see
/// [`crate::recall::recall_memories`]), so [`crate::host::join_within_budget`]
/// drops the least-recent of what did not fit — right in line with them
/// being explicitly framed as fallible ("may be stale or wrong").
///
/// Sized above the *default* case (`recall_limit: 5` fits comfortably under
/// this) so it changes nothing at default settings.
pub const MEMORY_BLOCK_CAP: usize = 6000;

/// How many turns a session document retains. Older ones are dropped from the
/// front, so a long-lived session stays a bounded document rather than growing
/// without limit under a path nothing prunes.
pub const SESSION_TURN_CAP: usize = 50;

/// Prompt budget on the whole session-history block in the intent prompt,
/// mirroring [`MEMORY_BLOCK_CAP`]. `history_limit` already bounds how many
/// turns are considered; this bounds their combined size once assembled, so
/// raising it toward [`MAX_HISTORY_LIMIT`] cannot push the intent prompt
/// arbitrarily large on its own. History is turns oldest-first, and is
/// explicitly framed as "context ... not evidence" — the least essential of
/// the three reference blocks — so [`session::history_block`] drops the
/// *oldest* surviving turns first when even `history_limit` of them do not
/// fit, which is why it reverses before calling
/// [`crate::host::take_within_budget`] and reverses the kept prefix back.
///
/// Sized above the *default* case (`history_limit: 6` fits comfortably under
/// this) so it changes nothing at default settings.
///
/// [`session::history_block`]: crate::session::history_block
pub const HISTORY_BLOCK_CAP: usize = 4000;

#[derive(Debug)]
pub struct InstructParams {
    pub instruction: String,
    pub model: String,
    /// Full `/path/name` reference of the session document. Created on first
    /// use; read for history, rewritten with this turn appended.
    pub session: String,
    pub max_inquiries: usize,
    pub max_terms: usize,
    pub max_results: usize,
    /// Restricts document inquiries only. Kept separate from
    /// [`Self::action_path_prefix`] because an `instruct` call fans out
    /// inquiries of mixed scope: an instruction that should search all
    /// documents but only a subset of actions (or vice versa) has no single
    /// prefix that expresses both.
    pub document_path_prefix: Option<String>,
    /// Restricts action inquiries only. See [`Self::document_path_prefix`].
    pub action_path_prefix: Option<String>,
    pub type_ref: Option<String>,
    pub skills_path: String,
    /// Where memories are recalled from, and the path stamped on the payloads
    /// returned for saving.
    ///
    /// **`None` turns memories off entirely** - nothing is recalled and no
    /// payload is minted. That is the default because there is no sensible
    /// guess to make: this value is the caller telling the pipeline where its
    /// own past output lives, and getting it wrong means a previous run's
    /// answers are read back as though a person had written them. Off is the
    /// safe reading of "not specified"; inventing a location is not.
    pub memory_path: Option<String>,
    pub recall_limit: usize,
    pub history_limit: usize,
    pub intent_prompt: Option<String>,
    pub document_prompt: Option<String>,
    pub action_prompt: Option<String>,
    /// Carries the connection overrides and `llm_action_ref` so the existing
    /// `params::apply_llm_overrides` / `llm::call` path is reused verbatim.
    /// Its `inquiry`/`scope`/`max_*` fields are not read by `instruct`.
    pub llm: Params,
}

impl InstructParams {
    pub fn llm_action_ref(&self) -> &str {
        &self.llm.llm_action_ref
    }
}

pub fn parse(params: &Value) -> Result<InstructParams, Outcome> {
    let params = &crate::host::normalize_params(params);
    let instruction = str_param(params, "instruction").ok_or_else(|| {
        Outcome::fail(
            "bad_params",
            "instruct requires a non-empty instruction",
            serde_json::json!({ "missing": ["instruction"] }),
        )
    })?;
    let model = str_param(params, "model").ok_or_else(|| {
        Outcome::fail(
            "bad_params",
            "instruct requires a model",
            serde_json::json!({ "missing": ["model"] }),
        )
    })?;
    let session = str_param(params, "session").ok_or_else(|| {
        Outcome::fail(
            "bad_params",
            "instruct requires a session document reference",
            serde_json::json!({ "missing": ["session"] }),
        )
    })?;
    // Checked here rather than at the first use so a malformed reference
    // fails before any llm call is paid for.
    if crate::host::split_ref(&session).is_none() {
        return Err(Outcome::fail(
            "bad_params",
            format!("session must be a full /path/name document reference (got {session:?})"),
            serde_json::json!({ "session": session }),
        ));
    }

    let clamped = |key: &str, default: usize, ceiling: usize| -> usize {
        params
            .get(key)
            .and_then(Value::as_u64)
            .map(|n| (n as usize).clamp(1, ceiling))
            .unwrap_or(default)
    };

    Ok(InstructParams {
        instruction,
        model: model.clone(),
        session,
        max_inquiries: clamped("max_inquiries", DEFAULT_MAX_INQUIRIES, MAX_INQUIRIES_CEILING),
        max_terms: clamped("max_terms", DEFAULT_MAX_TERMS, MAX_TERMS_CEILING),
        max_results: clamped("max_results", DEFAULT_MAX_RESULTS, MAX_RESULTS_CEILING),
        document_path_prefix: str_param(params, "document_path_prefix"),
        action_path_prefix: str_param(params, "action_path_prefix"),
        type_ref: str_param(params, "type_ref"),
        skills_path: str_param(params, "skills_path").unwrap_or_else(|| DEFAULT_SKILLS_PATH.to_string()),
        memory_path: str_param(params, "memory_path"),
        recall_limit: clamped("recall_limit", DEFAULT_RECALL_LIMIT, MAX_RECALL_LIMIT),
        history_limit: clamped("history_limit", DEFAULT_HISTORY_LIMIT, MAX_HISTORY_LIMIT),
        intent_prompt: str_param(params, "intent_prompt"),
        document_prompt: str_param(params, "document_prompt"),
        action_prompt: str_param(params, "action_prompt"),
        llm: Params {
            // Placeholders: `instruct` never reads these back. Only the
            // connection fields below are used, via `apply_llm_overrides`.
            inquiry: String::new(),
            model,
            scope: Scope::Documents,
            max_terms: DEFAULT_MAX_TERMS,
            max_results: DEFAULT_MAX_RESULTS,
            path_prefix: None,
            type_ref: None,
            inquiry_prompt: None,
            summary_prompt: None,
            llm_action_ref: str_param(params, "llm_action_ref")
                .unwrap_or_else(|| DEFAULT_LLM_ACTION_REF.to_string()),
            base_url: str_param(params, "base_url"),
            api_key: str_param(params, "api_key"),
            auth_secret_name: str_param(params, "auth_secret_name"),
            headers: params.get("headers").filter(|v| v.is_object()).cloned(),
            timeout_secs: params
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .map(|n| n.clamp(1, MAX_LLM_TIMEOUT)),
            options: params.get("options").filter(|v| v.is_object()).cloned(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Value {
        json!({ "instruction": "do a thing", "model": "m", "session": "/s/one" })
    }

    #[test]
    fn requires_instruction_model_and_session() {
        for missing in ["instruction", "model", "session"] {
            let mut p = base();
            p.as_object_mut().unwrap().remove(missing);
            let err = parse(&p).unwrap_err();
            assert_eq!(err.output["missing"], json!([missing]));
        }
    }

    #[test]
    fn a_session_without_a_path_and_name_is_rejected_before_any_llm_call() {
        let mut p = base();
        p["session"] = json!("no-slash");
        let err = parse(&p).unwrap_err();
        assert_eq!(err.output["kind"], json!("bad_params"));
    }

    #[test]
    fn accepts_camel_case_spellings_of_its_own_snake_case_fields() {
        let mut p = base();
        p["documentPathPrefix"] = json!("/notes");
        p["actionPathPrefix"] = json!("/packages/solx-google");
        p["typeRef"] = json!("/types/core/Object");
        p["memoryPath"] = json!("/notes/memories");
        p["maxInquiries"] = json!(2);
        let parsed = parse(&p).unwrap();
        assert_eq!(parsed.document_path_prefix.as_deref(), Some("/notes"));
        assert_eq!(parsed.action_path_prefix.as_deref(), Some("/packages/solx-google"));
        assert_eq!(parsed.type_ref.as_deref(), Some("/types/core/Object"));
        assert_eq!(parsed.memory_path.as_deref(), Some("/notes/memories"));
        assert_eq!(parsed.max_inquiries, 2);
    }

    #[test]
    fn max_inquiries_cannot_be_raised_past_the_ceiling() {
        let mut p = base();
        p["max_inquiries"] = json!(50);
        assert_eq!(parse(&p).unwrap().max_inquiries, MAX_INQUIRIES_CEILING);
    }

    #[test]
    fn document_and_action_path_prefixes_are_independent() {
        let mut p = base();
        p["document_path_prefix"] = json!("/notes");
        let parsed = parse(&p).unwrap();
        assert_eq!(parsed.document_path_prefix.as_deref(), Some("/notes"));
        assert_eq!(parsed.action_path_prefix, None);

        let mut p = base();
        p["action_path_prefix"] = json!("/packages/solx-google");
        let parsed = parse(&p).unwrap();
        assert_eq!(parsed.document_path_prefix, None);
        assert_eq!(parsed.action_path_prefix.as_deref(), Some("/packages/solx-google"));
    }

    #[test]
    fn skills_have_a_real_default_but_memories_are_off_unless_asked_for() {
        // Skills are seeded by install.solx at a known location, so the
        // default is an answer rather than a guess.
        let p = parse(&base()).unwrap();
        assert_eq!(p.skills_path, DEFAULT_SKILLS_PATH);
        assert!(p.skills_path.starts_with(INQUIRY_ROOT));
        // Memories have no such location, so absent means off, not "somewhere".
        assert_eq!(p.memory_path, None);

        let mut with = base();
        with["memory_path"] = json!("/notes/memories");
        assert_eq!(parse(&with).unwrap().memory_path.as_deref(), Some("/notes/memories"));
    }

    #[test]
    fn connection_overrides_land_on_the_reused_params() {
        let mut p = base();
        p["base_url"] = json!("http://box:9999");
        p["timeout_secs"] = json!(30);
        p["options"] = json!({ "num_ctx": 8192 });
        let parsed = parse(&p).unwrap();
        assert_eq!(parsed.llm.base_url.as_deref(), Some("http://box:9999"));
        assert_eq!(parsed.llm.timeout_secs, Some(30));
        assert_eq!(parsed.llm.options, Some(json!({ "num_ctx": 8192 })));
        assert_eq!(parsed.llm_action_ref(), DEFAULT_LLM_ACTION_REF);
    }
}
