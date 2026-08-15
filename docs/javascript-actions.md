# JavaScript Action Integration Design

> Status: **design proposal** (not yet implemented)
> Owner: `sol-manager` (compiler + host), `sol-browser` (UI), `sol-cli` (commands)
> Audience: contributors evaluating the scripting runtime, action authors writing JS actions

This document describes the design for integrating JavaScript as a
scripting language for Sol custom actions, using `componentize-qjs` to
compile JS source into WebAssembly components that run in the existing
wasmtime host.

---

## 1. Motivation

Sol needs a scripting runtime that the LLM can generate at runtime and
execute immediately. The requirements are:

1. **LLM-friendly** — the model must be able to generate correct scripts
   without syntax errors or unfamiliar stdlib quirks. JavaScript is the
   most widely-generated language across all LLMs.
2. **Wasmtime-compatible** — the compiled output must run in the existing
   `wasm_host::exec` path with no host-side changes beyond the compiler.
3. **WIT-native** — the scripting language must import/export WIT
   interfaces so actions can call `action_exec::exec("get document", …)`
   and other built-in actions.
4. **Fast compilation** — the LLM generates a script, the host compiles
   it, and the result executes. The round-trip must be under a few
   seconds for interactive use.
5. **Small binary** — the compiled component should be small enough to
   store in the DB and cache in memory.

Rust WASM actions (the current approach for `sol-google-actions`) are
reliable but require a Rust toolchain and 30-60s compile times —
unsuitable for LLM-generated scripts. Python via `componentize-py` is
broken due to wasmtime/componentize-py version drift (see
`python-wasm-integration.md` history). JavaScript via `componentize-qjs`
satisfies all five requirements.

---

## 2. `componentize-qjs` overview

