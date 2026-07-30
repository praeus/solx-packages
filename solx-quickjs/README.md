# solx-quickjs

This package adds a lightweight JavaScript action build flow for solx-core.

## What it provides

- a Rust CLI that compiles a JavaScript source file into a `wasm32-wasip2`
  component (via `componentize-qjs`) against solx-core's `custom-action` WIT
  world
- a `build-javascript-action` Command action (registered by `install.solx`)
  that invokes the CLI through `solx exec`
- a sample `.solx` workflow for build → register → execute

## Build the CLI

From this package directory:

```bash
cargo build --release
```

The resulting binary will be available at `target/release/solx-quickjs`.

## Writing the JS source

The WIT world exports a `runner` interface with a `run` function. Verified
against a real build+exec round trip: `componentize-qjs` binds that interface
to a **named export matching its identifier**, not a bare top-level `run`
function. Export it like this:

```js
export const runner = {
  run(actionName, params) {
    return { success: true, message: null, output: JSON.stringify({ ... }) };
  }
};
```

Exporting `export function run(actionName, params) { ... }` directly
compiles fine but fails at runtime with `interface
'sol:actions/runner@0.1.0' not found: FromJs { from: "undefined", to:
"object" }` — the guest module has no `runner`-named export for
componentize-qjs to bind the interface to, even though a `run` function
exists at the top level.

To call other solx actions from within the guest (recursively, permission-gated), import
`sol:actions/action-exec@0.1.0` and call `exec(actionRef, paramsJson)` with a
full `/path/name` action reference — see
[actions/sample-document-summary.js](actions/sample-document-summary.js).

## Example workflow

1. build the `.wasm` component: `solx exec /packages/solx-quickjs/build-javascript-action --json '{"action_name":"...","entry_artifact_name":"main.js","source_artifact_names":["main.js"],"artifact_root":"..."}'`
2. `solx post file files/actions/shared/<name>.wasm --file <path to the built .wasm>`
3. `solx post action /path/name --json '{"action_type":"wasm","bin_name":"<name>.wasm"}'`
4. `solx exec /path/name --json '{...}'`

See [actions/sample-workflow.solx](actions/sample-workflow.solx) for a
runnable reference. `actions/sol-quickjs-actions.rs` (a WASM wrapper around
the CLI) is retired — see the comment at the top of that file for why it
doesn't map onto solx-core's `action-exec` semantics.
