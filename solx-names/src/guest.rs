//! The wit-bindgen surface — the only file in this crate that touches it, and
//! the only one compiled solely for `wasm32`.
//!
//! Keeping it isolated is what lets `cargo test` link on the host target:
//! `generate!` emits `#[link(wasm_import_module = ...)]` externs and
//! `export!` emits `#[export_name]` shims referencing them, none of which
//! resolve in a host test binary.
//!
//! Note the export shape: componentize-style guests must export the WIT
//! `runner` *interface*, which `export!(Component)` wires up from an
//! `impl Guest for Component`. This mirrors `solx-ollama`/`solx-inquiry`.

wit_bindgen::generate!({
    world: "custom-action",
    path: "wit",
});

use exports::sol::actions::runner::{ActionResult, Guest};

struct Component;

impl Guest for Component {
    fn run(action_name: Option<String>, params: String) -> Result<ActionResult, String> {
        // Always Ok(..): `Err` renders the same to an operator but drops the
        // machine-readable error object, since the host collapses a `None`
        // output to `Value::Null`.
        let outcome = crate::dispatch(action_name.as_deref(), &params);
        Ok(ActionResult {
            success: outcome.success,
            message: outcome.message,
            output: Some(outcome.output.to_string()),
        })
    }
}

export!(Component);
