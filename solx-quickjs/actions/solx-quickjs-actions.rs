// NOTE: retired for solx-core. This wrapped `build-javascript-action` as a
// WASM guest that shelled out via `action_exec::exec("command", ...)` —
// old sol special-cased an action-exec call named "command" as a raw
// passthrough shell invocation. solx-core's `action-exec` has no such
// passthrough: it always resolves a real registered action by full
// `/path/name` reference (see `solx-actions/src/wasm_host.rs`), so this
// pattern has no host-side equivalent to target. `install.solx` in this
// package registers `build-javascript-action` directly as a plain `Command`
// action instead (wrapping the `solx-quickjs` CLI binary), which needs no
// WASM wrapper at all. Kept here for reference only — not built or wired
// into install.solx.

use serde_json::{json, Value};

wit_bindgen::generate!({ world: "custom-action", path: "../wit" });

use exports::sol::actions::runner::{ActionResult, Guest};
use sol::actions::{action_exec, logger};

struct SolQuickjsActions;

impl Guest for SolQuickjsActions {
    fn run(action_name: Option<String>, params: String) -> Result<ActionResult, String> {
        match action_name.as_deref() {
            Some("build-javascript-action") => build_javascript_action(&params),
            _ => Err("unknown action".to_string()),
        }
    }
}

export!(SolQuickjsActions);

fn build_javascript_action(params: &str) -> Result<ActionResult, String> {
    let input: Value = serde_json::from_str(params).map_err(|e| format!("invalid JSON params: {e}"))?;
    let action_name = input
        .get("action_name")
        .and_then(Value::as_str)
        .ok_or("missing required param: action_name")?;
    let entry_artifact_name = input
        .get("entry_artifact_name")
        .and_then(Value::as_str)
        .ok_or("missing required param: entry_artifact_name")?;
    let source_artifact_names = input
        .get("source_artifact_names")
        .and_then(Value::as_array)
        .ok_or("missing required param: source_artifact_names")?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();

    logger::log(&format!("[sol-quickjs] building {action_name} from {} source(s)", source_artifact_names.len()));

    let cmd = format!(
        "solx-quickjs --action_name {action_name} --entry_artifact_name {entry_artifact_name} --source_artifact_names {}",
        source_artifact_names.join(",")
    );

    let payload = json!({ "command": cmd, "cwd": null });
    let result_json = action_exec::exec("command", &payload.to_string())?;
    let result: Value = serde_json::from_str(&result_json).map_err(|e| format!("invalid build result: {e}"))?;

    Ok(ActionResult {
        success: result["success"].as_bool().unwrap_or(false),
        message: result["message"].as_str().map(str::to_string),
        output: Some(result.to_string()),
    })
}