[`componentize-qjs`](https://github.com/andreiltd/componentize-qjs) is a
Rust crate that converts JavaScript source + a WIT file into a
WebAssembly component.

### 2.1 Library API

```rust
use componentize_qjs::{ComponentizeOpts, Runtime, componentize};

let opts = ComponentizeOpts {
    wit_path: &wit_path,         // Path to .wit file or directory
    js_source: &js_source,       // JavaScript source code (ES module)
    js_path: Some(&js_path),      // Base path for resolving imports
    module_root: None,            // Read-only root for bare imports
    world_name: Some("custom-action"),
    stub_wasi: true,              // Trap WASI imports (we don't need them)
    disable_gc: false,
    runtime: Runtime::OptSize,    // Smaller runtime
};

let wasm_bytes: Vec<u8> = componentize(&opts).await?;
```

The `componentize()` function is `async` and returns the raw WASM
component bytes. Under the hood it:

1. Parses the WIT file with `wit-parser`
2. Generates a `wit-dylib` shim that bridges the component model and QuickJS
3. Links the QuickJS runtime + wit-dylib + WASI adapter into a component
4. Runs **Wizer** to snapshot the initialized JS state (startup cost paid
   at compile time, not runtime)
5. Stubs WASI imports (if `stub_wasi: true`) so the component is self-contained
6. Returns the final component bytes

### 2.2 Build-time dependencies

`componentize-qjs`'s `build.rs` downloads:
- **wasi-sdk** (C toolchain for `wasm32-wasip2`) — used to compile the QuickJS runtime
- **binaryen** (`wasm-opt`) — used to optimize the runtime Wasm

These are downloaded once and cached in `OUT_DIR`. They are **build-time**
dependencies only — the runtime host (`sol-manager`) does not need them.
The `componentize-qjs` crate embeds the pre-built QuickJS runtime as
static bytes, so `componentize()` at runtime only needs the WIT file and
JS source.

### 2.3 WIT type mappings

| WIT type | JavaScript |
|---|---|
| `string` | `string` |
| `bool`, `u8`..`u32`, `s8`..`s32` | `number` |
| `u64`, `s64` | `number` (precision limited to 2⁵³) |
| `f32`, `f64` | `number` |
| `list<T>` | `Array` / `Uint8Array` for `list<u8>` |
| `option<T>` | `T` or `null` |
| `result<T, E>` | top-level: return `T` or `throw E`; nested: `{ tag: "ok"\|"err", val }` |
| `record { ... }` | `object` (camelCase keys) |
| `variant` | `{ tag: string, val?: T }` |
| `enum` | `string` (case name) |
| `flags` | `object` (camelCase booleans) |
| `resource` | JS class with methods |

### 2.4 ES module imports

JavaScript sources are ES modules. WIT imports are available as ES module
imports using their fully-qualified WIT interface name:

```js
import { exec } from "sol:actions/action-exec@0.1.0";

export function run(actionName, params) {
    const result = exec("get document", JSON.stringify({ name: "my-doc" }));
    // result is { success, message, output }
    return { success: true, message: null, output: result.output };
}
```

Relative imports (`import { helper } from "./utils.js"`) are resolved
from the entry file path. Bare imports (`import { lib } from "my-lib"`)
are resolved under `module_root`.

---

## 3. Integration architecture

### 3.1 The `sol-quickjs` package

The compiler lives in a **separate sol package**, `sol-packages/sol-quickjs/`,
not in `sol-manager`. This keeps the WASI SDK + binaryen build-time
dependencies (required by `componentize-qjs`) out of the main sol
build.

```
sol-packages/sol-quickjs/
├── Cargo.toml              # Rust binary: sol-quickjs-build
├── src/
│   └── main.rs             # CLI: compiles JS → WASM via componentize-qjs
├── install.solx            # Registers the build & run actions
├── package.json
├── README.md
└── actions/
    └── sol-quickjs-actions.rs   # Rust WASM: build & run actions (custom-action)
```

The `sol-quickjs` package contains:

1. **A Rust command-line tool** `sol-quickjs-build` that embeds
   `componentize-qjs` and takes an action name, a list of `.js`
   artifact names, and an entry artifact name. It loads the
   artifacts from the DB, compiles them via `componentize()`, and
   writes the resulting `.wasm` as a sibling artifact.

2. **A Rust WASM action** `sol-quickjs-actions` (targeting the
   `custom-action` world) that exposes:
   - `build-javascript-action` — invokes `sol-quickjs-build` to
     compile a set of `.js` artifacts into a `.wasm`
   - `run-javascript-action` — loads a pre-compiled `.wasm` artifact
     and executes it via the existing `wasm_host::exec` path

### 3.2 Why a separate package?

| Aspect | In `sol-manager` (rejected) | In `sol-quickjs` (chosen) |
|---|---|---|
| WASI SDK / binaryen build deps | Adds 1-2GB to `sol-manager` build | Scoped to `sol-quickjs` build |
| Compilation time for `sol-manager` | +1-2min for first build | Unchanged |
| Binary size of `sol-browser` | +5-10MB (componentize-qjs + deps) | Unchanged |
| JS support is opt-in | No (always compiled) | Yes (install `sol-quickjs` package) |
| Upgrading componentize-qjs | Rebuilds sol-manager | Rebuilds only sol-quickjs |
| Source of `sol-manager` | More dependencies | Same as before (Rust WASM only) |

The `sol-quickjs` package follows the same pattern as `sol-google`,
`sol-extractous`, and other sol packages: it's a self-contained
crate that provides its own build tool and action implementations.

### 3.3 How `sol-quickjs` is installed

```sh
# from sol-browser/
bunx solx install-package ../sol-packages/sol-quickjs
```

This:

1. Builds `sol-quickjs-actions.wasm` (the Rust WASM with build/run
   actions) and uploads it as a shared artifact
2. Builds `sol-quickjs-build` (the host-side compiler CLI) and
   installs it on `PATH` (or into the sol appdata directory)
3. Registers the `build-javascript-action` and `run-javascript-action`
   entities
4. Defines the `sol-quickjs` types (build params, run params)

### 3.4 WIT world: `custom-action`

JS actions target the `custom-action` WIT world — the same world used by
Rust custom actions. This world imports:

- `action-exec` — `exec(action_name, payload) -> action-result`
- `artifact-read` — `read(name) -> list<u8>`
- `logger` — `log(message: string)`

There is **no** `artifact-eval` interface. It has been removed from the
WIT entirely. Multi-file actions are handled entirely at build time
(see §5): when `.js` artifacts are compiled, `componentize-qjs`
resolves `import` statements during the Wizer snapshot and bakes
utility modules into the compiled WASM.

### 3.5 Dispatcher integration

The dispatcher does **not** gain a new `action_type` branch. A
JavaScript action is just a regular WASM action whose `.wasm`
artifact was produced by `sol-quickjs-build`. The dispatcher checks
`action.action_type`:

- `"Command"` → existing `dispatch_command` (runs a shell command)
- `"WASM"` / `"Rust"` → existing `dispatch_wasm` (loads a `.wasm` artifact
  and executes it via `wasm_host::exec`)

JS actions register as `"WASM"` with `bin_name` pointing at the
compiled `.wasm` artifact. The dispatcher doesn't need to know it
was originally JavaScript — it just loads and runs the `.wasm` like
any other custom action.

```
LLM generates main.js
    │
    ▼
solx exec build-javascript-action '{"action": "my-action", "entry": "main.js", "sources": ["main.js", "utils.js"]}'
    │  (build-javascript-action runs sol-quickjs-build via Command)
    │  (sol-quickjs-build loads .js artifacts, compiles via componentize-qjs, writes .wasm artifact)
    ▼
solx set action my-action --action-type WASM --bin-name actions::my-action::main.js.wasm
    ▼
solx exec my-action '{"input": "..."}'
    │  (dispatcher sees action_type=WASM, loads main.js.wasm, executes)
    │  (no JS-specific code in the dispatcher)
```

---

## 4. Explicit build step (via `sol-quickjs`)

### 4.1 The core decision

> When should the host compile JS → WASM — at upload, at first call,
> or on an explicit build invocation?

### 4.2 Recommendation: **explicit build, invoked manually**

The build is **not** triggered automatically on artifact upload. It
runs only when the user (or LLM) explicitly invokes
`build-javascript-action` (provided by the `sol-quickjs` package).
This is the cleanest design because:

1. **Batch uploads work** — if the user uploads 5 `.js` artifacts in
   sequence (e.g. `main.js`, `utils.js`, `helpers/text.js`,
   `helpers/parse.js`, `helpers/fmt.js`), they don't trigger 5
   separate builds. The user uploads all sources, then invokes
   `build-javascript-action` once.
2. **No accidental builds** — uploading a `.js` artifact for
   documentation, review, or as a template doesn't trigger a build.
3. **Explicit intent** — the LLM or user decides when the source is
   "ready" to be compiled.
4. **No hash tracking** — the build is always deterministic from
   the current set of source artifacts. No need to compare hashes
   to detect staleness.
5. **The WIT build stays in sync with the artifacts** — the build
   is always run from the current artifacts in the DB at the moment
   of the build invocation. There is no "cached" build that could
   be stale.

The flow:

```
LLM generates main.js + utils.js
    │
    ▼
solx upload-artifact main.js --owner-action my-action
solx upload-artifact utils.js --owner-action my-action
    │ (no build triggered — just stores the sources)
    ▼
solx exec build-javascript-action '{
    "action": "my-action",
    "entry": "main.js",
    "sources": ["main.js", "utils.js"]
}'
    │ (build-javascript-action is a Command action that runs
    │  sol-quickjs-build with the provided params)
    │ (sol-quickjs-build loads .js artifacts from DB, compiles
    │  via componentize-qjs, writes main.js.wasm as a sibling artifact)
    ▼
solx set action my-action --action-type WASM \
       --bin-name actions::my-action::main.js.wasm
    ▼
solx exec my-action '{"input": "..."}'
    │ (dispatcher sees action_type=WASM, loads main.js.wasm, executes)
    │ (no JS-specific code in the dispatcher)
```

### 4.3 What `build-javascript-action` does

The action is implemented as a **Rust WASM component** in the
`sol-quickjs` package (`actions/sol-quickjs-actions.rs`). It targets
the `custom-action` world and uses `action_exec::exec` to call
`sol-quickjs-build` (the host-side CLI).

```rust
// sol-quickjs/actions/sol-quickjs-actions.rs
use serde_json::{json, Value};
wit_bindgen::generate!({ world: "custom-action", path: "wit" });

use exports::sol::actions::runner::{ActionResult, Guest};
use sol::actions::{action_exec, logger};

struct SolQuickjsActions;

impl Guest for SolQuickjsActions {
    fn run(action_name: Option<String>, params: String)
        -> Result<ActionResult, String>
    {
        match action_name.as_deref() {
            Some("build-javascript-action") => build_action(&params),
            _ => Err(format!("unknown action '{}'", action_name.unwrap_or(""))),
        }
    }
}

export!(SolQuickjsActions);

fn build_action(params: &str) -> Result<ActionResult, String> {
    let input: Value = serde_json::from_str(params)
        .map_err(|e| format!("invalid JSON params: {e}"))?;

    let action = input.get("action").and_then(Value::as_str)
        .ok_or("missing required param: action")?;
    let entry = input.get("entry").and_then(Value::as_str)
        .ok_or("missing required param: entry")?;
    let sources: Vec<String> = input.get("sources")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    logger::log(&format!("[sol-quickjs] building action '{action}' from {} source(s)",
                          sources.len()));

    // Invoke the host-side CLI. We use a shell command action —
    // `sol-quickjs-build` is a Rust binary installed by the package.
    let cmd = format!(
        "sol-quickjs-build --action {action} --entry {entry} --sources {}",
        sources.join(",")
    );
    let payload = json!({ "command": cmd, "cwd": null });
    let result_json = action_exec::exec("command", &payload.to_string())?;

    let result: Value = serde_json::from_str(&result_json)
        .map_err(|e| format!("invalid build result: {e}"))?;

    Ok(ActionResult {
        success: result["success"].as_bool().unwrap_or(false),
        message: result["message"].as_str().map(String::from),
        output: Some(result.to_string()),
    })
}
```

### 4.4 What `sol-quickjs-build` (the CLI) does

The host-side CLI is a Rust binary that links `componentize-qjs` as a
library:

```rust
// sol-quickjs/src/main.rs
use componentize_qjs::{ComponentizeOpts, Runtime, componentize};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args();
    let action = args.action;
    let entry = args.entry;
    let sources: Vec<String> = args.sources;

    // Connect to sol-manager via sol-client (HTTP/IPC)
    let db = connect_to_sol().await?;

    // 1. Load all .js source artifacts from the DB
    let mut js_entries: Vec<(String, Vec<u8>)> = Vec::new();
    for source_name in &sources {
        let artifact_name = format!("actions::{action}::{source_name}");
        let artifact = db.get_artifact(&artifact_name).await?;
        let bytes = base64::decode(&artifact.data)?;
        js_entries.push((source_name.clone(), bytes));
    }

    // 2. Write to a temp directory (preserving relative paths)
    let temp_dir = tempfile::tempdir()?;
    for (name, data) in &js_entries {
        let path = temp_dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, data)?;
    }

    // 3. Write the WIT file to the temp dir
    let wit_path = temp_dir.path().join("sol-actions.wit");
    std::fs::write(&wit_path, include_str!("../../sol-actions/wit/sol-actions.wit"))?;

    // 4. Compile via componentize-qjs
    let entry_path = temp_dir.path().join(&entry);
    let opts = ComponentizeOpts {
        wit_path: &wit_path,
        js_source: &std::fs::read_to_string(&entry_path)?,
        js_path: Some(&entry_path),
        module_root: Some(temp_dir.path()),
        world_name: Some("custom-action"),
        stub_wasi: true,
        disable_gc: false,
        runtime: Runtime::OptSizeSync,
    };
    let wasm_bytes = componentize(&opts).await?;

    // 5. Save the compiled WASM as a sibling artifact
    let wasm_name = format!("{}.wasm", entry);
    let wasm_artifact_name = format!("actions::{action}::{}", wasm_name);
    db.create_artifact_with_data(
        &wasm_artifact_name,
        "application/wasm",
        &base64::encode(&wasm_bytes),
        "actions",
        Some(&action),
    ).await?;

    println!("✓ Compiled {} → {} ({} bytes)",
              entry, wasm_artifact_name, wasm_bytes.len());
    Ok(())
}
```

### 4.5 Multi-file builds

If `actions::my-action::utils.js` is updated, the next build invocation
re-compiles `main.js` (the entry) with the latest `utils.js`. Since
the build always reads from the current DB state, there's no
"stale source" problem.

The entry point is the `.js` artifact whose name matches the `entry`
parameter (e.g. `entry: "main.js"` → the entry is `main.js`).
Utility files (`utils.js`, `helpers/text.js`, etc.) are listed in
`sources` and resolved by `componentize-qjs` via `import` statements.

### 4.6 What the LLM workflow looks like

```
# 1. Upload sources (no build)
solx upload-artifact main.js --owner-action my-action --file ./main.js
solx upload-artifact utils.js --owner-action my-action --file ./utils.js

# 2. Build (one explicit invocation)
solx exec build-javascript-action '{
  "action": "my-action",
  "entry": "main.js",
  "sources": ["main.js", "utils.js"]
}'

# 3. Register the action (one time)
solx set action my-action \
  --action-type WASM \
  --bin-name actions::my-action::main.js.wasm \
  --capabilities "javascript" \
  --phrases "my-action,run my action"

# 4. Run it
solx exec my-action '{"input": "value"}'
```

### 4.7 Why this is better than build-on-upload

| Aspect | Explicit build (chosen) | Build on upload (rejected) |
|---|---|---|
| Batch uploads | One build after all uploads | N builds for N uploads (wasteful) |
| Accidental builds | Only when user invokes build action | Every `.js` upload triggers a build |
| Predictable timing | Build happens when user requests | Build happens during upload (blocks progress) |
| LLM workflow | LLM uploads sources, then builds, then runs | LLM must wait for build during each upload |
| Hash tracking | None (build is always current) | Required to know when to re-build |
| Cache invalidation | Automatic (re-build = re-read from DB) | Requires comparing source vs WASM hashes |
| WIT build sync | Always from current artifacts | Could be stale if upload interleaves |
| Error handling | Build errors are a single ActionExecResult with clear context | Build errors mixed into artifact save responses |

The explicit build approach gives the LLM more control and avoids
wasted work on batch uploads. It's one extra step in the workflow
but is more predictable.

## 5. Multiple JS files per action

### 5.1 The problem

An action may have multiple JS files: a main entry point and utility
modules. The LLM might generate a main script plus helper functions in
separate files. We need a way for the main script to access the other
scripts.

### 5.2 Design: build-time ES module imports

The `custom-action` WIT world imports `artifact-read` (raw byte access)
and `action-exec` (dispatch built-in actions). For multi-file JS, we
rely entirely on **build-time** ES module resolution — no runtime
artifact loading is needed.

The main script uses standard `import` statements. The host writes all
attached `.js` artifacts to a temp directory before calling
`componentize()`, and `componentize-qjs` resolves imports during the
Wizer snapshot. The utility modules are **baked into the WASM component**
at compile time — they don't need to be available at runtime.

```js
// main.js
import { flattenTiptap } from "./utils.js";

export function run(actionName, params) {
    const input = JSON.parse(params);
    const text = flattenTiptap(input.content);
    return { success: true, message: null, output: JSON.stringify({ text }) };
}
```

**How it works**: Before calling `componentize()`, `sol-quickjs-build`:
1. Reads all `.js` artifacts attached to the action from the DB
2. Writes them to a temp directory (preserving relative paths)
3. Passes `js_path: temp_dir/<entry_basename>` and `module_root: temp_dir`
4. `componentize-qjs` resolves `import { foo } from "./utils.js"` from
   the temp directory during the Wizer snapshot

**Pros**: Native ES module semantics, no runtime overhead, standard
JS `import` syntax that LLMs generate naturally, single self-contained
WASM component.

**Cons**: The build is explicit — it does not run on upload. If a
utility script changes, the user/LLM must re-run
`build-javascript-action` to produce a fresh `.wasm`. This is
explicit by design (see §4) and acceptable for the LLM use case (the
LLM re-runs the build after each iteration).

### 5.3 Action config schema

After the build runs, the action entity is registered as a regular
WASM action whose `bin_name` points at the compiled `.wasm`
artifact. The `.js` source files are not part of the action's runtime
config — they're just the inputs to the build.

```json
{
  "action_type": "WASM",
  "bin_name": "actions::my-action::main.js.wasm",
  "artifacts": [
    "actions::my-action::main.js",
    "actions::my-action::utils.js",
    "actions::my-action::helpers/text.js",
    "actions::my-action::main.js.wasm"
  ],
  "action_config": {}
}
```

- `bin_name` — the compiled `.wasm` artifact (produced by
  `build-javascript-action`)
- `artifacts` — all artifacts attached to the action: the `.js`
  sources plus the compiled `.wasm`
- No `js_entry` or `js_module_root` config needed — the build
  already resolved all imports at compile time

If the user updates a `.js` source artifact, they must re-run
`build-javascript-action` to produce a fresh `.wasm`. The action's
`bin_name` does not change; the new `.wasm` overwrites the old one.

### 5.4 Build and execution flow

The build is triggered explicitly by calling
`build-javascript-action` (provided by the `sol-quickjs` package).
The execution is a pure WASM load — no compile step at call time.

```
┌──────────────────────────────────────────────────────────────┐
│  Build (when user/llm invokes build-javascript-action):       │
│                                                              │
│  1. build-javascript-action runs sol-quickjs-build on the     │
│     host (via action_exec::exec("command", ...))             │
│  2. sol-quickjs-build loads all .js artifacts from the DB    │
│  3. Writes .js sources to a temp dir (relative paths)        │
│  4. Calls componentize_qjs::componentize()                     │
│     - js_path: temp_dir/<entry basename>                      │
│     - module_root: temp_dir                                   │
│  5. Saves result as actions::<name>::<entry>.wasm             │
│                                                              │
│  Execute (at action call time):                               │
│  1. Load the .wasm artifact (bin_name)                        │
│  2. Execute via wasm_host::exec(trusted: false)              │
│  3. Return ActionExecResult                                  │
│     (No build, no cache check — pure load)                    │
└──────────────────────────────────────────────────────────────┘
```

---

## 7. Execution flow

The compiled WASM component is executed via the existing
`wasm_host::exec` path with `trusted: false`. Sol's dispatcher does
**not** need a "JavaScript" branch — JS actions register as plain
`WASM` actions (their `bin_name` points at the compiled `.wasm`
artifact), so they go through the same `dispatch_wasm` path as Rust
custom actions.

```rust
wasm_host::exec(
    db,
    search_engine,
    browser_actions,
    &wasm_bytes,
    action.fn_name.as_deref(),  // forwarded as action_name to run()
    params,                     // JSON-encoded params
    action_name,                // for permission checks
    false,                      // trusted = false (custom-action world)
).await
```

The `custom-action` world's host implementations (`action_exec::Host`,
`artifact_read::Host`) are already wired in `wasm_host.rs`. No host-side
changes are needed.

