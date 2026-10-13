# solx-packages

Installable action bundles for [solx-core](../solx-core). A package adds
capabilities to a solx instance — Google Workspace, a local LLM, browser
control, media extraction — without any change to solx-core itself.

**[HOWTO.md](HOWTO.md)** is the authoring guide: the four package forms
(Rust WASM, JS WASM, Rust CLI, Node CLI + widget), the shared
`solx-package-lib`/`solx-wasm`/`solx-widgets` libraries, the build and install
rules each form follows, and how any action integrates with another language
over WASM, CLI, REST, or solx scripts.

## What a package is

A directory with a `solx-package.json` and an `install.solx` script.
Installing runs the script, which registers types and actions through the
ordinary CLI:

```sh
solx install-package ./solx-ollama
solx exec /packages/solx-ollama/ollama-chat --json '{"model":"llama3.2:1b","messages":[{"role":"user","content":"hi"}]}'
```

Every action a package registers is immediately available everywhere solx is
— CLI, HTTP, and as an MCP tool — because they're all projections of the same
actions database.

### Allowlist grants

solx-core denies shell commands and outbound HTTP by default: a command's
`fn_name` must be a registered key, and **any** outbound URL must match an
allowed prefix — a `webhook` action's, and any URL handed to
`/builtin/web/http-request`, `/builtin/web/stream/start` or
`/builtin/web/open-url`. A package declares what it needs in
`solx-package.json`, in the same shape `solx-config.json` uses:

```json
{
  "name": "solx-google",
  "version": "0.1.0",
  "allowed_base_urls": [
    "https://gmail.googleapis.com/",
    "https://oauth2.googleapis.com/"
  ]
}
```

```json
{
  "name": "solx-media",
  "version": "0.1.0",
  "command_actions": {
    "solx-media-image": { "command": "./solx-media image", "cwd": "…/bin" }
  }
}
```

That second gate is why a wasm package needs a grant even though it registers
no webhook rows: a guest has no sockets, so `/builtin/web/*` is how it reaches
the network at all. `solx-ollama` cannot talk to Ollama without one.

A URL resolved at call time can outrun a static manifest. `solx-ollama`
declares the default `http://localhost:11434/`, so pointing it at a remote
Ollama through `base_url` or `OLLAMA_HOST` needs that prefix added to
`solx-config.json` by hand.

Install grants exactly those entries and records them; uninstall revokes
exactly those, unless another installed package also declares them. This is
the migration path for a package to work under the deny-by-default posture —
reinstalling is how an older package picks up its grants.

Reviewing this block is the single most useful thing you can do before
installing a package you didn't write: it is a complete list of the shell
commands and outbound hosts that package is asking for.

## Packages

| Package | Kind | What it does |
|---|---|---|
| `solx-google`      | Webhook | Google Docs, Drive, Gmail, Tasks, and Calendar over their REST APIs, with OAuth credentials encrypted into the scoped secret store. Includes a one-shot `login.solx` orchestrator. |
| `solx-ollama`      | WASM | Chat, list models, pull a model, and set an API key against a local or remote Ollama server. Token stream renders to the action console live. |
| `solx-mcp-actions` | Command | Connects to third-party MCP servers over stdio and imports their tools as ordinary solx `Command` actions. |
| `solx-media`       | Command | Image (vision), audio (whisper), and video (whisper + scene captions) extraction via Ollama + ffmpeg, persisted as `MediaDocument` documents. |
| `solx-omniparse`   | Command | Document extraction preprocessing with OCR support. |
| `solx-firefox`     | Command | Manages one persistent, dedicated-profile Firefox with Marionette enabled, so browser tool calls share a session instead of respawning a browser each time. |
| `solx-livejournal` | WASM | Extracts a LiveJournal into `BlogPostWithComments` documents with the full comment tree, resumably. |
| `solx-quickjs`     | Command | Build tool: compiles a JavaScript action into a WASM component via `componentize-qjs`. |
| `solx-agent`       | Script | A supervised agent thread: the action registry as an LLM tool catalogue, driven one iteration at a time behind a default-deny gate with human approval for destructive calls. The harness runs inside the widget bundle; sessions, memories and skills are documents under `/agent`. |
| `solx-inquiry`     | Script | Ask a question and get an answer grounded in solx documents. A companion to `solx-agent`: the model is handed five fixed read-only verbs (`search`/`list`/`get`/`types`/`actions`) instead of a tool catalogue, so it never discovers or is refused a tool. Plan, gather, answer -- two model calls, no approval. Inquiries are documents under `/inquiry`. |

