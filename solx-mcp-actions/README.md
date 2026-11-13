# solx-mcp-actions

MCP (Model Context Protocol) action package for solx-core. Connects to MCP
servers over stdio and imports their tools as ordinary solx `Command`
actions — no changes to the core `solx-core` repository are required.

## Naming

This package is called `solx-mcp-actions` (binary `solx-mcp-actions`) to
avoid colliding with `solx-mcp`, the MCP **server** in `solx-core` that
exposes solx itself as an MCP server. The MCP ecosystem naming convention
for imported tool actions (`mcp-<server>-<tool>`, e.g.
`mcp-filesystem-read-file`) is unchanged — those names come from the MCP
ecosystem, not from this package.

## What it provides

Two solx `Command` actions (a small Rust CLI, no `tokio`-only async
needed — this package's heavy lifting is done by the `rmcp` crate):

- `import` — connect to a server configured in
  `mcp-servers.json`, list its tools, and create one solx `Command`
  action per tool (e.g. `mcp-filesystem-read-file`).
- `remove` — delete a server's previously imported
  actions and types, and stop the background that was serving them.

Plus the command key every dynamically-imported tool action reuses:

- `solx-mcp-actions-serve` (runs `solx-mcp-actions serve`) — the command
  key every generated per-tool action points at. You never call this
  directly; it's what makes the generated actions work.

### How per-tool actions work

Every generated action is a solx **background** action: it names a
background in its `action_config.background`, and solx-core runs it in a
long-lived process that serves many calls over stdin and stdout instead of
one process per call.

There is **one background per MCP server**, so every tool imported from a
server shares it — and therefore one MCP connection to that server, for as
long as the host runs:

```
namespace = solx-mcp-actions
name      = <the server's key in mcp-servers.json>
```

The background's name is the server key, and its operation says which tool
to call: a generated action for `new_page` carries the operation
`tool:new_page`, so the background knows what to pass to `tools/call`. The
prefix keeps a tool named `start` or `stop` from being mistaken for one of
solx-core's own reserved operations.

solx-core starts the background on the first tool call and stops it when
the server's tools are removed or the host exits. If the MCP server dies
underneath it, the background starts a new one and retries the call once.

Each server also gets two control actions, next to its tools:

| Action | What it does |
|---|---|
| `background-start` | Start the background, so it is ready before the first tool call |
| `background-stop` | Stop it; the next tool call starts it again |

Run `/builtin/background/list` to see what is running.

### Migrating from the one-shot model

Actions imported before the background existed point at
`solx-mcp-actions-invoke` and start an MCP server per call. Re-running
`import` for a server rewrites them in place.

One wrinkle: a package install locks every action under the package's own
path, and the imported tools live under it — so after a reinstall they are
locked, and `import` (running with its own token, as content) cannot
change them. Unlock them first, as an operator:

```bash
solx action unlock /packages/solx-mcp-actions/<server>/<tool-action>
```

`import` says so in its error when it hits one.

### Why a Command action instead of a new `action_type`

solx-core's dispatcher understands `wasm`, `webhook`, `command`,
`internal`, and `script`. Adding a native `"Mcp"` type would require
changes to `solx-actions/src/lib.rs`'s dispatch match. Instead, this
package reuses `Command`: `solx-mcp-actions` is a CLI that speaks MCP (via
the `rmcp` crate) and is invoked with a JSON payload on stdin, exactly
like `solx-omniparse` and `solx-quickjs`.

### Key difference from sol-mcp (sol ecosystem)

solx-core's `run_command` (in `solx-actions/src/exec.rs`) passes
parameters as JSON on **stdin**, not via the `SOL_PARAMS` env var. This
is the primary change when porting from sol-mcp to this package.

## Build

```bash
cd solx-mcp-actions
cargo build --release
```

The `build.rs` compiles into a package-local `.build-target/` and stages
the binary into `bin/`, same as `solx-omniparse`. Skip auto-staging with
`SOLX_MCP_ACTIONS_SKIP_AUTOBUILD=1`.

Staged binary path:
`solx-mcp-actions/bin/solx-mcp-actions.exe` (Windows) /
`solx-mcp-actions/bin/solx-mcp-actions` (Linux/macOS).

## Install package

```bash
solx install-package ./solx-packages/solx-mcp-actions
```

This registers `import`, `remove`, and their parameter/result types, and
grants the `solx-mcp-actions-serve` command key the imported tools run
under. It does **not** run any MCP server or create any per-tool actions —
those only happen when you actually run `import` (see below).

## Configuring MCP servers

Servers are configured in `mcp-servers.json` in the solx-mcp-actions
package directory. It ships with a `firefox` entry for `solx-firefox`; add
others alongside it:

```json
{
  "servers": {
    "git": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-git", "--repository", "C:/path/to/repo"],
      "env": {}
    }
  }
}
```

How `command` runs:

- **A path** (it contains `/` or `\`) is resolved against this package
  directory unless absolute, trying it with `.exe` appended on Windows if it
  doesn't exist as written. If it names an executable, that file is spawned
  directly with `args`, no shell involved, so paths with spaces are safe.
  The shipped `firefox` entry uses this: `"command": "../solx-firefox/bin/mcp-server"`
  finds the sibling package's bundled server in a repo checkout and in a zip
  install alike (both put packages side by side).
- **Anything else** (a bare name like `npx`, or a `.cmd`/`.bat`) has
  `command`/`args` joined and run through the platform shell (`cmd.exe /C`
  on Windows, `sh -c` on Linux/macOS), since on Windows `npx` is a `.cmd`
  shim that can't be exec'd directly.

**Security note:** any `env` values here (API tokens, etc.) are stored in
**plaintext**. A natural future improvement is resolving `"$SECRET:NAME"`
placeholders through solx's secret store — out of scope for this phase.

### Filtering which tools get imported

Most real MCP servers expose far more tools than are actually useful once
imported as solx actions — a server can add `tool_filter` to keep only the
ones you want:

```json
{
  "servers": {
    "firefox": {
      "command": "../solx-firefox/bin/mcp-server",
      "args": ["--connect-existing", "--marionette-port", "2828", "--tool-preset", "developer"],
      "env": {},
      "tool_filter": {
        "include": ["navigate_page", "take_snapshot", "screenshot_*"],
        "exclude": ["screenshot_by_uid"]
      }
    }
  }
}
```

Patterns match the tool's raw MCP name (e.g. `read_file`, not
`mcp-<server>-read-file`) and support at most one `*` wildcard. `include`
is an allowlist — when present, only matching tools are considered at
all; `exclude` is then applied on top, so a tool can be pulled in by
`include` and still dropped by `exclude`. Omit `tool_filter` entirely to
import everything the server exposes.

This default filter applies on every import of that server. To override
it for one call without editing `mcp-servers.json` — e.g. to test a
narrower set before committing to it — pass `include`/`exclude` directly
in the import params (CLI: `--include`/`--exclude`, repeatable); a
per-call filter **replaces** the server's default rather than merging
with it.

Re-running import after narrowing a filter (in config or via the
per-call override) removes the actions/types for any previously-imported
tool that no longer passes — reported as `tools_pruned` in the result —
so tightening the filter actually enforces it instead of just skipping
new tools going forward.

## Importing a server

```bash
echo '{"server":"git"}' | solx exec /packages/solx-mcp-actions/import
```

Or, for fast local iteration without touching the database:

```bash
bin/solx-mcp-actions.exe import git --dry-run
```

Successful import prints
`{"server", "tools_imported": [...], "tools_pruned": [...], "errors": [...],
"background": {...}}` and writes a manifest to
`<package>/tools/<server>/manifest.json` — this manifest (not a
name-prefix scan) is what `remove` uses to know exactly which entities it
created. Re-running import is safe (idempotent up) if you want to refresh a
server's tool list after it changes; `tools_pruned` lists any
previously-imported tool removed because it no longer passes the current
filter. The `background` object names the background the server's tools now
share.

Optionally pass `"permission_name"` to stamp a solx `Permission` onto
every generated action, scoping which callers may invoke the imported
tools.

## Using an imported tool

Generated actions are named `mcp-<server>-<tool>` (e.g.
`mcp-git-git-log`) and behave like any other solx action:

```bash
echo '{"repo_path":"C:/path/to/repo"}' | solx exec mcp-git-git-log
```

The first call for a server starts its background (and the MCP server
behind it); later calls reuse it. To have it ready before the first call:

```bash
solx exec /packages/solx-mcp-actions/<server>/background-start
```

## Removing a server

```bash
echo '{"server":"git"}' | solx exec /packages/solx-mcp-actions/remove
```

This stops the server's background — closing its connection to the MCP
server — and deletes the server's imported actions and types.

Run this for **every** configured server before uninstalling the package
— otherwise the generated actions are left pointing at a command that
still resolves fine but is orphaned from the package's perspective.

## Uninstall package

```bash
solx uninstall-package solx-mcp-actions
```

## Known limitations (phase 1)

- A tool call and a one-shot `invoke` of the same server cannot run at
  the same time: they would fight over the MCP server's single session.
  New imports all use the background, so this only shows up with an
  action imported before the background existed.
- The MCP server is started when the background starts, so a change to
  `mcp-servers.json` (a different command, a new argument) only takes
  effect after the background is restarted — stop it and call a tool
  again.
- Only `tools/list` + `tools/call` are supported (no MCP resources or
  prompts).
- `mcp-servers.json` stores server env vars in plaintext.

## Recommended test servers

| Server | Install command | Good tools to test |
|---|---|---|
| **Filesystem** | `npx -y @modelcontextprotocol/server-filesystem /path/to/dir` | `read_file`, `write_file`, `list_directory` |
| **Memory** | `npx -y @modelcontextprotocol/server-memory` | `create_entities`, `search_nodes` |
| **Fetch** | `npx -y @modelcontextprotocol/server-fetch` | `fetch` (HTTP GET → markdown) |
| **Git** | `npx -y @modelcontextprotocol/server-git --repository /path/to/repo` | `git_log`, `git_diff`, `git_status` |
| **SQLite** | `npx -y @modelcontextprotocol/server-sqlite --db-path /path/to/db.sqlite` | `read_query`, `write_query` |
| **Puppeteer** | `npx -y @modelcontextprotocol/server-puppeteer` | `puppeteer_navigate`, `puppeteer_screenshot` |