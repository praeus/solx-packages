//! Phase 1: ask the configured LLM action to turn the inquiry into search
//! terms.
//!
//! Structured output (`format` as a JSON schema) is requested so a compliant
//! model answers with exactly `{"terms": [...]}`. [`parse_terms`] still falls
//! back to looser parsing for a model that ignores `format` (older Ollama
//! models predate schema-constrained decoding) or wraps the JSON in prose or
//! a markdown fence — the alternative is a pipeline that only works with a
//! handful of models, which would defeat the point of taking `model` as a
//! caller-supplied param.

use serde_json::{json, Value};

use crate::host::{Host, Outcome};
use crate::params::Params;
use crate::prompts;

pub fn generate_search_terms(host: &dyn Host, p: &Params) -> Result<Vec<String>, Outcome> {
    let system = p
        .inquiry_prompt
        .clone()
        .unwrap_or_else(|| prompts::DEFAULT_INQUIRY_PROMPT.to_string());

    let payload = json!({
        "model": p.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": p.inquiry },
        ],
        "format": prompts::terms_schema(p.max_terms),
        "think": false,
        "options": { "temperature": 0 },
    });
    let payload = crate::params::apply_llm_overrides(payload, p);

    host.log(&format!("solx-inquiry: generating search terms via {}", p.llm_action_ref));

    let result = crate::llm::call(host, p, payload, "terms")?;

    let content = result.pointer("/message/content").and_then(Value::as_str).unwrap_or("");

    parse_terms(content, p.max_terms).ok_or_else(|| {
        Outcome::fail(
            "bad_llm_output",
            "could not extract search terms from the model's response",
            json!({ "stage": "terms", "raw": content }),
        )
    })
}

/// Words too common to narrow an FTS5 query, and short enough that a prefix
/// match on them would sweep in almost everything. Kept deliberately small:
/// this is a fallback for a model that ignored a `required` schema field, not
/// a linguistics exercise.
const STOPWORDS: &[&str] = &[
    "a", "about", "all", "an", "and", "any", "are", "as", "at", "be", "been", "but", "by", "can",
    "did", "do", "does", "for", "from", "get", "had", "has", "have", "here", "how", "i", "if",
    "in", "into", "is", "it", "its", "me", "my", "no", "not", "of", "on", "or", "our", "out",
    "should", "so", "some", "than", "that", "the", "their", "them", "then", "there", "these",
    "they", "this", "to", "up", "was", "we", "were", "what", "when", "where", "which", "who",
    "why", "will", "with", "would", "you", "your",
];

/// Derive search terms from a question **without** an llm call.
///
/// `instruct`'s intent schema marks each inquiry's `terms` as `required`, so
/// a compliant model always supplies them. This covers the model that ignores
/// `format` anyway — and it covers it locally, because the alternative (one
/// `generate_search_terms` call per inquiry) would double the call budget and,
/// worse, serialize the fan-out those calls were parallelized to avoid.
///
/// Single words only, for the reason [`DEFAULT_INQUIRY_PROMPT`] spells out:
/// `search_documents`/`search_actions` AND every word within one `q`, so a
/// multi-word term is narrower, not broader.
///
/// [`DEFAULT_INQUIRY_PROMPT`]: crate::prompts::DEFAULT_INQUIRY_PROMPT
pub fn fallback_terms(question: &str, max_terms: usize) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for word in question.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_') {
        let word = word.trim_matches(['-', '_']).to_lowercase();
        if word.len() < 3 || STOPWORDS.contains(&word.as_str()) {
            continue;
        }
        if !terms.contains(&word) {
            terms.push(word);
        }
        if terms.len() >= max_terms {
            break;
        }
    }
    // A question made entirely of stopwords still has to search *something*
    // rather than silently returning zero hits for zero terms.
    if terms.is_empty() {
        if let Some(first) = question.split_whitespace().next() {
            let first = first.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
            if !first.is_empty() {
                terms.push(first);
            }
        }
    }
    terms
}

