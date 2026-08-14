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

## Install

`.solx` scripts have no comment syntax — the whole file is split on `;` — so
`install.solx` itself carries no inline commentary; here is what it does and
what you need to do first.

1. **Build the CLI first** (see above). `install.solx` registers
   `build-javascript-action` as a `Command` action whose `fn_name` is
   `.\solx-quickjs.exe`, resolved against `action_config.cwd` — the binary
   has to already exist at that path when the action is later invoked.
2. **Edit `action_config.cwd`** in `install.solx` before installing: replace
   `REPLACE_WITH_ABSOLUTE_PATH_TO/solx-quickjs/target/release` with the
   absolute path to this package's `target/release` directory. `.solx` has no
   path templating, so this is a manual edit. (Alternatively, install first
   and `solx save action` the same reference again afterward to correct it.)
3. Run `solx install-package .` from this directory.

Registering a `Command` action has no allowlist gate: `fn_name` is the
literal command solx-core will run the moment `install-package` posts the
action row, with no confirmation step.

Params (`action_name`, `entry_artifact_name`, `source_artifact_names`,
`output_artifact_name`, `artifact_root`) are passed to the CLI as JSON on
stdin, per solx-core's `Command` action contract; the CLI falls back to
parsing them as flags only when stdin is a terminal (direct manual
invocation).

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
2. `solx save file files/actions/shared/<name>.wasm --file <path to the built .wasm>`
3. `solx save action /path/name --json '{"action_type":"wasm","bin_name":"<name>.wasm"}'`
4. `solx exec /path/name --json '{...}'`

See [actions/sample-workflow.solx](actions/sample-workflow.solx) for a
runnable reference — replace `REPLACE_WITH_ABSOLUTE_PATH_TO_THIS_DIR` with
the absolute path to this `actions/` directory before running it with
`solx script -f sample-workflow.solx`. `actions/solx-quickjs-actions.rs` (a
WASM wrapper around the CLI) is retired — see the comment at the top of that
file for why it doesn't map onto solx-core's `action-exec` semantics.
