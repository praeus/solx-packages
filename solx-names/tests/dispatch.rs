//! Host-target tests for `dispatch` — everything except the wit-bindgen shim.

use serde_json::{json, Value};

fn kind(outcome: &solx_names::Outcome) -> String {
    outcome
        .output
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("<none>")
        .to_string()
}

fn name_of(outcome: &solx_names::Outcome) -> String {
    outcome
        .output
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no name in {}", outcome.output))
        .to_string()
}

#[test]
fn unknown_and_absent_fn_names() {
    let out = solx_names::dispatch(Some("nope"), "{}");
    assert!(!out.success);
    assert_eq!(kind(&out), "unknown_action");
    assert_eq!(out.output["fn_name"], json!("nope"));
    assert_eq!(out.output["known"], json!(["random_name"]));

    let out = solx_names::dispatch(None, "{}");
    assert!(!out.success);
    assert_eq!(kind(&out), "unknown_action");
}

#[test]
fn malformed_params() {
    let out = solx_names::dispatch(Some("random_name"), "not json");
    assert_eq!(kind(&out), "bad_params");

    let out = solx_names::dispatch(Some("random_name"), "[1,2,3]");
    assert_eq!(kind(&out), "bad_params");
}

#[test]
fn a_bare_null_is_no_arguments_not_an_error() {
    let out = solx_names::dispatch(Some("random_name"), "null");
    assert!(out.success, "{:?}", out.message);
}

#[test]
fn plain_name_is_one_adjective_noun_pair() {
    let out = solx_names::dispatch(Some("random_name"), "{}");
    assert!(out.success, "{:?}", out.message);
    let name = name_of(&out);
    let parts: Vec<&str> = name.split('-').collect();
    assert_eq!(parts.len(), 2, "{name:?}");
    assert!(parts.iter().all(|p| !p.is_empty()), "{name:?}");
}

#[test]
fn with_id_appends_an_eight_hex_digit_suffix() {
    let out = solx_names::dispatch(Some("random_name"), r#"{"with_id":true}"#);
    assert!(out.success, "{:?}", out.message);
    let name = name_of(&out);
    let parts: Vec<&str> = name.split('-').collect();
    assert_eq!(parts.len(), 3, "{name:?}");
    let id = parts[2];
    assert_eq!(id.len(), 8, "{name:?}");
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "{name:?}");
}

#[test]
fn with_id_false_matches_the_plain_default() {
    let out = solx_names::dispatch(Some("random_name"), r#"{"with_id":false}"#);
    assert!(out.success, "{:?}", out.message);
    assert_eq!(name_of(&out).split('-').count(), 2);
}

#[test]
fn repeated_calls_are_not_constant() {
    // Not a strong statistical test, just a guard against an RNG that never
    // actually got wired up (e.g. a stubbed/fixed seed).
    let names: Vec<String> = (0..20)
        .map(|_| name_of(&solx_names::dispatch(Some("random_name"), r#"{"with_id":true}"#)))
        .collect();
    assert!(
        names.windows(2).any(|w| w[0] != w[1]),
        "20 calls produced the same name every time: {names:?}"
    );
}
