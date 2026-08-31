# solx-quickjs

This package adds a lightweight JavaScript action build flow for solx-core.

## What it provides

- a Rust CLI that compiles a JavaScript source file into a `wasm32-wasip2`
  component (via `componentize-qjs`) against solx-core's `custom-action` WIT
  world
- a `build-javascript-action` Command action (registered by `install.solx`)
  that invokes the CLI through `solx exec`
- a `build-javascript-file` Command action — the same build, but it only
  uploads the wasm artifact and never touches an action row, for packages
  that want to `save action` against the artifact themselves (see below)
- a sample `.solx` workflow for build → execute

`build-javascript-action` is a **single action** that does the whole
pipeline in one invocation: it reads the JavaScript source from the file
store, compiles it to a wasm component, uploads the wasm back to the file
store, and points the target action's `bin_name` at the uploaded artifact.
There is no separate `save file` / `save action` step — the target action is
created (or updated) by the build action itself.

`build-javascript-file` runs the identical build + upload, but stops there —
no action row is created or updated, regardless of what params it's called
with. It exists for packages whose own `install.solx` registers actions
against a fixed, known `bin_name`: the package stages its JS source, calls
`build-javascript-file` to compile+upload it, then `save action`s its own
actions with `bin_name` set to the same `output_artifact_name`. See
`solx-livejournal/install.solx` for a worked example.

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

`install.solx` registers **only the builders**: the `QuickjsBuildParams` /
`QuickjsBuildFileParams` types and the `build-javascript-action` /
`build-javascript-file` Command actions. It does not stage any sample
JavaScript or create any demo action — those are examples of *using* the
builder, run later by hand (see "Example workflow" below).

1. **Build the CLI first** (see above). `install.solx` registers
   `build-javascript-action`/`build-javascript-file` as `Command` actions
   whose `fn_name` resolves (via `package.json`'s `command_actions`) to
   `.\solx-quickjs.exe` and `.\solx-quickjs.exe --file-only` respectively,
   both resolved against the same `cwd` — the binary has to already exist at
   that path when either action is later invoked.
2. **Edit `action_config.cwd`** in `install.solx` before installing: replace
   `D:/Projects/solx-packages/solx-quickjs/target/release` with the absolute
   path to this package's `target/release` directory. `.solx` has no path
   templating, so this is a manual edit. (Alternatively, install first and
   `solx save action` the same reference again afterward to correct it.)
3. Run `solx install-package .` from this directory.

Registering a `Command` action has no allowlist gate: `fn_name` is the
literal command solx-core will run the moment `install-package` posts the
action row, with no confirmation step.

### Server connection

The build action talks back to `solx-server` over HTTP to read the JS source
and write the wasm + action row. It needs `SOLX_SERVER_URL` (set on the
action's `action_config.env` by `install.solx`) and a bearer token. The token
is resolved in this order:

1. `SOLX_SERVER_TOKEN` (or `SOLX_TOKEN`) from the environment / `action_config.env`;
2. `server_token` from `solx-config.json` in the default appdata dir
   (`%APPDATA%/praeus/solx` on Windows) — `solx-server` generates this on
   first run, so a freshly installed action works without baking the token
   into the action config.

Params (`action_name`, `path`, `entry_artifact_name`, `source_artifact_names`,
`output_artifact_name`, `artifact_root`) are passed to the CLI as JSON on
stdin, per solx-core's `Command` action contract; the CLI falls back to
parsing them as flags only when stdin is a terminal (direct manual
invocation). `build-javascript-file`'s `QuickjsBuildFileParams` is the same
shape minus `path`, which it ignores — there's no target action to place it
on.

### Two source-loading modes

- **Server mode (default)** — `artifact_root` is absent. The CLI reads each
  `source_artifact_names` entry from the file store via
  `GET /files/...`, compiles, and uploads the wasm to
  `files/actions/shared/<output_artifact_name>`. `build-javascript-action`
  additionally upserts the target action (`PUT /actions/{path}/{name}`) with
  `action_type: "wasm"` and `bin_name: "<output_artifact_name>"`;
  `build-javascript-file` stops after the upload — the `--file-only` flag
  baked into its `command_actions` entry (not a JSON param, so a caller can't
  turn it off) skips that step unconditionally.
- **Local mode** — `artifact_root` is set (manual CLI invocation). The CLI
  reads sources from local disk and writes the wasm back to disk, with no
  HTTP. This is the old 3-step flow, kept for direct manual use. Action
  upsert never applies here either way, since it's server-mode-only.

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

### Multiple files, and libraries

`source_artifact_names` is a list, and every entry is staged into one temp
directory that becomes the module root. componentize-qjs resolves imports from
there with a real node resolver (`oxc_resolver`, conditions `import`/`default`,
extensions `.mjs`/`.js`, main fields `module`/`main`), so ordinary relative
imports between your own files just work:

```
save file names.js --file src/names.js;
save file main.js  --file src/main.js;
exec /packages/solx-quickjs/build-javascript-file --json '{"entry_artifact_name":"main.js","source_artifact_names":["main.js","names.js"],"output_artifact_name":"thing.wasm"}';
```

```js
import { pick } from "./names.js";
```

**Artifact names are staged as relative paths**, so directory structure is
preserved and `vendor/lib/index.js` is importable as `./vendor/lib/index.js`.
Names are validated first: no absolute paths, no root or drive prefix, and no
`..` — a source artifact cannot be staged outside the module root.

That is what makes a third-party library usable, with one caveat about how
you get it here. There is no npm step in this pipeline and no directory
upload: the file store is addressed one file at a time, so every file a
dependency needs must be listed in `source_artifact_names` individually. In
practice that means **pre-bundling**. Run esbuild or rollup over the package,
commit the single ESM output, stage that one file, and import it by path:

```bash
npx esbuild --bundle --format=esm --outfile=vendor/some-lib.js entry-for-lib.js
```

Bare specifiers (`import x from "some-lib"`) would resolve if a `node_modules`
tree existed under the module root, but nothing stages one for you — you would
have to list every file of it. Prefer the bundle.

Everything staged is compiled into the wasm, so a dependency's weight is
permanent per-action size. Weigh that against writing the twenty lines
yourself.

To call other solx actions from within the guest (recursively, permission-gated), import
`sol:actions/action-exec@0.1.0` and call `exec(actionRef, paramsJson)` with a
full `/path/name` action reference — see
[actions/sample-document-summary.js](actions/sample-document-summary.js).

## Example workflow

1. stage the sample JS source into the file store:
   `solx save file files/actions/shared/sample-document-summary.js --file <abs path to actions/sample-document-summary.js>`
2. build + register + execute, all in one action:
   `solx exec /packages/solx-quickjs/build-javascript-action --json '{"action_name":"demo-js-action","path":"/packages/solx-quickjs","entry_artifact_name":"sample-document-summary.js","source_artifact_names":["sample-document-summary.js"]}'`
3. `solx exec /packages/solx-quickjs/demo-js-action --json '{"limit":5}'`

See [actions/sample-workflow.solx](actions/sample-workflow.solx) for a
runnable reference — replace `REPLACE_WITH_ABSOLUTE_PATH_TO_THIS_DIR` with
the absolute path to this `actions/` directory, then run it with
`solx script -f sample-workflow.solx` after installing.
`actions/solx-quickjs-actions.rs` (a WASM wrapper around the CLI) is retired
— see the comment at the top of that file for why it doesn't map onto
solx-core's `action-exec` semantics.
