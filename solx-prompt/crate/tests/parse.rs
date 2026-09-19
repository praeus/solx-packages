//! Tests for the tolerant model-output parser, at the `Value` level.
//!
//! These deliberately do not go through a phase. solx-inquiry's equivalents
//! assert on a parsed `Intent`, which conflates two questions — did the XML
//! parser read the document, and did the phase read the right fields out of it.
//! Splitting them is what makes a failure here point at one of the two.
//!
//! The XML tier exists because real models produce it. Every shape asserted
//! below was chosen to match something a model actually does: uppercase tags, a
//! wrapper element around a list, a repeated tag with no wrapper, and a single
//! occurrence where the schema asked for a list.

use serde_json::{json, Value};

use solx_prompt::parse::{items, object, strip_code_fence};

/// Every caller names the XML roots it will accept; these use the intent
/// phase's.
fn obj(text: &str) -> Option<Value> {
    object(text, &["intent"])
}

// ── tier 1: strict JSON ──────────────────────────────────────────────────────

#[test]
fn strict_json_is_read_as_is() {
    let v = obj(r#"{"message": "hello", "steps": []}"#).unwrap();
    assert_eq!(v["message"], json!("hello"));
    assert_eq!(v["steps"], json!([]));
}

#[test]
fn a_json_value_that_is_not_an_object_is_declined() {
    // The callers all want an object. An array or a scalar is not a partial
    // answer to unwrap, it is content to be read as prose instead.
    for text in ["[1, 2, 3]", "\"just a string\"", "42", "true", "null"] {
        assert!(obj(text).is_none(), "{text} should be declined");
    }
}

#[test]
fn prose_is_declined_rather_than_guessed_at() {
    assert!(obj("I think you should probably search the notes first.").is_none());
    assert!(obj("").is_none());
    assert!(obj("   \n  ").is_none());
}

// ── tier 2: a fenced block ───────────────────────────────────────────────────

#[test]
fn json_wrapped_in_a_markdown_fence_is_unwrapped() {
    let v = obj("```json\n{\"message\": \"hi\"}\n```").unwrap();
    assert_eq!(v["message"], json!("hi"));
}

#[test]
fn a_fence_without_a_language_tag_also_works() {
    let v = obj("```\n{\"message\": \"hi\"}\n```").unwrap();
    assert_eq!(v["message"], json!("hi"));
}

#[test]
fn strip_code_fence_declines_text_that_is_not_fenced() {
    assert!(strip_code_fence("{\"a\": 1}").is_none());
    // An opening fence with no closing one is not a block yet - the model may
    // simply have been cut off mid-answer.
    assert!(strip_code_fence("```json\n{\"a\": 1}").is_none());
}

// ── tier 3: XML ──────────────────────────────────────────────────────────────

#[test]
fn a_flat_xml_document_becomes_a_flat_object() {
    let v = obj("<intent><mode>direct</mode><message>the answer</message></intent>").unwrap();
    assert_eq!(v["mode"], json!("direct"));
    assert_eq!(v["message"], json!("the answer"));
}

#[test]
fn xml_tags_are_matched_case_insensitively_and_whitespace_is_trimmed() {
    // Observed from gemma-family models, which uppercase tags unpredictably.
    let v = obj("<INTENT>\n  <Mode> direct </Mode>\n</INTENT>").unwrap();
    assert_eq!(v["mode"], json!("direct"));
}

#[test]
fn a_repeated_xml_element_becomes_an_array() {
    // The one thing an XML document cannot say that JSON can is that a single
    // occurrence was meant to be a list. Repetition is the only signal, so a
    // repeated tag has to collapse into an array rather than let the last
    // occurrence overwrite the first.
    let v = obj("<intent><step>one</step><step>two</step></intent>").unwrap();
    assert_eq!(v["step"], json!(["one", "two"]));
}

#[test]
fn a_single_occurrence_stays_scalar_rather_than_becoming_a_one_element_array() {
    // Which is exactly why `items` exists: the parser cannot tell a one-item
    // list from a scalar, so the shape layer settles it.
    let v = obj("<intent><step>only</step></intent>").unwrap();
    assert_eq!(v["step"], json!("only"));
}

#[test]
fn nested_elements_become_nested_objects() {
    let v = obj("<intent><steps><step><goal>do it</goal></step></steps></intent>").unwrap();
    assert_eq!(v["steps"]["step"]["goal"], json!("do it"));
}

#[test]
fn an_unrecognised_field_survives_instead_of_being_dropped() {
    // solx-inquiry's parser matched on its own schema and dropped everything
    // else, so a field it did not know about vanished silently. Every field the
    // model wrote has to arrive; deciding which ones matter is the phase's job.
    let v = obj("<intent><message>hi</message><something_new>42</something_new></intent>").unwrap();
    assert_eq!(v["message"], json!("hi"));
    assert_eq!(v["something_new"], json!("42"));
}

#[test]
fn an_xml_root_carrying_only_text_is_declined() {
    // No fields means nothing for a caller to read, so the content is better
    // read as the prose it is.
    assert!(obj("<intent>just talking</intent>").is_none());
}

#[test]
fn xml_whose_root_the_caller_did_not_name_is_declined() {
    // A model answering in prose that happens to contain an angle bracket must
    // not be read as a document.
    assert!(obj("<html><body>nope</body></html>").is_none());
    assert!(obj("use a < b as the condition").is_none());
}

#[test]
fn each_caller_accepts_only_its_own_roots() {
    // The steps phase names a different root, and neither should read the
    // other's document as its own.
    assert!(object("<steps><step>a</step></steps>", &["steps", "plan"]).is_some());
    assert!(object("<steps><step>a</step></steps>", &["intent"]).is_none());
    assert!(object("<intent><message>a</message></intent>", &["steps"]).is_none());
}

#[test]
fn a_mismatched_close_tag_is_declined_rather_than_half_read() {
    // Half a document is worse than none: the fields that did parse would look
    // like a complete answer.
    assert!(obj("<intent><mode>direct</message></intent>").is_none());
}

#[test]
fn the_five_predefined_entities_are_decoded() {
    let v = obj("<intent><message>a &lt; b &amp;&amp; c &gt; d</message></intent>").unwrap();
    assert_eq!(v["message"], json!("a < b && c > d"));
}

// ── tier ordering ────────────────────────────────────────────────────────────

#[test]
fn strict_json_wins_over_the_later_tiers() {
    // Content that is valid JSON *and* contains something fence-shaped has to
    // be read as the JSON it already is.
    let v = obj(r#"{"message": "write it as ```json ...```"}"#).unwrap();
    assert_eq!(v["message"], json!("write it as ```json ...```"));
}

#[test]
fn every_tier_either_produces_an_object_or_declines() {
    // The contract the phases rely on: there is no third outcome to handle.
    for text in [
        r#"{"a": 1}"#,
        "```json\n{\"a\": 1}\n```",
        "<intent><a>1</a></intent>",
        "prose",
        "",
        "[1]",
        "<intent><a>1</b></intent>",
    ] {
        if let Some(v) = obj(text) {
            assert!(matches!(v, Value::Object(_)), "{text} produced a non-object");
        }
    }
}

// ── items(): reading a list out of any of its five shapes ────────────────────

#[test]
fn items_reads_all_five_shapes_the_same_way() {
    let a = json!({ "goal": "a" });
    let b = json!({ "goal": "b" });

    // The schema, honored.
    let v = json!({ "steps": [a, b] });
    assert_eq!(items(&v, "steps", "step").len(), 2);

    // XML, repeated child under a wrapper.
    let v = json!({ "steps": { "step": [a, b] } });
    assert_eq!(items(&v, "steps", "step").len(), 2);

    // XML, a single child under a wrapper.
    let v = json!({ "steps": { "step": a } });
    let got = items(&v, "steps", "step");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["goal"], json!("a"));

    // XML, repeated with no wrapper at all.
    let v = json!({ "step": [a, b] });
    assert_eq!(items(&v, "steps", "step").len(), 2);

    // One item, never wrapped.
    let v = json!({ "steps": a });
    let got = items(&v, "steps", "step");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["goal"], json!("a"));
}

