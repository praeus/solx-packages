# solx-packages

Installable action bundles for [solx-core](../solx-core). A package adds
capabilities to a solx instance — Google Workspace, a local LLM, browser
control, media extraction — without any change to solx-core itself.

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
`/builtin/web/http_request`, `/builtin/web/stream/start` or
`/builtin/web/open_url`. A package declares what it needs in
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

`solx-package-lib/` is not a package — it's a shared Rust crate (`solx-package-log`
in code, since every consumer still calls it via `solx_package_log::...`)
that `Command`-action packages depend on for logging to stderr, a log file,
and the solx-core action console in one call; resolving where solx-server is
and how to authenticate to it (`SOLX_SERVER_URL`/`SOLX_SERVER_TOKEN`, falling
back to `solx-config.json`); and persisting a document to it.

`solx-widgets/` is likewise not an installable package — it's a shared TS
toolkit (build a React component into a single-file widget bundle; mount one
client-side) that widget-content packages like `solx-agent` and `solx-web`
both consume as source. See [`solx-widgets/README.md`](solx-widgets/README.md).

## Building

Packages that ship a binary or a WASM component build before they install:

```sh
cd solx-ollama && ./build.sh          # or build.ps1 on Windows
cd ../ && solx install-package ./solx-ollama
```

WASM packages target `wasm32-wasip2`:

```sh
rustup target add wasm32-wasip2
```

Packages using `bin/*.exe` command actions need a release build of their Rust
binary first; see each package's own README for its prerequisites (several
need external tools — `ffmpeg`, `whisper`, an Ollama server, or Firefox).

## Known limitation: absolute paths

Several packages currently hardcode `D:/Projects/solx-packages/...` in their
`solx-package.json` `command_actions` `cwd` and `command` fields. These are
development-machine paths and **will not work on another machine** — edit
them to match your checkout before installing, or the registered commands
will resolve to nothing. Making these relative to the package directory is
tracked work, not a design decision.

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