---

## 8. LLM script generation

### 8.1 What the LLM generates

The LLM generates one or more JavaScript ES modules. The entry module
exports a `run` function:

```js
// main.js

import { exec } from "sol:actions/action-exec@0.1.0";
import { flattenTiptap, buildBatchUpdate } from "./utils.js";

export function run(actionName, params) {
    const input = JSON.parse(params);

    // Fetch the Sol document
    const docResult = exec("get document", JSON.stringify({
        name: input.sol_document_name
    }));
    if (!docResult.success) {
        return { success: false, message: docResult.message, output: null };
    }

    const doc = JSON.parse(docResult.output);
    const title = doc.title || input.default_title || "Imported Google Doc";
    const content = doc.contents?.content || {};

    // Use imported utility functions
    const text = flattenTiptap(content);
    const batchUpdateBody = buildBatchUpdate(text);

    return {
        success: true,
        message: null,
        output: JSON.stringify({
            title,
            text,
            batch_update_body: batchUpdateBody,
            create_body: { title, text }
        })
    };
}
```

```js
// utils.js — utility module imported by main.js

export function flattenTiptap(tiptapDoc) {
    let text = "";
    for (const node of tiptapDoc.content || []) {
        if (node.type === "text") text += node.text || "";
        if (node.type === "hardBreak") text += "\n";
        for (const child of node.content || []) {
            if (child.type === "text") text += child.text || "";
        }
        if (node.type === "paragraph") text += "\n\n";
    }
    return text.trim();
}

export function buildBatchUpdate(text) {
    return {
        requests: [{ insertText: { location: { index: 1 }, text } }]
    };
}
```

