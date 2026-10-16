# Bundling the Firefox DevTools MCP server as a standalone binary

Goal: run the Firefox DevTools MCP server from a compiled native binary shipped
inside the `solx-firefox` package, so nothing at run time needs Node, `npx`, or
a registry round-trip.

**Status: implemented.** `solx-firefox/mcp/`, `extra_binaries` in
`scripts/package.mjs` / `build-all.sh` / `release.yml`, and direct exec with
relative `command` resolution in `solx-mcp-actions`. Verified on Windows x64:
checklist items 1–8 except 8 (run through a live solx server); macOS and Linux
are covered only by CI's build-time smoke test.

## 1. Before

`solx-mcp-actions` spawns the server from
[`solx-mcp-actions/mcp-servers.json`](../solx-mcp-actions/mcp-servers.json):

```json
"firefox": {
  "command": "npx",
  "args": ["-y", "@mozilla/firefox-devtools-mcp@latest", "--connect-existing",
           "--marionette-port", "2828", "--tool-preset", "developer"],
  "env": {}
}
```

What that costs:

- **Node is a hard prerequisite.** `solx-firefox`'s README requires Node
  ≥ 20.19.0. The first run pulls 140 packages / 81.3 MB plus a geckodriver
  download, and `@latest` means any run can pick up a new upstream version.
- **A shell is always in the middle.** `npx` on Windows is a `.cmd` shim, so
  [`mcp_client.rs`](../solx-mcp-actions/src/mcp_client.rs) runs every spawn
  through `cmd.exe /C` (or `sh -c`).

`solx-firefox` is the natural owner of the replacement: it already manages the
Firefox process the server attaches to, and it is already built per platform.

## 2. Verified findings

Tested on Windows x64, Bun 1.4.2, `@mozilla/firefox-devtools-mcp@0.10.6`
(which pins `selenium-webdriver@4.36.0`, `geckodriver@6.0.2`).

| Finding | Detail |
|---|---|
| Package shape | One bundled ESM CLI, `dist/index.js`, plus `dist/snapshot.injected.global.js`. Configured by CLI flags; there is no server class to embed. |
| Plain `--compile` works | `bun build --compile dist/index.js` bundles 262 modules and the binary boots (`--version` → `0.10.6`). **No plugin is needed**: selenium-webdriver 4.36.0 already loads its atoms with static `require('./atoms/…')` calls. (Bun has no `--plugin` CLI flag anyway; plugins only exist in the `Bun.build()` JS API.) |
| Handshake | `initialize` → `serverInfo`, and `tools/list` → 49 tools under `--tool-preset developer`, from an unrelated cwd. All 22 names in the existing `tool_filter.include` are in that preset. |
| `take_snapshot` needs `--asset` | The server finds `snapshot.injected.global.js` next to its own `import.meta.url`. `--asset <dist>/snapshot.injected.global.js` embeds it. Verified: against a headless `solx-firefox start`, run from an unrelated cwd, `new_page` + `take_snapshot` returns the page's elements. |
| **NODE_ENV must be baked in at build time** | The bundle loads `dotenv` unless `process.env.NODE_ENV === "production"`, and dotenv prints a banner **to stdout**, which is the MCP protocol channel. `bun build` inlines `process.env.NODE_ENV` as `"development"`, so setting `NODE_ENV` in the spawn env has **no effect**. Verified fix: `--define 'process.env.NODE_ENV="production"'`. With it, a `.env` planted in the cwd is not loaded and stdout is JSON-RPC only. |
| **Security: cwd autoload** | Bun standalone binaries autoload `.env` and `bunfig.toml` from the cwd by default; a `bunfig.toml` `preload` runs arbitrary code inside the binary. The server is spawned with a caller-chosen cwd, so build with `--no-compile-autoload-dotenv --no-compile-autoload-bunfig`. |
| `--firefox-path` is inert under `--connect-existing` | The server only attaches to the Marionette port; `solx-firefox start` launches Firefox. No browser needs to be bundled. |
| geckodriver | Still needed under `--connect-existing`. Lookup order: `PATH` → `~/.cache/selenium/geckodriver/*/*/` → download via the `geckodriver` package into `GECKODRIVER_CACHE_DIR` (default `os.tmpdir()`). **`GECKODRIVER_PATH` is not consulted.** Same behavior as with `npx` today. |
| Size | ~84 MiB (windows-x64), ~80 MiB (linux-x64). |

