# How to build solx packages

A field guide to the package forms in this repo: what each one is, how it
builds, how it installs, and the four ways any of them can be reached from
another language.

If you are an LLM (or a human) generating a new package, read
["Picking a form"](#1-picking-a-form) first, copy the form's skeleton, and
keep the naming rules in ["Naming and layout rules"](#2-naming-and-layout-rules).
Everything else is detail.

A package is a directory holding `solx-package.json` plus an `install.solx`
(and usually `uninstall.solx`). Installing runs the script, which registers
types, files, and actions through the ordinary CLI:

```sh
solx install-package ./solx-ollama
solx list-packages
solx uninstall-package solx-ollama
```

`install-package` also accepts a `.zip` of a package directory (what
`scripts/package.mjs` produces), which it unpacks into the configured
`packages_directory` first.

---

## 1. Picking a form

There are four buildable forms, plus three shared libraries that are not
packages at all.

| Form | Ships | Built by | In this repo |
|---|---|---|---|
| **Rust WASM** | `bin/<pkg>.wasm`, a `wasm32-wasip2` component | `scripts/build-wasm.sh` | `solx-names`, `solx-ollama`, `solx-facebook`, `solx-google` (the latter two under `actions/<crate>/`) |
| **JS WASM** | a `.wasm` compiled at install time from JS source | `solx-quickjs`'s `build-javascript-file` action | `solx-livejournal`, `solx-reddit` |
| **Rust CLI** | a native binary, in `bin/` or `target/release/` | `cargo build --release` | `solx-media`, `solx-omniparse`, `solx-mcp-actions`, `solx-firefox`, `solx-quickjs`, `solx-git` |
| **Node CLI + widget** | `dist/*.js` widget bundle, `dist-headless/*.mjs` command | `npm run build` / `npm run build:headless` | `solx-agent`, `solx-inquiry` |
| **Script-only** | nothing — `install.solx` is the whole package | — | (a package can be this; both script actions in the repo ride along with other forms) |

Shared, **not** installable (no `install.solx`, nothing registered):

| Library | Language | Used by | Purpose |
|---|---|---|---|
| [`solx-package-lib/`](solx-package-lib/) (crate `solx-package-log` in code) | Rust | every CLI-form package | Logging to stderr + a log file + the solx console in one call; `cancelled()` for cooperative cancel; server URL/token resolution; document persistence |
| [`solx-wasm/`](solx-wasm/) | Rust | every WASM-form package, and `solx-quickjs` | The `solx-custom-actions-lib` helper crate and the canonical `wit/custom-action.wit`; `wit-bindgen` points at `solx-wasm/wit` instead of keeping a private copy |
| [`solx-widgets/`](solx-widgets/) | TypeScript | `solx-agent`, `solx-inquiry`, `solx-web` | Build a React component into a single-file widget bundle (`createWidgetConfig`), mount one client-side (`mountWidget`), and the widget's scoped client |

Decide by asking what the action *needs* to touch:

- **Pure logic, no native I/O, must be sandboxed** → Rust WASM.
- **Logic that is easiest to write in JS, and you want it sandboxed** → JS WASM.
- **Needs the filesystem, a subprocess, a native library, secrets it owns, or an existing binary** → Rust CLI.
- **Needs a UI, or is mostly gluing solx calls together in TS/JS** → Node CLI + widget.
- **Just orchestrates other actions** → a script action (no build at all).

### Actions inside a package

A package registers any mix of the five action kinds. The kind is data on the
row, so one package can carry several.

| `actionType` | `fnName` is | Registered by | Typical form |
|---|---|---|---|
| `wasm` | the guest's own identifier, handed to the component's single `run` export as `action-name` (dispatch on it inside) | `save action` with `binName` + `files` | Rust WASM, JS WASM |
| `command` | a **key** into `command_actions` | `save action` with `"actionType": "command"` | Rust CLI, Node CLI |
| `webhook` | a URL (with `{placeholders}`) | `save action` with `"actionType": "webhook"` | REST, usually alongside a WASM/CLI form |
| `script` | a `.solx` artifact | `save action` with `binName` = the `.solx` | Script-only, or orchestration beside anything |
| `internal` | a native handler | solx-core itself | `/builtin` only — not available to packages |

---

## 2. Naming and layout rules

These are what the build scripts key off, so getting them right is what makes
`scripts/build-all.sh` and the release workflow work without per-package edits.

- **Package directory**: `solx-<name>`. All packages live at the repo root.
- **Manifest**: `solx-package.json` — `name`, `version`, `description`, plus
  optional `command_actions` and `allowed_base_urls`. `package.json` is still
  read as a legacy fallback, but only *real* node projects (with a
  `package.json` of their own) should have one.
- **WASM staged artifact**: `<package>/bin/<pkg>.wasm`, and the crate's
  `[package] name` must equal `<pkg>` (the file name without `.wasm`). The
  crate is looked up at `<package>/Cargo.toml` first, then
  `<package>/actions/*/Cargo.toml`.
- **`install.solx` first statement** for a WASM package must be
  `save file files/actions/shared/<pkg>.wasm --file bin/<pkg>.wasm;` — that
  literal line is how `build-wasm.sh` and `build-all.sh` discover what to
  build and what to check.
- **Command `cwd`** is relative to the package directory and is resolved by
  `install-package` against wherever the package is checked out, so nothing
  needs editing per machine. Do **not** put a `cwd` in an action's
  `actionConfig` in `install.solx`: it is not resolved, and the manifest's
  `cwd` takes priority anyway.
- **Actions are locked on install.** Everything `install.solx` leaves under
  `/packages/<name>/` is locked: anyone can run it, but only the CLI, a
  reinstall, or the root token can change it, and a Script/WASM action runs
  only the artifact it was locked against. List any action a model or a
  widget should be able to edit under `"unlocked_actions": ["<name>", ...]`
  in `solx-package.json`, relative to `/packages/<name>/`. After rebuilding
  an artifact outside an install, run `solx action lock <ref>` to re-pin it
  (a locked action whose file changed refuses to run). `install.solx` can't
  set `locked` itself.
- **`command_actions` keys** are the `fnName`s actions use. The program token
  is the command's first word, or its second when the first is an interpreter
  (`node`, `bun`, `deno`, `python`, `python3`).

---

## 3. Rust WASM packages

Sandboxed `wasm32-wasip2` components. The host gives them exactly three
imports — `action-exec`, `artifact-read`, `logger` — and nothing else: no
database, no filesystem, no sockets. Network access is via
`/builtin/web/*` actions, which is why a WASM package that talks to the
network still needs an `allowed_base_urls` grant.

Examples: [`solx-names/`](solx-names/) (no host calls at all),
[`solx-ollama/`](solx-ollama/) (host `exec` + streaming),
[`solx-google/`](solx-google/) and [`solx-facebook/`](solx-facebook/) (crate
under `actions/<crate>/`).

### Skeleton

```
solx-thing/
  Cargo.toml            # [package] name = "solx-thing"  (must match bin/solx-thing.wasm)
  .cargo/config.toml    # alias: wasm = build --release --target wasm32-wasip2
  src/lib.rs            # pure dispatch(fn_name, params_json) -> Outcome, no wit-bindgen
  src/guest.rs          # #[cfg(target_arch = "wasm32")] wit-bindgen shim calling dispatch
  tests/dispatch.rs     # host-target tests of dispatch
  solx-package.json
  install.solx          # first line: save file files/actions/shared/solx-thing.wasm --file bin/solx-thing.wasm;
  uninstall.solx
  README.md
```

### Cargo.toml

```toml
[package]
name = "solx-thing"
version = "0.1.0"
edition = "2021"

# Its own workspace: this is a wasm32-wasip2 artifact, not a member of any
# surrounding workspace.
[workspace]

[lib]
# cdylib produces the component; rlib lets tests/ link the crate so the pure
# logic can be exercised on the host target.
crate-type = ["cdylib", "rlib"]

[dependencies]
serde_json = { version = "1", default-features = false, features = ["alloc"] }

# wit-bindgen only exists for the guest. Keeping it off the host target is what
# lets `cargo test` link.
[target.'cfg(target_arch = "wasm32")'.dependencies]
wit-bindgen = "0.36"

[profile.release]
opt-level = "s"
lto = true
codegen-units = 1
strip = true
panic = "abort"     # a guest panic traps; the host reports success:false
```

`panic = "abort"` makes `cargo test --release` unusable — test in the dev
profile.

### The two-file split

Keep all `wit-bindgen` use in one `wasm32`-only module; the rest is ordinary
Rust that host tests can link.

```rust
// src/lib.rs — no wit-bindgen here.
#[cfg(target_arch = "wasm32")]
mod guest;

use serde_json::{json, Value};

pub struct Outcome { pub success: bool, pub message: Option<String>, pub output: Value }

pub fn dispatch(fn_name: Option<&str>, params_json: &str) -> Outcome {
    // Match on fn_name; parse params_json; return Outcome::ok/fail.
}
```

```rust
// src/guest.rs — the only wasm32-only file.
wit_bindgen::generate!({ world: "custom-action", path: "../solx-wasm/wit" });

use exports::sol::actions::runner::{ActionResult, Guest};

struct Component;

impl Guest for Component {
    fn run(action_name: Option<String>, params: String) -> Result<ActionResult, String> {
        // Always Ok(..): Err renders the same to an operator but drops the
        // machine-readable error object.
        let outcome = crate::dispatch(action_name.as_deref(), &params);
        Ok(ActionResult {
            success: outcome.success,
            message: outcome.message,
            output: Some(outcome.output.to_string()),
        })
    }
}

export!(Component);
```

Alternatively depend on the vendored SDK and skip the shim entirely:
`solx-wasm`'s `solx-custom-actions-lib` (path `../solx-wasm`) re-exports
`exec_action`, `read_artifact`, `log`, and the `Guest` trait.

### install.solx

```solx
save file files/actions/shared/solx-thing.wasm --file bin/solx-thing.wasm;

save type /packages/solx-thing/ThingParams --json {
    "description": "Parameters for the thing action.",
    "schema": { "$schema": "http://json-schema.org/draft-07/schema#", "type": "object", "properties": {} },
};

save action /packages/solx-thing/thing --json {
    "actionType": "wasm",
    "binName": "solx-thing.wasm",
    "files": [
        { "name": "solx-thing.wasm", "relPath": "files/actions/shared/solx-thing.wasm", "contentType": "application/wasm" },
    ],
    "fnName": "thing",                       // dispatched on fn_name inside the component
    "caption": "Thing",
    "category": "utility",
    "description": "…",
    "capabilities": ["thing", "utility"],
    "phrases": ["do the thing"],
    "paramTypeRef": "/packages/solx-thing/ThingParams",
    "actionConfig": { "timeout_secs": 30 },
};
```

One component can back several actions; they share `binName` and `files` and
differ by `fnName` — see [`solx-ollama/install.solx`](solx-ollama/install.solx)
(four actions) and [`solx-livejournal/install.solx`](solx-livejournal/install.solx).

### Build and install

```sh
rustup target add wasm32-wasip2
scripts/build-wasm.sh solx-thing             # build and stage bin/solx-thing.wasm
scripts/build-wasm.sh solx-thing --install   # ...then solx install-package solx-thing
scripts/build-all.sh                         # every package that has anything to build
```

If the package owns secrets an action encrypts (OAuth tokens etc.), list
their names under each action's `actionConfig.secrets`, as
`solx-ollama/install.solx` and `solx-google/install.solx` do. The install
script never handles a key: solx generates one per package, keeps it across
reinstalls (so stored credentials survive an upgrade), and forgets it on
uninstall. Secrets are stored per package, so names only need to be unique
within yours. Only the CLI or a package install — root — can save an action
that lists secrets.

---

## 4. JS WASM packages

Same sandbox and same `wasm` action kind as the Rust form, but the component
is compiled from JavaScript **at install time** by
[`solx-quickjs/`](solx-quickjs/)'s `build-javascript-file` action. There is no
build step and nothing under `bin/` to stage by hand.

Examples: [`solx-livejournal/`](solx-livejournal/),
[`solx-reddit/`](solx-reddit/).

This is the form to reach for when the logic is mostly DOM/JSON wrangling
inside a browser session, or when you want an LLM to be able to write the
action body in JS.

### Skeleton

```
solx-thing/
  src/solx-thing.js        # export const runner = { run(actionName, params) { … } }
  install.solx             # save file → exec build-javascript-file → save type/action
  uninstall.solx
  verify.solx
  solx-package.json
  README.md
```

### install.solx

```solx
save file solx-thing.js --file src/solx-thing.js;
$build = exec /packages/solx-quickjs/build-javascript-file --json {
    "action_name": "solx-thing",
    "entry_artifact_name": "solx-thing.js",
    "source_artifact_names": ["solx-thing.js"],
    "output_artifact_name": "solx-thing.wasm",
};
save type /packages/solx-thing/ThingParams --json { … };
save action /packages/solx-thing/thing --json {
    "actionType": "wasm",
    "binName": "solx-thing.wasm",
    "files": $build.result.files,
    "fnName": "thing",
    …
};
```

Prerequisites at install time: `solx-quickjs` installed **and built** at
`solx-quickjs/target/release/solx-quickjs[.exe]`, and `solx-server` running
(the build action does the compile over HTTP). That ordering is why
`scripts/reinstall-all.sh` reinstalls `solx-quickjs` first.

### The JS source

Export the WIT `runner` interface as a **named export matching its
identifier** — a bare top-level `run` compiles but fails at runtime:

```js
export const runner = {
  run(actionName, params) {
    const p = JSON.parse(params || "{}");
    return { success: true, message: null, output: JSON.stringify({ ok: true }) };
  }
};
```

To call other solx actions from the guest, import the WIT interface:

```js
import { exec } from "sol:actions/action-exec@0.1.0";
const result = exec("/builtin/document/search-documents", JSON.stringify({ q: "x" }));
```

Multiple sources are staged into one module root, so ordinary relative imports
work. `source_artifact_names` is a flat list of file-store artifacts — there
is no npm step and no directory upload — so **pre-bundle** any third-party
dependency with esbuild/rollup and list the single output file.

`build-javascript-action` is the variant that also creates/updates the action
row; `build-javascript-file` (used above) only compiles and uploads, letting
`install.solx` register its own actions against a fixed `binName`.

---

## 5. Rust CLI (Command) packages

A native binary that solx runs with the params as **JSON on stdin** and reads
a JSON result from **stdout**. Reach for it when the action needs the
filesystem, a subprocess, a native library, or the ability to hold its own
secrets.

Examples: [`solx-media/`](solx-media/) and [`solx-omniparse/`](solx-omniparse/)
(stage into `bin/`), [`solx-mcp-actions/`](solx-mcp-actions/) and
[`solx-firefox/`](solx-firefox/) (also `bin/`),
[`solx-quickjs/`](solx-quickjs/) and [`solx-git/`](solx-git/) (run from
`target/release/`).

### Skeleton

```
solx-thing/
  Cargo.toml
  build.rs              # optional: stage the release binary into bin/ (bin/ form only)
  src/main.rs           # clap subcommands + read JSON params from stdin + print JSON
  src/….rs
  solx-package.json     # command_actions: one entry per subcommand
  install.solx          # save type + save action (actionType "command")
  uninstall.solx
  README.md
```

### solx-package.json

```json
{
  "name": "solx-thing",
  "version": "0.1.0",
  "description": "…",
  "command_actions": {
    "thing-do": { "command": ".\\solx-thing.exe do", "cwd": "bin" },
    "thing-list": { "command": ".\\solx-thing.exe list", "cwd": "bin" }
  }
}
```

Use `cwd: "target/release"` and skip `build.rs` staging when you would rather
not copy the binary (that is what `solx-git` and `solx-quickjs` do); use
`cwd: "bin"` with a `build.rs` that stages (the `solx-omniparse` /
`solx-mcp-actions` / `solx-firefox` / `solx-media` pattern) when the package
should be self-contained.

### install.solx

```solx
save type /packages/solx-thing/ThingParams --json { … };

save action /packages/solx-thing/thing-do --json {
    "actionType": "command",
    "fnName": "thing-do",                       // the command_actions KEY, not a command
    "caption": "Do the thing",
    "category": "thing",
    "description": "…",
    "capabilities": ["thing", "command"],
    "phrases": ["do the thing"],
    "paramTypeRef": "/packages/solx-thing/ThingParams",
    "actionConfig": { "timeout_secs": 600 },
};
```

`fnName` is never a literal command — it is a key the allowlist resolves at
exec time. That is why a `Command` action cannot grant itself shell access,
and why a package must declare every key it wants in `command_actions`.

### The CLI contract

- **stdin**: one JSON object of params (empty/absent means "no arguments").
  `run_command` feeds stdin concurrently with draining stdout/stderr, so a
  large payload cannot deadlock against a full pipe.
- **stdout**: the result. A valid JSON document is parsed as JSON; anything
  else is returned as a string. Do not interleave logs here — a running
  action's console renders to **stderr**, and `solx exec … | jq` stays clean.
- **exit code**: non-zero is a failed exec.
- **Environment** `run_command` sets on every invocation:
  `SOL_LOG_DIR` (log file directory), `SOLX_ACTION_REF` (the `/path/name`
  of the action being run, for one command key that serves many actions),
  `SOLX_CONSOLE_URL`/`SOLX_CONSOLE_TOKEN` (the action console loopback),
  `SOLX_CONTROL_URL` (cancellation checks), and, when the host has a token
  authority, `SOLX_SERVER_URL`/`SOLX_SERVER_TOKEN` (the invocation's own
  server credential).
  `actionConfig.env` is ignored: nothing in the action row can add a
  variable.

Use [`solx-package-lib/`](solx-package-lib/) (`solx-package-log` in code) and
you get stderr + log file + console in one `info()` call, `cancelled()` for
cooperative cancellation, and `server::ServerConfig::from_env` +
`persist::put_document` for talking to solx-server — all of which no-op
cleanly when the binary is run by hand.

```rust
solx_package_log::init("solx-thing");
solx_package_log::info("starting");
if solx_package_log::cancelled().await { return Ok(()); }
```

### Build and install

```sh
cargo build --release          # or: scripts/build-all.sh, which builds and stages every package
solx install-package ./solx-thing
```

The auto-staging `build.rs` pattern compiles into a package-local
`.build-target/` (so the workspace `target/` is untouched) and copies into
`bin/`, warning rather than failing if the copy fails — which is exactly why
`scripts/build-all.sh` re-stages from cargo's own output and then verifies
each command's program is present and current. It honours a per-package
`SOLX_<NAME>_SKIP_AUTOBUILD=1` escape hatch.

---

## 6. Node CLI + widget packages

Two artifacts from one package: a **widget bundle** (a React component wrapped
as a self-mounting custom element, served from the file store and mounted
client-side) and a **headless command** (the same logic with no UI, for
driving from the CLI). This is the form used when the package needs a UI, or
when the action is mainly orchestration that reads best in TypeScript.

Examples: [`solx-agent/`](solx-agent/) and [`solx-inquiry/`](solx-inquiry/).
[`solx-widgets/`](solx-widgets/) is the shared toolkit both consume as
source.

### Skeleton

```
solx-thing/
  package.json          # scripts: build (vite), build:headless (esbuild)
  vite.config.ts        # createWidgetConfig({ entry, outFile })
  tsconfig.json
  src/main.ts           # defineReactWidget("solx-thing-widget", ThingWidget)
  src/ThingWidget.tsx
  src/headless/thingEntry.ts   # the headless entry point
  thing-widget-descriptor.solx # {"tag_name":"…","bin_name":"widgets/solx-thing.js","fields":null}
  solx-package.json     # command_actions: { "thing": { "command": "node dist-headless/thing.mjs", "cwd": "." } }
  install.solx
  uninstall.solx
  README.md
```

### package.json

```json
{
  "name": "solx-thing",
  "private": true,
  "type": "module",
  "scripts": {
    "build": "vite build",
    "build:headless": "esbuild src/headless/thingEntry.ts --bundle --platform=node --format=esm --target=node18 --outfile=dist-headless/thing.mjs",
    "typecheck": "tsc --noEmit",
    "test": "vitest run"
  }
}
```

```ts
// vite.config.ts — bundles into one self-contained ESM file, CSS injected at
// runtime (a blob-URL'd bundle has no base path for a sibling chunk).
import { createWidgetConfig } from "../solx-widgets/src/build/widgetViteConfig";
export default createWidgetConfig({ entry: "src/main.ts", outFile: "solx-thing.js" });
```

### install.solx

```solx
save file widgets/solx-thing.js --file dist/solx-thing.js;
save file files/actions/shared/solx-thing-widget-descriptor.solx --file thing-widget-descriptor.solx;

save action /packages/solx-thing/thing-widget --json {
    "actionType": "script",
    "binName": "solx-thing-widget-descriptor.solx",
    "files": [
        { "name": "solx-thing-widget-descriptor.solx",
          "relPath": "files/actions/shared/solx-thing-widget-descriptor.solx",
          "contentType": "text/x-solx" },
    ],
    "caption": "Thing",
    "category": "widgets",
    "description": "…",
    "capabilities": ["thing", "widgets"],
    "phrases": ["open the thing"],
    "resultTypeRef": "/builtin/types/WidgetDescriptor",   // <- what makes it a widget
};

save action /packages/solx-thing/thing --json {
    "actionType": "command",
    "fnName": "thing",
    "description": "…",
    "actionConfig": { "timeout_secs": 600 },
};
```

The widget action itself is a `script` action that returns the descriptor
literal from `thing-widget-descriptor.solx`; the bundle it names is what the
frontend fetches over `GET /files/{bin_name}` and mounts. Nothing on the
backend understands widgets beyond that `resultTypeRef` discriminator — see
[solx-core's widget-actions.md](../solx-core/docs/widget-actions.md).

### Build and install

```sh
npm install && npm run build && npm run build:headless
solx install-package ./solx-thing
```

`scripts/build-all.sh` detects a `command_actions` package with a
`package.json` (and no `Cargo.toml`), installs dependencies if `node_modules`
is missing, and runs whichever of `build` / `build:headless` the package
defines, using the runner its lockfile belongs to (`npm` for
`package-lock.json`, `bun` for `bun.lock`). The release workflow does the same
and zips `dist/` and `dist-headless/`.

---

## 7. Script actions and script-only packages

A `script` action is a `.solx` artifact — no compiler, no build script, no
`bin/`. It is the natural form for composition: an orchestrator that chains
several other actions in one call.

Examples:
[`solx-firefox/mcp-setup.solx`](solx-firefox/mcp-setup.solx),
[`solx-google/login.solx`](solx-google/login.solx),
[`solx-facebook/login.solx`](solx-facebook/login.solx).

```solx
save file files/actions/shared/solx-thing-orchestrate.solx --file orchestrate.solx;

save action /packages/solx-thing/orchestrate --json {
    "actionType": "script",
    "binName": "solx-thing-orchestrate.solx",
    "files": [
        { "name": "solx-thing-orchestrate.solx",
          "relPath": "files/actions/shared/solx-thing-orchestrate.solx",
          "contentType": "text/x-solx" },
    ],
    "caption": "Orchestrate",
    "category": "thing",
    "description": "…",
    "phrases": ["run the thing"],
};
```

The script body is seeded with the caller's payload as `$params` and can call
anything:

```solx
$start = exec /packages/solx-thing/thing-start --json {};
$import = exec /packages/solx-mcp-actions/import --json {"server": "thing"};
{"thing": $start, "import": $import};
```

A **script-only** package is one whose entire content is like this (plus
types/documents): no `Cargo.toml`, no `package.json`, no `bin/`.
`scripts/package.mjs` classifies it as `source` and zips the checked-out
directory as-is.

### install.solx / uninstall.solx are themselves scripts

Every package's `install.solx` and `uninstall.solx` run through the same
language. Useful when generating a package:

- Statements run in order and the script **aborts on the first failure** —
  which is why every `delete` in a teardown carries `--if-exists`.
- `save`, `get`, `delete`, `list`, `exec`, `json`, `str`, `escape`, `random`
  are the verbs; `;` sequences, `|` pipes, `$var` holds values, and
  `if … then … else … endif` branches.
- `save action` is an upsert, so `install.solx` alone brings an existing
  install up to date. That is what
  [`scripts/reinstall-all.sh`](scripts/reinstall-all.sh) relies on — it
  deliberately does **not** uninstall first, because a clean uninstall would
  revoke grants and drop secrets for packages that carry them.

---

## 8. Integrating with another language

Every action, whatever its form, is reachable the same four ways. Pick by how
the *caller* is written, not by how the action was built.

| Method | Best when | Surface |
|---|---|---|
| **WASM** | your caller is another solx action, and you want it sandboxed | `action-exec` import |
| **CLI command** | your caller can spawn a process | `solx exec`, or the binary contract directly |
| **REST** | your caller is a separate process/service in any language | `solx-server` on `127.0.0.1:8766` |
| **solx script** | you want to compose several actions in one round trip | `solx script` |

### 8.1 WASM

A WASM action reaches other actions through the single `action-exec` import —
`exec(actionRef, paramsJson)` with a full `/path/name` reference, including
every built-in. Calls are attributed to the calling action, which is what
scopes `/builtin/secrets` reads to the secret names in the guest's own
`action_config.secrets`, within its package.
Guests **cannot** create, modify or delete `command`/`webhook` actions, so a
component can never grant itself shell or outbound HTTP, and cannot create or
modify any action that has secrets.

From Rust, `solx-wasm` re-exports `exec_action` / `read_artifact` / `log`.
From JS, `import { exec } from "sol:actions/action-exec@0.1.0"`.

The world is deliberately tiny — `action-exec`, `artifact-read`, `logger` are
the *only* imports. Wanting more is the signal to be a CLI package instead.

### 8.2 CLI commands

Three shapes, in increasing directness:

```sh
# 1. Run a registered action, whatever its kind.
solx exec /packages/solx-thing/thing --json '{"a":1}'

# 2. Run an inline script.
solx script -e 'exec /packages/solx-thing/thing --json {"a":1}'

# 3. Run the package's own binary, bypassing solx entirely.
echo '{"a":1}' | ./solx-thing/bin/solx-thing do
```

(3) is the `Command` action contract and needs no solx at all: JSON params on
stdin, JSON (or a string) on stdout, non-zero exit on failure. That is the
cheapest integration point if you already have the binary. (1) and (2) both
still work when the action is registered, and (1) is what every other surface
calls under the hood.

### 8.3 REST endpoints

Two distinct things share this name. Do not confuse them.

**(a) `webhook` actions — package-owned outbound REST.** A package can
register its upstream HTTP call as an action, so a caller does not need to
know the API's shape. `fnName` is the URL, `{placeholders}` are filled from
the params, and `actionConfig.method` picks GET (params become query string)
vs POST (params become the JSON body). Auth credentials are never inline in
`actionConfig.auth`: each comes from a `<field>_secret` pointer into the
action's own scoped secrets (OAuth refresh-token flow, bearer) or a keyring
reference; custom headers are plain `actionConfig.headers`.
The host must have the target host in `allowed_base_urls` — declared in
`solx-package.json` and granted at install.

```solx
save action /packages/solx-thing/thing-get --json {
    "actionType": "webhook",
    "fnName": "https://api.example.com/v1/things/{id}",
    "actionConfig": {
        "method": "GET",
        "secrets": ["EXAMPLE_TOKEN"],
        "timeout_secs": 30,
    },
    …
};
```

**(b) `solx-server` — solx's own REST API.** This is the integration point for
a caller written in another language: start `solx server start`, then talk
HTTP to `http://127.0.0.1:8766` with `Authorization: Bearer <token>`
(run `solx token show` to get it). The reference in the URL is just `path` + `name`, and
everything solx can do is a route:

| Purpose | Route |
|---|---|
| Execute an action | `POST /actions/{path}/{name}` with the params as the JSON body |
| Read/write a document | `GET` / `PUT /docs/{path}/{name}` |
| List / search documents | `GET /docs`, `GET /search?q=…` |
| Types | `GET` / `PUT /types/{ref}`, `POST /validate` |
| Files (raw bytes) | `GET` / `PUT` / `DELETE /files/{relPath}` |
| Health (no auth) | `GET /health` |
| MCP over Streamable HTTP | `POST` / `GET` / `DELETE /mcp` |

Errors are the tagged body `{ "kind": …, "message": … }` with a conventional
status (`not_found` 404, `invalid` 400, `validation` 422, …). Percent-encode
each path segment. See [solx-core's http-api.md](../solx-core/docs/http-api.md)
for every query parameter.

```sh
curl -X POST "$BASE/actions/packages/solx-thing/thing" \
     -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
     -d '{"a":1}'

# PUT is create-or-replace, and always answers 200 with the saved entity.
curl -X PUT "$BASE/docs/research/ai/note" -H "Authorization: Bearer $TOKEN" \
     -H 'Content-Type: application/json' \
     -d '{"typeRef":"/types/docs/Document","title":"A note","contents":{}}'
```

Two shortcuts already exist if you are in their ecosystem: Rust via
`solx-client` (the manager traits over HTTP), TypeScript/browser via
`@solx/http`'s `connectHttp(baseUrl, token)`. A Tauri webview uses
`@solx/tauri`'s `connectTauri` instead — same methods, no URL or token.

For an MCP client (an LLM host), the same catalogue is available over stdio
via `solx-mcp`, or over HTTP at `/mcp`. The stdio server reads an optional
`SOLX_MCP_PATH_PREFIX`, which restricts the tool catalogue to that subtree
(`/builtin`, `/packages/solx-google`, …), so you can run several narrowly
scoped instances instead of flooding one model's context. The server's
mounted `/mcp` route always exposes the full catalogue.

### 8.4 solx scripts

The composition method. `solx script -f file.solx` or `-e '…'` runs it from
the CLI; a `script`-typed action is a stored script with a real reference.
(There is no built-in for running inline script source over MCP or HTTP;
save a `script` action instead.)

- `exec <ref> [--json '<params>']` calls an action; its result is the stage
  value, so `$r = exec …` captures it.
- `json <value>` and `str <value>` are the value primitives; `|` pipes a
  stage's value into the next.
- `$params` is seeded from the caller's payload for a script action.
- `$var` holds intermediate values; `if … then … else … endif` branches.

Because it drives the ordinary CLI, a script composes across languages freely:
a Rust WASM action, a JS WASM action, a Rust CLI, a webhook, and a built-in can
all appear in one script.

---

## 9. Building and releasing everything

| Script | Does |
|---|---|
| [`scripts/build-wasm.sh`](scripts/build-wasm.sh) `<pkg> [--install]` | Builds one WASM package and stages `bin/<pkg>.wasm`; `--install` also installs it |
| [`scripts/build-all.sh`](scripts/build-all.sh) | Builds every package with something to build, then verifies every artifact an install depends on is present and current (non-zero exit otherwise) |
| [`scripts/reinstall-all.sh`](scripts/reinstall-all.sh) | Reinstalls every package against a running server, in dependency order, without uninstalling first |
| [`scripts/package.mjs`](scripts/package.mjs) | `plan` (CI matrix), `package <pkg> --platform <p> [--out <dir>]` (zip one built package), `companions <pkg>` (sibling dirs it builds from), `extra-builds <pkg>` (its `extra_binaries` build commands) |
| [`scripts/sync-solx-wasm.sh`](scripts/sync-solx-wasm.sh) `<solx-checkout>` | Refreshes the vendored `solx-wasm/` from `core/solx-wasm` in the solx repo |

`package.mjs` classifies each package the same way, and that classification is
worth internalizing:

- `wasm` — `install.solx` stages a `bin/*.wasm`; platform-independent, one zip.
- `rust` — has `command_actions` and a `Cargo.toml`; **one zip per platform**,
  with `solx-package.json` rewritten so every command runs from `bin/`. A
  `rust` package can also ship programs that aren't commands by declaring
  `extra_binaries` (`{"<name>": {"cwd": "…", "build": "…", "needs": "bun"}}`):
  CI and `build-all.sh` run each `build` in its `cwd`, which must leave
  `bin/<name>[.exe]` or fail, and the zip carries it. `solx-firefox`'s
  compiled MCP server is the example (see
  [docs/bundled-mcp-server.md](docs/bundled-mcp-server.md)).
- `node` — has `command_actions` and a `package.json`; zips `dist/` +
  `dist-headless/`.
- `source` — everything else; the tracked source is the package.

Needs `cargo` + the `wasm32-wasip2` target, and `node` (`npm`, or `bun` for
packages whose lockfile is bun's, and for `solx-firefox`'s MCP server). The
[release workflow](.github/workflows/release.yml) runs this on every PR and
push, keeps the zips as artifacts, and publishes them to a draft release on a
`v*` tag.

---

## 10. Checklist for a new package

1. Pick the form (§1) and copy its skeleton from §3–§7.
2. Name the directory; write `solx-package.json` with `name`,
   `version`, `description`, and any `command_actions` / `allowed_base_urls`
   the package needs.
3. Make the naming rules hold (§2): the crate name matches the staged wasm
   filename, and the WASM `install.solx` opens with the exact
   `save file files/actions/shared/<pkg>.wasm --file bin/<pkg>.wasm;` line.
4. Write `install.solx` to register types then actions; write
   `uninstall.solx` that deletes them, each with `--if-exists`.
5. Build it (`scripts/build-all.sh`, or the form's own command), install it,
   and exercise it with a `verify.solx` plus `cargo test` / `npm test` where
   the form has host-target logic to test.
6. Write the package's own `README.md`: prerequisites, build, install,
   usage, and any external tools it needs.

Nothing here edits the release workflow — packages are discovered from their
`solx-package.json`.