### 8.2 How the LLM uploads and tests

```
LLM: "I've generated a JS script for the conversion action."
LLM: calls solx upload-artifact main.js --file ./main.js --owner-action convert-sol-doc-to-google-doc
LLM: calls solx upload-artifact utils.js --file ./utils.js --owner-action convert-sol-doc-to-google-doc
LLM: calls solx exec build-javascript-action '{
  "action": "convert-sol-doc-to-google-doc",
  "entry": "main.js",
  "sources": ["main.js", "utils.js"]
}'
LLM: calls solx set action convert-sol-doc-to-google-doc \
       --action-type WASM \
       --bin-name actions::convert-sol-doc-to-google-doc::main.js.wasm
LLM: calls solx exec convert-sol-doc-to-google-doc '{"sol_document_name": "my-doc"}'
Host: loads main.js.wasm, executes via wasm_host::exec
LLM: "The action returned successfully. The document was converted."
```

### 8.3 Error surfacing

If the JS has a syntax error, `componentize()` returns an error with the
line number. The host surfaces this in `ActionExecResult.message`:

```json
{
  "success": false,
  "message": "JS compilation error: SyntaxError: Unexpected token at line 12: ...",
  "result": null
}
```

If the JS throws at runtime, the `run` export returns `Err(string)` which
maps to `ActionExecResult { success: false, message: e, output: null }`.