## 3. Design

```
solx-firefox/
  src/                     # unchanged: start/stop/status
  mcp/                     # new
    package.json           #   pins @mozilla/firefox-devtools-mcp exactly; "build" script
    bun.lock               #   committed
    build.mjs              #   compile + stage into ../bin, fail loudly
  bin/
    solx-firefox(.exe)     # unchanged
    mcp-server(.exe)       # new
```

`solx-mcp-actions` spawns it directly, and finds it relative to its own
package directory (§4.3), so the shipped `mcp-servers.json` works with no
per-machine edits.

## 4. Implementation

### 4.1 Build: `solx-firefox/mcp/build.mjs`

Resolve the package's `dist/` through `node_modules`, then run:

```
bun build --compile <dist>/index.js \
  --outfile ../bin/mcp-server<.exe> \
  --asset <dist>/snapshot.injected.global.js \
  --define 'process.env.NODE_ENV="production"' \
  --no-compile-autoload-dotenv \
  --no-compile-autoload-bunfig
```

- Fail (non-zero exit) if `index.js` or the snapshot asset is missing, or if
  the built binary doesn't print the pinned version for `--version`.
- Pin the exact upstream version (no `^`, never `@latest`). Every bump re-runs
  §6.
- Host platform only. `--target` cross-builds work (linux-x64 from Windows was
  built) but CI builds natively on each runner.
- Do not hook this into `build.rs`: its staging only warns on failure, which
  is how a binary silently goes missing.

### 4.2 Packaging and builds

The packaging scripts only know about `command_actions` programs, so an extra
binary in `bin/` would be silently dropped. Declare it in
`solx-firefox/solx-package.json`, with its build command, so the scripts stay
manifest-driven (`release.yml`'s stated rule: a new package builds without
editing the workflow):

```json
"extra_binaries": {
  "mcp-server": { "build": "bun install --frozen-lockfile && bun run build", "cwd": "mcp", "needs": "bun" }
}
```

- **`scripts/package.mjs`**: in the `rust` branch, for each key copy
  `bin/<key><exe>` into the zip with mode 0o755; push to `missing` if absent
  (the existing "build output missing" error). `plan` adds `needs_bun: true` to
  matrix entries for packages that declare it.
- **`release.yml`**: for `matrix.needs_bun`, add `oven-sh/setup-bun` (pinned
  Bun version) and a step that runs each `extra_binaries` build. Placed after
  the cargo build, before `Zip`. No matrix changes; `solx-firefox` is already
  `rust` on all three platforms.
- **`scripts/build-all.sh`**: run the same `extra_binaries` builds after a
  rust package's cargo build, and add each `bin/<key>` to the artifact check
  via `check_rewritten`.

### 4.3 `solx-mcp-actions`: spawn the binary directly

Two changes in `solx-mcp-actions`, plus the config update:

1. **Resolve relative commands against the package home.** A `command` that
   contains a path separator and is relative is joined onto `resolve_home()`.
   If the file doesn't exist, try it again with `std::env::consts::EXE_SUFFIX`
   appended, so one config works on every platform.
2. **Direct exec.** When the resolved command is an existing file that is not
   `.cmd`/`.bat`, spawn it with `Command::new(path).args(args)` and skip
   `cmd.exe /C` / `sh -c`. Keep the shell path for everything else (`npx`,
   etc.). This is required, not optional: zip installs land in
   `%APPDATA%\…\packages`, and a username with a space produces a quoted
   command line, which `cmd /C` plus Rust's argument escaping mangle.

Then the shipped entry in `solx-mcp-actions/mcp-servers.json` becomes:

