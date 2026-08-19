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

- `solx-mcp-actions-import` — connect to a server configured in
  `mcp-servers.json`, list its tools, and create one solx `Command`
  action per tool (e.g. `mcp-filesystem-read-file`).
- `solx-mcp-actions-remove` — delete a server's previously imported
  actions and types.

Plus one shared internal command key that every dynamically-imported
tool action reuses:

- `solx-mcp-actions invoke` — the command every generated per-tool action
  points at. You never call this directly; it's what makes the generated
  actions work.

### How per-tool actions work

Every generated action shares a single command (`solx-mcp-actions invoke`)
and carries its identity via `action_config.cwd`: each imported tool gets
its own directory containing a `tool.json` descriptor
(`{"server": ..., "tool": ...}`), and the generated action's `cwd` points
there. When solx-core invokes the action, `solx-mcp-actions invoke`
reads `./tool.json` (relative to its own process cwd) to find out which
MCP server/tool to call, and reads the caller's actual arguments from
**stdin**.

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

This registers `solx-mcp-actions-import`, `solx-mcp-actions-remove`, and
their parameter/result types. It does **not** run any MCP server or
create any per-tool actions — those only happen when you actually run
`solx-mcp-actions-import` (see below).

## Configuring MCP servers

Create `mcp-servers.json` in the solx-mcp-actions package directory:

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

`command`/`args` are joined and run through the platform shell
(`cmd.exe /C` on Windows, `sh -c` on Linux/macOS), which matters in
practice because many real MCP servers are launched via `npx`, and on
Windows `npx` is a `.cmd` shim that can't be exec'd directly without a
shell.

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
      "command": "npx",
      "args": ["-y", "@mozilla/firefox-devtools-mcp@latest"],
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
echo '{"server":"git"}' | solx exec /packages/solx-mcp-actions/solx-mcp-actions-import
```

Or, for fast local iteration without touching the database:

```bash
bin/solx-mcp-actions.exe import git --dry-run
```

Successful import prints
`{"server", "tools_imported": [...], "tools_pruned": [...], "errors": [...]}`
and writes a manifest to `<package>/tools/<server>/manifest.json` — this
manifest (not a name-prefix scan) is what `solx-mcp-actions-remove` uses
to know exactly which entities it created. Re-running import is safe
(idempotent up) if you want to refresh a server's tool list after it
changes; `tools_pruned` lists any previously-imported tool removed
because it no longer passes the current filter.

Optionally pass `"permission_name"` to stamp a solx `Permission` onto
every generated action, scoping which callers may invoke the imported
tools.

## Using an imported tool

Generated actions are named `mcp-<server>-<tool>` (e.g.
`mcp-git-git-log`) and behave like any other solx action:

```bash
echo '{"repo_path":"C:/path/to/repo"}' | solx exec mcp-git-git-log
```

## Removing a server

```bash
echo '{"server":"git"}' | solx exec /packages/solx-mcp-actions/solx-mcp-actions-remove
```

Run this for **every** configured server before uninstalling the package
— otherwise the generated actions are left pointing at a command that
still resolves fine but is orphaned from the package's perspective.

## Uninstall package

```bash
solx uninstall-package solx-mcp-actions
```

## Known limitations (phase 1)

- Every invocation — `import` or `invoke` — spawns a fresh MCP server
  process and tears it down afterward. There's no persistent connection
  pooling, so concurrent calls to the same tool each pay full server
  cold-start cost independently.
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