---

## 9. Frontend integration

### 9.1 ActionEditor

The ActionEditor doesn't need a new "JavaScript" action type — JS
actions register as plain WASM actions whose `bin_name` points at a
`.wasm` artifact. The user just sets up:

- `action_type: "WASM"` (the standard custom-action type)
- `bin_name: "actions::<name>::<entry>.js.wasm"` (the compiled WASM
  produced by `build-javascript-action`)

The `sol-quickjs` package is installed the same way as `sol-google`:
via `bunx solx install-package ../sol-packages/sol-quickjs`. After
install, the `build-javascript-action` and `run-javascript-action`
entities are available in the action list.

A small UI affordance could be added — a "Build" button next to the
Artifacts list that calls `build-javascript-action` against the
attached `.js` files. But this is optional: the LLM/user can call
`solx exec build-javascript-action` from the CLI.

### 9.2 Artifact editor

The artifact editor supports `.js` content type with syntax highlighting
(if the code editor supports it). The user can edit JS source directly
and save — the build is **not** automatic, so the user must invoke
`build-javascript-action` (or re-run the LLM workflow) to recompile
after edits.

---

## 10. Comparison with Rust WASM actions

| | Rust WASM | JavaScript WASM |
|---|---|---|
| Compile time | 30-60s (cargo) | 1-3s (componentize-qjs) |
| Binary size | ~200-400KB | ~1-2MB (includes QuickJS) |
| LLM generation | Poor (Rust is complex) | Excellent (JS is universal) |
| WIT compatibility | Perfect (wit-bindgen) | Good (wit-dylib, same family) |
| Type safety | Compile-time | Runtime |
| Dependencies | Cargo crates | npm packages (via module_root) |
| Use case | Production actions, libraries | LLM-generated scripts, prototyping |