```json
"firefox": {
  "command": "../solx-firefox/bin/mcp-server",
  "args": ["--connect-existing", "--marionette-port", "2828", "--tool-preset", "developer"],
  "env": {},
  "tool_filter": { "include": [ "…unchanged…" ] }
}
```

This relies on the two packages being siblings, which holds for both a repo
checkout and zip installs (both unpack into `packages_directory`). A user who
installs them elsewhere can still put an absolute path in `command`.

Why not have `install.solx` write the entry: `mcp-servers.json` is a tracked
file inside `solx-mcp-actions`, and reinstalling that package from a zip
replaces its directory, so any install-time or hand edit would be lost.
`install.solx` also has no way to learn its own package directory.

## 5. Documentation updates

- `solx-firefox/README.md`: drop the Node prerequisite and the `npx … --help`
  pre-warm, and replace the "required addition to mcp-servers.json" section
  (the entry now ships). Document Bun as a build-only dependency, the binary
  size, and geckodriver lookup (offline: put geckodriver on `PATH` or
  pre-stage it in `GECKODRIVER_CACHE_DIR`).
- `solx-mcp-actions/README.md` and `docs/mcp-actions.md`: update the `firefox`
  example; document relative-`command` resolution and direct exec. The `npx`
  guidance for other servers stands.
- `HOWTO.md` §9: add `extra_binaries` to the packaging rules.

## 6. Verification checklist

Run after any build change and every upstream bump:

1. **Boots:** `bin/mcp-server --version` prints the pinned version.
2. **Handshake:** `initialize` returns `serverInfo`, and `tools/list` returns
   49 tools under `--tool-preset developer`, including all 22 filtered names.
3. **Clean stdout:** with a `.env` in the cwd, every stdout line parses as
   JSON-RPC and stderr has no `Loaded .env`.
4. **Snapshot:** with `solx-firefox start` running, `new_page` then
   `take_snapshot` returns real DOM text (the `--asset` regression test).
5. **cwd-independence:** repeat 2 and 4 from an unrelated directory.
6. **Hardening:** a `bunfig.toml` with `preload = ["./evil.cjs"]` in the cwd
   does not run.
7. **Spaces:** install to a path containing a space and repeat 2 through
   `solx-mcp-actions`.
8. **Integrated:** `solx exec /packages/solx-firefox/firefox-mcp-setup`
   imports the tools, and an `mcp-firefox-*` call succeeds.

`mcp/build.mjs` runs 1–3 and 6 (from a scratch cwd, which also covers the
no-Firefox half of 5) after every host build, so CI and `build-all.sh` fail
on a regression. 4, 7 and 8 are manual.

## 7. Risks

| Risk | Mitigation |
|---|---|
| An upstream bump changes the asset name/layout or reintroduces a dynamic require. | Exact pin; `build.mjs` fails on a missing asset; §6 on every bump. |
| ~84 MB per platform zip. | Already per-platform; note it in the README. |
| geckodriver is downloaded at first use. | Unchanged from today; document the offline options (§5). |
| Bun version drift changes `--compile` behavior (e.g. the `NODE_ENV` inlining). | Pin Bun in CI; §6 item 3 catches stdout regressions. |

## 8. Effort

| Work | Estimate |
|---|---|
| `mcp/` project + `build.mjs`, verified on Windows (§6 1–6) | ½ day |
| `solx-mcp-actions` relative resolution + direct exec + config | ½ day |
| `extra_binaries` in `package.mjs`, `build-all.sh`, `release.yml`, green on all three runners | 1 day |
| macOS/Linux end to end | ½–1 day |
| Docs | ½ day |
| **Total** | **~3–3.5 days** |

## 9. Out of scope

- **Tauri sidecar.** `solx-firefox` is a standalone Rust CLI; the desktop shell
  does not launch this server.
- **Bundled/portable Firefox.** Nothing would use it: the server only attaches
  via `--connect-existing`. Mozilla also ships only installers for Windows
  (NSIS/MSI) and macOS (dmg/pkg), and a portable tarball only for Linux, so
  provisioning a browser would be a separate project with its own update
  story.
