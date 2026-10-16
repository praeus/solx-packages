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
dedicated-profile Firefox instance, and to ship the MCP server itself as a
compiled binary (`bin/mcp-server`). `solx-mcp-actions`'s own `mcp-servers.json`
has a `firefox` entry that runs that binary with `--connect-existing`, so every
`solx-mcp-actions invoke` call talks to the *same* long-lived browser.

## Key difference from sol-firefox (sol ecosystem)

solx-core's `run_command` (in `solx-actions/src/exec.rs`) passes parameters
as JSON on **stdin**, not via the `SOL_PARAMS` env var. There is no
`command_actions` registry in solx — `fn_name` is the literal shell command,
and `action_config.cwd` is set on the action itself.

The setup/teardown orchestrators are `Script`-type actions
(`mcp-setup.solx` / `mcp-teardown.solx`, uploaded with `save file`) rather than `Actions`-type (ActionScript JSON
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

### The MCP server binary

`mcp/` compiles `@mozilla/firefox-devtools-mcp` (pinned in
`mcp/package.json`) into a standalone `bin/mcp-server[.exe]` with
`bun build --compile`, then smoke-tests it (version, MCP handshake, every tool
in `solx-mcp-actions`' `tool_filter`, clean stdout, no `.env`/`bunfig.toml`
loading from the cwd):

```bash
cd solx-firefox/mcp
bun install --frozen-lockfile
bun run build
```

`scripts/build-all.sh` and CI run this for you (it is declared under
`extra_binaries` in `solx-package.json`). Bun is needed only to build, not to
run. The binary is about 85 MB. To bump the upstream version, change the
pin, `bun install`, rebuild, and run the checklist in
[docs/bundled-mcp-server.md](../docs/bundled-mcp-server.md).

## Prerequisites

- Firefox 100+, installed at a standard location (auto-detected) or pass
  `--firefox-path`.
- `geckodriver`. The MCP server looks for it on `PATH`, then in
  `~/.cache/selenium/geckodriver/`, and otherwise downloads it into
  `GECKODRIVER_CACHE_DIR` (default: the OS temp dir) on first use, which
  needs network access. Offline, put geckodriver on `PATH` or pre-stage it
  in `GECKODRIVER_CACHE_DIR`. (`GECKODRIVER_PATH` is not consulted.)

## Security

This package always launches Firefox with a **dedicated profile** under its
own package directory — never your regular Firefox profile. This matches
Mozilla's own guidance for `firefox-devtools-mcp`: whatever MCP tools can
reach through this browser (page content, and anything the page itself can
reach), an agent can reach too, so don't point it at a profile with real
logins/cookies you care about.

## Install package

Build the binary first (it stages into `bin/`), then run:

```bash
solx install-package ./solx-packages/solx-firefox
```

The `command_actions` in `solx-package.json` use `"cwd": "bin"`,
which `install-package` resolves against this package's directory, so there is no path to edit.

This registers `firefox-start`, `firefox-stop`, `firefox-mcp-setup`,
`firefox-mcp-teardown`, and uploads the two `.solx` script files to the
file store.

## The solx-mcp-actions entry

`solx-mcp-actions` ships with this `firefox` entry in its `mcp-servers.json`
(tool filter omitted here):

```json
"firefox": {
  "command": "../solx-firefox/bin/mcp-server",
  "args": ["--connect-existing", "--marionette-port", "2828", "--tool-preset", "developer"],
  "env": {}
}
```

The relative `command` resolves against the `solx-mcp-actions` package
directory, so it works whenever the two packages sit side by side: in this
repo, and when both are installed from zips. If you install them elsewhere,
change `command` to the absolute path of `bin/mcp-server`.

## Usage flow

1. Install `solx-mcp-actions` and `solx-firefox`.
2. `solx exec /packages/solx-firefox/firefox-mcp-setup` — starts the
   managed Firefox and imports its DevTools MCP tools as solx actions
   (`mcp-firefox-*`) in one call.
3. Use the generated `mcp-firefox-*` actions (navigate, click, screenshot,
   etc.) — they all share the one persistent browser session.
4. `solx exec /packages/solx-firefox/firefox-mcp-teardown` when done.

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