Both coexist: Rust for stable, performance-critical actions (like
`sol-google-actions`); JavaScript for LLM-generated, experimental, or
rapidly-iterating actions.

---

## 11. Open questions

### 11.1 Wasmtime version compatibility

`componentize-qjs` 0.4.1 tracks the latest wasmtime (bumped 2 weeks ago
at time of writing). Sol uses wasmtime 28. We need to verify that
`componentize-qjs` 0.4.1 produces components that wasmtime 28 can parse.
If not, we may need to upgrade wasmtime or pin `componentize-qjs` to a
compatible version.

**Mitigation**: The `custom-action` WIT world has a small surface (2
interfaces, simple types), so the risk of WIT binding mismatch is low
compared to the `backend-action` world that broke with Python.

### 11.2 WASI stubbing

`componentize-qjs` supports `stub_wasi: true` which traps all WASI
imports. Sol's `custom-action` world doesn't use WASI directly, so
stubbing is safe and produces a smaller, self-contained component. We
should verify that `wasm_host::exec` doesn't require WASI for
`custom-action` components.

### 11.3 Memory limits

QuickJS in WASM has a default heap limit. For large scripts or large
input payloads, we may need to configure the heap size. The
`componentize-qjs` API exposes `disable_gc` but not a heap limit — we
may need to patch the runtime or use a custom runtime Wasm.