/// Turn the model's terms into the list actually searched, adding the
/// individual words of any multi-word one.
///
/// Both search built-ins AND every word *within* one `q`, so a multi-word term
/// is narrower than its words, not broader — and a model that answers with a
/// phrase where the prompt asked for keywords can therefore drive an inquiry
/// to **zero** hits. That is not a hypothetical: a live 4B run answered
/// `["list installed actions"]` for "what lists every installed action?", which
/// matched nothing at all and produced no script from an inquiry that had a
/// perfectly good answer available.
///
/// `max_terms` is a budget of *search calls*, not of model output, so what
/// matters is which candidates are worth spending it on. They are added in
/// priority order:
///
/// 1. **The model's own single keywords.** Its clearest signal, and already
///    usable as-is.
/// 2. **The words inside every multi-word term.** These carry the recall: a
///    document matching a whole phrase matches each of its words too, so the
///    words' results are a *superset* of the phrase's.
/// 3. **The phrases themselves, if there is room left.** A phrase adds only
///    ranking, never reach — so it is what gives way under budget pressure,
///    rather than the words that decide whether the inquiry finds anything at
///    all. With hits now scored by rank fusion across terms (see
///    [`crate::search`]), a document all of whose words matched is corroborated
///    by each of them, which recovers most of what the phrase's own #1 ranking
///    would have said.
///
/// Ordering it the other way round is what made this a no-op in the case it
/// exists for: the intent schema caps `terms` at `max_terms`, so a model
/// answering with `max_terms` phrases filled the budget with phrases before a
/// single word could be added, and the inquiry searched only the phrases that
/// match nothing.
///
/// Deliberately *not* done in [`crate::prompts::DEFAULT_INQUIRY_PROMPT`]'s
/// pipeline, where the same advice is given: `inquire` returns its terms to
/// the caller as part of its documented result, so quietly rewriting them
/// there would misreport what the model said.
pub fn expand_terms(terms: &[String], max_terms: usize) -> Vec<String> {
    fn is_phrase(term: &str) -> bool {
        term.split_whitespace().nth(1).is_some()
    }

    let mut out: Vec<String> = Vec::new();
    let push = |candidate: &str, out: &mut Vec<String>| {
        if !candidate.is_empty()
            && out.len() < max_terms
            && !out.iter().any(|t| t.eq_ignore_ascii_case(candidate))
        {
            out.push(candidate.to_string());
        }
    };

    for term in terms.iter().filter(|t| !is_phrase(t)) {
        push(term, &mut out);
    }
    for term in terms.iter().filter(|t| is_phrase(t)) {
        for word in fallback_terms(term, max_terms) {
            push(&word, &mut out);
        }
    }
    for term in terms.iter().filter(|t| is_phrase(t)) {
        push(term, &mut out);
    }

    out
}

/// Best-effort extraction of a term list from a chat response's text
/// content. Tried in order:
///
/// 1. `{"terms": [...]}` — the schema-constrained shape.
/// 2. `[...]` — a bare JSON array of strings.
/// 3. Either of the above inside a ```-fenced code block.
/// 4. Line/comma-separated text, stripping common list markers.
///
/// `None` only when every strategy yields zero non-empty terms.
fn parse_terms(content: &str, max_terms: usize) -> Option<Vec<String>> {
    let trimmed = content.trim();

    if let Some(terms) = parse_terms_json(trimmed) {
        return Some(cap(terms, max_terms));
    }
    if let Some(fenced) = strip_code_fence(trimmed) {
        if let Some(terms) = parse_terms_json(fenced) {
            return Some(cap(terms, max_terms));
        }
    }

    let terms: Vec<String> = trimmed
        .split(['\n', ','])
        .map(strip_list_marker)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(cap(terms, max_terms))
    }
}

fn parse_terms_json(text: &str) -> Option<Vec<String>> {
    let value: Value = serde_json::from_str(text).ok()?;
    let array = value
        .get("terms")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())?;
    let terms: Vec<String> = array
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms)
    }
}

fn strip_code_fence(text: &str) -> Option<&str> {
    let text = text.strip_prefix("```")?;
    let text = text.strip_prefix("json").unwrap_or(text);
    let (body, _) = text.split_once("```")?;
    Some(body.trim())
}

/// Strip a leading list marker (`-`, `*`, `1.`, `1)`) and surrounding quotes.
fn strip_list_marker(s: &str) -> &str {
    let s = s.trim();
    let s = s
        .trim_start_matches(|c: char| c == '-' || c == '*' || c == '•')
        .trim_start();
    let s = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(i) if i > 0 && matches!(s.as_bytes().get(i), Some(b'.') | Some(b')')) => s[i + 1..].trim_start(),
        _ => s,
    };
    s.trim_matches(|c: char| c == '"' || c == '\'').trim()
}