#[test]
fn items_is_empty_rather_than_failing_when_there_is_no_list() {
    // "the model proposed nothing" is a normal answer, not an error.
    assert!(items(&json!({}), "steps", "step").is_empty());
    assert!(items(&json!({ "steps": null }), "steps", "step").is_empty());
    assert!(items(&json!({ "steps": [] }), "steps", "step").is_empty());
}

#[test]
fn items_round_trips_what_the_xml_tier_produces() {
    // The two halves have to agree, which is the whole point of splitting the
    // schema knowledge out of the parser: whatever shape the parser emits for a
    // repeated tag, `items` has to read back as a list.
    for xml in [
        "<intent><steps><step><goal>a</goal></step><step><goal>b</goal></step></steps></intent>",
        "<intent><step><goal>a</goal></step><step><goal>b</goal></step></intent>",
    ] {
        let v = obj(xml).unwrap();
        let got = items(&v, "steps", "step");
        assert_eq!(got.len(), 2, "{xml} produced {got:?}");
        assert_eq!(got[0]["goal"], json!("a"), "{xml}");
        assert_eq!(got[1]["goal"], json!("b"), "{xml}");
    }
}

#[test]
fn items_reads_a_single_xml_step_as_a_one_item_list() {
    let v = obj("<intent><steps><step><goal>a</goal></step></steps></intent>").unwrap();
    let got = items(&v, "steps", "step");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["goal"], json!("a"));
}