### 11.4 Async vs. sync

`componentize-qjs` defaults to the async component-model runtime. Sol's
`wasm_host::exec` is synchronous (uses `block_on_db!` for host calls).
We should use `Runtime::OptSizeSync` (the non-async runtime) to avoid
async ABI compatibility issues with wasmtime 28's sync host.

---

## 12. Instruction pipeline integration

### 12.1 The opportunity

The instruction pipeline (`instruction_execute`) lets the LLM plan and
execute multi-step workflows using existing Sol actions. With JS action
support, the LLM can **generate a custom action at runtime** when no
existing action fits the task, then execute it — all within a single
instruction.

### 12.2 Built-in actions for creating artifacts and actions

Sol already has built-in actions the LLM can use:

| Action | Purpose | Key params |
|---|---|---|
| `new artifact` | Create an artifact with inline data | `name`, `data` (base64), `content_type`, `owner_type`, `owner_id` |
| `set artifact` | Update an existing artifact | `name`, `data`, `content_type` |
| `new action` | Create a new action entity | `name`, `action_type`, `bin_name`, `artifact_names`, `fn_name`, `action_config` |
| `set action` | Update an existing action | `name`, `action_type`, `bin_name`, `artifact_names` |
| `exec action` | Execute an action | `name`, `params` (JSON) |

The `new artifact` action accepts `data` as a **base64-encoded string**,
so the LLM can create a JS artifact entirely from within the instruction
pipeline — no file upload needed.

### 12.3 The generate-create-build-execute pattern

When the LLM encounters a task that no existing action can handle, it
can:

1. **Generate** JavaScript source code
2. **Create** a `.js` artifact via `new artifact` (base64-encoded inline)
3. **Create** a sibling `.js` artifact (for utility modules) if needed
4. **Build** the artifacts into a `.wasm` via `exec build-javascript-action`
   (provided by the `sol-quickjs` package)
5. **Update** an existing action (or **create** a new one) via
   `set action` / `new action` with `action_type: "WASM"` and
   `bin_name` pointing to the compiled `.wasm` artifact
6. **Execute** the action via `exec action`

In an ActionScript (the JSON step format the instruction pipeline
produces), this looks like:

```json
{
  "steps": [
    {
      "step_number": 1,
      "action_name": "new artifact",
      "parameters": {
        "name": "convert-csv-to-json.js",
        "content_type": "text/javascript",
        "data": "<base64-encoded JS source>",
        "owner_type": "shared"
      },
      "description": "Create a JS artifact that converts CSV to JSON",
      "depends_on": []
    },
    {
      "step_number": 2,
      "action_name": "exec build-javascript-action",
      "parameters": {
        "action": "convert-csv-to-json",
        "entry": "convert-csv-to-json.js",
        "sources": ["convert-csv-to-json.js"]
      },
      "description": "Compile the JS into a WASM component",
      "depends_on": [1]
    },
    {
      "step_number": 3,
      "action_name": "new action",
      "parameters": {
        "name": "convert-csv-to-json",
        "action_type": "WASM",
        "bin_name": "shared::convert-csv-to-json.js.wasm",
        "caption": "Convert CSV to JSON",
        "description": "Converts CSV text to a JSON array of objects",
        "capabilities": ["conversion", "javascript"],
        "phrases": ["convert csv to json", "csv to json"]
      },
      "description": "Register the compiled WASM as an action",
      "depends_on": [2]
    },
    {
      "step_number": 4,
      "action_name": "exec action",
      "parameters": {
        "name": "convert-csv-to-json",
        "params": { "csv": "name,age\nAlice,30\nBob,25" }
      },
      "description": "Execute the generated action",
      "depends_on": [3]
    }
  ]
}
```