`solx-package-lib/` is not a package — it's a shared Rust crate (`solx-package-log`
in code, since every consumer still calls it via `solx_package_log::...`)
that `Command`-action packages depend on for logging to stderr, a log file,
and the solx-core action console in one call; resolving where solx-server is
and how to authenticate to it (`SOLX_SERVER_URL`/`SOLX_SERVER_TOKEN`, falling
back to `solx-config.json`); and persisting a document to it.

`solx-wasm/` is a vendored copy of solx's `core/solx-wasm`: the helper crate
for custom WASM actions and the canonical `wit/custom-action.wit`. The WASM
packages point `wit-bindgen` at `solx-wasm/wit` instead of keeping their own
copy, and `solx-quickjs` depends on the crate by path. It rarely changes;
refresh it with `scripts/sync-solx-wasm.sh <path-to-solx>`.

`solx-widgets/` is likewise not an installable package — it's a shared TS
toolkit (build a React component into a single-file widget bundle; mount one
client-side) that widget-content packages like `solx-agent`, `solx-inquiry`
and `solx-web` both consume as source. See
[`solx-widgets/README.md`](solx-widgets/README.md).

## Building

Packages that ship a binary or a WASM component build before they install.
The Rust WASM packages (solx-names, solx-ollama, solx-facebook, solx-google)
all build with one script, which stages the component in the package's
`bin/` for its `install.solx` to upload:

```sh
scripts/build-wasm.sh solx-ollama             # build and stage bin/solx-ollama.wasm
scripts/build-wasm.sh solx-ollama --install   # ...then solx install-package solx-ollama
```

To build everything, run `scripts/build-all.sh`. It builds every WASM package
with `build-wasm.sh`, release-builds every Rust command package and stages its
binary into `bin/` where the package runs from there, and builds the node
command packages (solx-agent). It then checks that each command's program is
current, and exits non-zero if anything failed or is stale.

WASM packages target `wasm32-wasip2`:

```sh
rustup target add wasm32-wasip2
```

Packages using `bin/*.exe` command actions need a release build of their Rust
binary first; see each package's own README for its prerequisites (several
need external tools — `ffmpeg`, `whisper`, an Ollama server, or Firefox).

## Command action paths

A package's `solx-package.json` `command_actions` give each command a `cwd`
relative to the package directory (`"bin"`, `"target/release"`, `"."`).
`install-package` resolves it against wherever the package is checked out,
so nothing needs editing per machine. Don't put a `cwd` in an action's
`actionConfig` in `install.solx`: it isn't resolved, and the manifest's
`cwd` takes priority anyway.

## Docs

[docs/](docs/) holds the design and implementation notes for the larger
packages: [mcp-actions.md](docs/mcp-actions.md),
[media-actions.md](docs/media-actions.md),
[javascript-actions.md](docs/javascript-actions.md) (a design proposal, not
yet implemented), and [widget-system.md](docs/widget-system.md) (how
`solx-widgets` + `solx-agent` implement the frontend half of
`solx-core/docs/widget-actions.md`).

## Security

Installing a package runs its `install.solx` and grants the shell commands
and outbound hosts its `solx-package.json` declares. There is no package signing
or verification today — a package is trusted the moment you install it. Read
the manifest first. See
[solx-core's SECURITY.md](../solx-core/SECURITY.md) for the full model.

## License

MIT OR Apache-2.0.
