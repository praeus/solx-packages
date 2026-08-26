# solx-firefox

Manages one persistent, dedicated-profile Firefox instance with Marionette
enabled, so `solx-mcp-actions`'s Firefox DevTools MCP integration can share a single
browser session across tool calls instead of spawning (and tearing down) a
fresh Firefox for every single tool invocation.

## Why this package exists

`solx-mcp-actions` normally spawns a fresh MCP server subprocess for every tool call
and tears it down afterward — correct for stateless servers (filesystem,
fetch), but wrong for [Mozilla's `firefox-devtools-mcp`](https://github.com/mozilla/firefox-devtools-mcp)
(npm `@mozilla/firefox-devtools-mcp`): browser automation is inherently
stateful (navigate, then click, then screenshot — all against the *same*
page). By default that server launches its own throwaway Firefox per
connection, so under solx-mcp-actions's normal model every tool call would get a
brand new, blank browser with no memory of the previous call.

The fix: `firefox-devtools-mcp` supports `--connect-existing`, attaching to
an already-running Firefox with Marionette enabled instead of launching its
own. This package's only job is to start/stop that one persistent,
dedicated-profile Firefox instance; `solx-mcp-actions`'s own `mcp-servers.json` then
gets a `firefox` entry using `--connect-existing`, so every `solx-mcp-actions invoke`
call talks to the *same* long-lived browser.

## Key difference from sol-firefox (sol ecosystem)

solx-core's `run_command` (in `solx-actions/src/exec.rs`) passes parameters
as JSON on **stdin**, not via the `SOL_PARAMS` env var. There is no
`command_actions` registry in solx — `fn_name` is the literal shell command,
and `action_config.cwd` is set on the action itself.

The setup/teardown orchestrators are `Script`-type actions (`.solx` files
uploaded via `file_put`) rather than `Actions`-type (ActionScript JSON
artifacts). This avoids the artifact signing requirement that `Actions`-type
actions have in solx.

## What it provides

Two `Command` actions (a small Rust CLI, no `tokio`/async runtime needed —
this package does no MCP protocol work itself, that's `solx-mcp-actions`'s job):

- `firefox-start` — launch (or reuse) the dedicated-profile Firefox with
  Marionette enabled. Idempotent: if Firefox is already running under this
  package's management, returns `already_running` rather than spawning a
  second instance (which would fail anyway — nothing else can bind the same
  Marionette port).
- `firefox-stop` — stop the managed Firefox instance (kills the whole
  process tree, including content/GPU child processes). Idempotent.

Plus two `Script`-type orchestrator actions that chain a `solx-firefox`
step with a `solx-mcp-actions` step in one call:

- `firefox-mcp-setup` — `firefox-start` then `/packages/solx-mcp-actions/import {"server":"firefox"}`.
- `firefox-mcp-teardown` — `/packages/solx-mcp-actions/remove {"server":"firefox"}` then `firefox-stop`.

### Why a Command action can't just run `firefox -marionette` directly

solx's `Command` action dispatch (`run_command`) is fully blocking — it
waits for the process to exit and reads its stdout/stderr to EOF. A naive
"start firefox" action would hang until the user closes the browser. So
`solx-firefox start` is itself a tiny launcher: it spawns Firefox with
`Stdio::null()` on all three streams (this is the actual fix — an inherited
pipe handle held open by the Firefox child is what would cause the hang,
not just a cosmetic detail) plus OS-level process-group detachment
(`CommandExt::process_group(0)` on Unix, `CommandExt::creation_flags
(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)` on Windows — both stable
`std` APIs, no extra dependency), polls the Marionette port until ready,
records `{pid, port}` in a `state.json`, and exits — leaving Firefox running
independently. `firefox-stop` reads that PID back and kills the whole tree.

## Build

```bash
cd solx-firefox
cargo build --release
```

Mirrors `solx-mcp-actions`/`solx-omniparse`'s `build.rs` staging pattern: compiles
into a package-local `.build-target/` and stages the binary into `bin/`.
Skip auto-staging with `SOLX_FIREFOX_SKIP_AUTOBUILD=1`.

## Prerequisites

- Firefox 100+, installed at a standard location (auto-detected) or pass
  `--firefox-path`.
- Node.js ≥ 20.19.0 (for `npx @mozilla/firefox-devtools-mcp`, run via
  `solx-mcp-actions`, not this package).
- `geckodriver` is auto-managed by `@mozilla/firefox-devtools-mcp` — no
  separate install needed. The *first* invocation may need network access to
  download it; consider pre-warming with
  `npx -y @mozilla/firefox-devtools-mcp@latest --help` once during setup so
  the first real `/packages/solx-mcp-actions/import` call for `firefox` isn't slowed by (or
  fails on) that download.

## Security

This package always launches Firefox with a **dedicated profile** under its
own package directory — never your regular Firefox profile. This matches
Mozilla's own guidance for `firefox-devtools-mcp`: whatever MCP tools can
reach through this browser (page content, and anything the page itself can
reach), an agent can reach too, so don't point it at a profile with real
logins/cookies you care about.

## Install package

1. Edit `install.solx` and replace `REPLACE_WITH_ABSOLUTE_PATH_TO` with the
   actual absolute path to the solx-firefox package directory.
2. Run:
   ```bash
   solx install-package ./solx-packages/solx-firefox
   ```

This registers `firefox-start`, `firefox-stop`, `firefox-mcp-setup`,
`firefox-mcp-teardown`, and uploads the two `.solx` script files to the
file store.

## Required addition to solx-mcp-actions's mcp-servers.json

This file belongs to the sibling `solx-mcp-actions` package — add a `firefox` entry
using `--connect-existing` so tool calls attach to the instance this package
manages rather than each spawning a fresh Firefox:

```json
{
  "servers": {
    "firefox": {
      "command": "npx",
      "args": ["-y", "@mozilla/firefox-devtools-mcp@latest", "--connect-existing", "--marionette-port", "2828"],
      "env": {}
    }
  }
}
```

## Usage flow

1. Install `solx-mcp-actions` and `solx-firefox`.
2. Add the `mcp-servers.json` `firefox` entry (see above).
3. `solx exec /packages/solx-firefox/firefox-mcp-setup` — starts the
   managed Firefox and imports its DevTools MCP tools as solx actions
   (`mcp-firefox-*`) in one call.
4. Use the generated `mcp-firefox-*` actions (navigate, click, screenshot,
   etc.) — they all share the one persistent browser session.
5. `solx exec /packages/solx-firefox/firefox-mcp-teardown` when done.

## Troubleshooting

- **Port 2828 already in use**: `firefox-start` checks the Marionette port
  first and will never spawn a second instance on top of one it doesn't
  recognize — it reports `{"status":"already_running_external"}` rather than
  guessing. If that's not actually a Firefox instance you want, stop it
  yourself first (or pass `--marionette-port` to use a different one, and
  update the `mcp-servers.json` entry to match).
- **Headless mode**: `firefox-start --headless` (or `{"headless": true}` in
  the action's params) runs without a visible window — useful for
  automation-only use, less useful if you want to visually follow along.
- **Firefox path not found**: pass `--firefox-path` (or
  `{"firefox_path": "..."}`) if it's not at a standard per-OS location.
- **First-run network dependency**: see the geckodriver note under
  Prerequisites — an offline first run of the `firefox` MCP server will
  fail while it tries to download geckodriver.

## Uninstall package

Run `solx exec /packages/solx-firefox/firefox-mcp-teardown` first so the
imported `mcp-firefox-*` actions and their types are cleaned up and Firefox
is stopped, then:

```bash
solx uninstall-package solx-firefox
```