The `build-javascript-action` call is the key new step. It assumes
the `sol-quickjs` package is installed; if it isn't, the LLM should
either install it first (`install package sol-quickjs`) or fall back
to generating a Rust WASM action.

### 12.4 Planner prompt guidance

The instruction plan generator prompt
(`sol-browser/prompts/instruction-plan-generator.prompt.txt`) needs
guidance so the LLM knows it can generate JS actions. Add a section like:

```
## Dynamic action generation (via sol-quickjs)

When no existing action matches the task, you can generate a JavaScript
action at runtime, provided the sol-quickjs package is installed:

1. Use "new artifact" to create a .js artifact (base64-encode the source
   in the "data" field, set content_type to "text/javascript").
2. Use "exec build-javascript-action" to compile the JS sources into a
   .wasm component (params: action, entry, sources). The build is
   explicit — it does not run on upload.
3. Use "new action" or "set action" to register the compiled .wasm as
   an action (action_type: "WASM", bin_name pointing at the .wasm
   artifact).
4. Use "exec action" to execute it.

The JS script must export a `run(actionName, params)` function that
returns { success, message, output }. It can import
"sol:actions/action-exec@0.1.0" to call other Sol actions.

Only generate JS actions when the task cannot be accomplished with
existing actions. Prefer existing actions whenever possible. If the
sol-quickjs package is not installed, prefer using existing actions or
request that the user install it.
```

### 12.5 When to generate vs. use existing

The LLM should **only** generate JS actions when:
- No existing action covers the task
- The task requires custom logic (data transformation, format
  conversion, conditional processing)
- The task is unlikely to be reused (one-off transformations)

For common operations (fetch HTML, get document, model chat, etc.),
the LLM should use existing built-in actions directly.

### 12.6 Security considerations

- JS actions run in the `custom-action` WASM sandbox — they only have
  access to `action-exec` (call other actions) and `artifact-read` (read
  artifacts). No direct DB, filesystem, or network access.
- The `new action` built-in rejects `trusted: true` — LLM-generated
  actions are always untrusted.
- The LLM should not generate JS actions that attempt to access
  `system-ops`, `document-ops`, or `model-ops` — those interfaces are
  only in the `backend-action` world, not `custom-action`.
- The instruction pipeline's `needs_confirmation` flag should be set
  when a step creates a new action (it modifies the entity database).

---

## 13. Implementation plan

1. **Create `sol-packages/sol-quickjs/`** — the new external package:
   - `Cargo.toml` — Rust binary depending on `componentize-qjs`
   - `src/main.rs` — `sol-quickjs-build` CLI: loads `.js` artifacts
     from the DB, compiles via `componentize()`, writes the `.wasm`
     as a sibling artifact
   - `actions/sol-quickjs-actions.rs` — Rust WASM component targeting
     `custom-action` world, implementing `build-javascript-action`
     (invokes `sol-quickjs-build` via `action_exec::exec("command", …)`)
   - `install.solx` — registers the build/run actions
   - `package.json` + `README.md`

2. **Build the package** — `bunx solx install-package ../sol-packages/sol-quickjs`
   - Builds `sol-quickjs-actions.wasm` and uploads it
   - Builds `sol-quickjs-build` and installs it on `PATH`
   - Registers the `build-javascript-action` entity

3. **No changes to `sol-manager`** — the dispatcher, `wasm_host.rs`,
   and the WIT file are all unchanged. The JS build is provided
   entirely by the external `sol-quickjs` package.

4. **Write a test JS action** — port `convert-sol-doc-to-google-doc` to
   JS (single file), upload the artifact, invoke
   `build-javascript-action`, register the action as `WASM`, verify
   the action executes end-to-end.

5. **Write a multi-file test** — split the converter into `main.js` +
   `utils.js`, verify `import` resolution works at build time, verify
   the compiled `.wasm` is self-contained (no runtime artifact lookups
   for utility modules).

6. **Update the instruction plan generator prompt** with guidance on
   when and how to generate JS actions, including the
   `build-javascript-action` step (see §12.4).

7. **Document the JS action authoring guide** in the README — how to
   write a JS script, how to invoke the build, how to register the
   action, how to test it.