fn cap(mut terms: Vec<String>, max_terms: usize) -> Vec<String> {
    terms.truncate(max_terms);
    terms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_schema_shape() {
        let terms = parse_terms(r#"{"terms": ["alpha", "beta"]}"#, 5).unwrap();
        assert_eq!(terms, vec!["alpha", "beta"]);
    }

    #[test]
    fn parses_a_bare_array() {
        let terms = parse_terms(r#"["alpha", "beta"]"#, 5).unwrap();
        assert_eq!(terms, vec!["alpha", "beta"]);
    }

    #[test]
    fn parses_a_fenced_code_block() {
        let terms = parse_terms("```json\n{\"terms\": [\"alpha\"]}\n```", 5).unwrap();
        assert_eq!(terms, vec!["alpha"]);
    }

    #[test]
    fn falls_back_to_list_markers() {
        let terms = parse_terms("- alpha\n- beta\n1. gamma", 5).unwrap();
        assert_eq!(terms, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn falls_back_to_comma_separated() {
        let terms = parse_terms("alpha, beta, gamma", 5).unwrap();
        assert_eq!(terms, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn caps_at_max_terms() {
        let terms = parse_terms(r#"{"terms": ["a", "b", "c", "d"]}"#, 2).unwrap();
        assert_eq!(terms, vec!["a", "b"]);
    }

    #[test]
    fn empty_content_yields_none() {
        assert!(parse_terms("", 5).is_none());
        assert!(parse_terms("   ", 5).is_none());
    }

    #[test]
    fn a_multi_word_term_keeps_the_phrase_and_gains_its_words() {
        // The live failure this exists for: one phrasal term matched nothing,
        // because both search built-ins AND every word within one `q`. The
        // words come first because they are what carries the recall.
        assert_eq!(
            expand_terms(&["list installed actions".to_string()], 5),
            vec!["list", "installed", "actions", "list installed actions"]
        );
    }

    #[test]
    fn single_word_terms_are_left_exactly_as_they_are() {
        let terms = vec!["auth".to_string(), "session".to_string()];
        assert_eq!(expand_terms(&terms, 5), terms);
    }

    #[test]
    fn a_full_budget_of_phrases_still_gets_searched_by_word() {
        // The regression this ordering exists for. The intent schema caps
        // `terms` at max_terms, so a model answering entirely in phrases used
        // to fill the budget before a single word could be added - and every
        // one of those phrases matches nothing, so the inquiry found nothing
        // at all.
        let phrases: Vec<String> = [
            "list installed actions",
            "search stored documents",
            "read action catalogue",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let expanded = expand_terms(&phrases, 3);
        assert_eq!(expanded, vec!["list", "installed", "actions"]);
        assert!(
            expanded.iter().all(|t| t.split_whitespace().count() == 1),
            "a phrase must not crowd out the words that can actually match: {expanded:?}"
        );
    }

    #[test]
    fn expansion_respects_the_cap_and_never_duplicates() {
        assert_eq!(
            expand_terms(&["deploy release".to_string(), "deploy".to_string()], 3),
            vec!["deploy", "release", "deploy release"]
        );
        // Words fill the budget first; the phrase only rides along if it fits.
        assert_eq!(
            expand_terms(&["deploy release rollback".to_string()], 2),
            vec!["deploy", "release"]
        );
    }

    #[test]
    fn a_phrase_rides_along_when_there_is_room_for_it() {
        assert_eq!(
            expand_terms(&["deploy release".to_string()], 5),
            vec!["deploy", "release", "deploy release"]
        );
    }

    #[test]
    fn fallback_terms_strips_stopwords_and_short_words() {
        assert_eq!(
            fallback_terms("How do I deploy the authentication service?", 5),
            vec!["deploy", "authentication", "service"]
        );
    }

    #[test]
    fn fallback_terms_deduplicates_and_caps() {
        assert_eq!(fallback_terms("deploy deploy release rollback", 2), vec!["deploy", "release"]);
    }

    #[test]
    fn fallback_terms_never_returns_nothing_for_a_nonempty_question() {
        // Every word is a stopword or too short - searching zero terms would
        // silently produce zero hits, which reads as "nothing is known" rather
        // than "the question was unusable".
        assert_eq!(fallback_terms("what is it?", 5), vec!["what"]);
        assert!(fallback_terms("   ", 5).is_empty());
    }
}
