# Plan: MCP actions on solx background commands

Status: **agreed design**, not yet implemented.

Moves the **tool-call path** of `solx-mcp-actions` from "one process per tool
call" to solx-core's **background commands**: one long-lived process per MCP
server, shared by every tool imported from it, holding a single continuous
connection to that server.

The reference implementation is `solx-graph`, the only package that uses the
feature today. The core design is in solx-core's
`core/docs/background-commands-plan.md`.

---

## 1. What changes, and what deliberately does not

| | Today | After |
|---|---|---|
| `solx-mcp-actions` tool calls | one `solx-mcp-actions` process + one MCP server process per call | **one background per MCP server**, one MCP connection for its lifetime |
| `solx-mcp-actions` `import` / `remove` | one-shot Command actions | **unchanged** — still one-shot |
| `solx-firefox` | one-shot `firefox-start` / `firefox-stop`, Script setup/teardown | **unchanged** — Firefox stays a detached process |
| solx-core | background commands implemented and tested | **no changes** |

Two boundaries are worth stating explicitly, because one of them is *forced*
rather than chosen:

**`import` and `remove` cannot be backgrounds.** They create and delete
action and type rows over solx's REST API, which needs an action token. A
background is issued **no token at all** (`start_background` never calls
`issue_action_token` — `core/solx-actions/src/background/lifecycle.rs:32-128`).
A background can only reach the host through the `exec` / `console.print`
host ops, and those are for *running actions*, not for writing rows. So
`import` and `remove` stay one-shot Command actions, and they are the only
two actions in the package that need scopes.

**`solx-firefox` does not become a background package.** Its job is to start
and stop one persistent Firefox, which it already does: `firefox-start`
spawns a detached Firefox — `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` on
Windows, `process_group(0)` on POSIX (`solx-firefox/src/process.rs`) —
deliberately outside any job object, so it is not tied to the host's
lifetime. It polls the Marionette port, records `state.json` and exits. That
is a start/stop operation, not a serve-many-calls operation, so the
background machinery buys it nothing and would cost it the documented
"Firefox keeps running independently" property. What `solx-firefox` needs
from this work is only the **scope declarations** its setup/teardown scripts
need, plus the teardown ordering (§6.7).

---

## 2. The problem, measured

The bundled Firefox DevTools MCP server is an 84 MB bun-compiled binary.
Today every tool call spawns it, handshakes, calls one tool, and tears it
down. Measured on this machine, against a running Firefox:

| | today, per call | with a background |
|---|---|---|
| spawn + MCP handshake | **~86 ms** | 0 (once) |
| first tool call (Firefox DevTools connect) | ~41 ms | — |
| subsequent tool call | — | **~2 ms** |
| **overhead per tool call** | **~125 ms** | **~2 ms** |
| working set / private memory | 58 MB / 141 MB, allocated and freed per call | steady state |
| processes spawned per call | 2 (one an 84 MB binary) | 0 |

The latency is the smaller half. The reason to do this is that **any MCP
server that holds state between calls loses it today**. Firefox only works
because `solx-firefox` exists to keep a detached browser alive — a
workaround. Every other stateful MCP server (a websocket, a session, a
loaded model) is simply unusable under the one-shot model. The background is
what makes MCP servers work the way MCP servers are supposed to.

---

## 3. Where things stand today

### 3.1 `solx-mcp-actions`

| Piece | Where |
|---|---|
| `import` — connect, `tools/list`, create one action per tool | `src/main.rs:158` (`run_import`), `src/main.rs:281` (`import_one_tool`) |
| `invoke` — the shared command key every generated action points at | `src/main.rs:391` (`run_invoke`) |
| `remove` — delete a server's imported actions/types | `src/main.rs:480` |
| MCP client (spawns the server, handshake, `tools/call`) | `src/mcp_client.rs` |
| Per-tool identity (`tool.json` + `manifest.json`) | `src/config.rs` |
| Naming (`mcp-<server>-<tool>`, `/packages/solx-mcp-actions/<server>`) | `src/naming.rs` |
| Allowlist keys | `solx-package.json` — `solx-mcp-actions-{import,remove,invoke}` |

`run_invoke` resolves the tool from `SOLX_ACTION_REF` → manifest → `tool.json`,
reads the arguments from stdin, then does `McpSession::connect` →
`call_tool` → `session.close()`. That spawn/teardown pair per call is what
goes away.

### 3.2 `solx-firefox`

`firefox-start` spawns a **detached** Firefox, polls the Marionette port,
writes `state.json` and exits. `firefox-stop` reads the PID back and kills
the tree. Firefox therefore already outlives the call; what does *not*
outlive the call is the MCP server that talks to it.

The `firefox` entry in `solx-mcp-actions/mcp-servers.json` runs
`../solx-firefox/bin/mcp-server --connect-existing --marionette-port 2828
--tool-preset developer`, so the *browser* session is shared today — but the
MCP server around it is not.

### 3.3 The background machinery in solx-core

Already implemented and tested (`core/solx-actions/tests/background.rs`,
18 tests):

| Piece | Where |
|---|---|
| Wire protocol (`Message`, `CommandRequest`, `CommandResponse`, `BackgroundConfig`) | `core/solx-surface/src/background.rs` |
| Dispatch branch for a background action | `core/solx-actions/src/dispatch.rs:259` (the `ActionType::Command` arm) |
| Manager, handle, reader, lifecycle, file lock, containment | `core/solx-actions/src/background/` |
| Lazy start (find the package's `start` action, run it) | `core/solx-actions/src/background/lifecycle.rs:137` (`ensure_background`) |
| Host ops served in-process: `exec`, `console.print` | `core/solx-actions/src/background/reader.rs:159` |
| Scope enforcement for a background's host calls | `core/solx-actions/src/background/handle.rs:208` (`host_frame`) |
| Built-ins `/builtin/background/list`, `/builtin/background/stop` | `core/solx-actions/src/internal/background.rs` |
| Host shutdown: stop every background | `core/solx-manager/src/lib.rs:378` (`App::shutdown`) — called by solx-server, solx-cli, solx-mcp (stdio), the desktop app |
| Child side of the protocol (`serve`, `serve_one_at_a_time`, `Call`, `Host`) | `solx-package-lib/src/background.rs` |
| A package that uses it | `solx-graph/` (`install.solx`, `src/main.rs`, `solx-package.json`) |

---

## 4. Constraints that shape the design

Properties of the existing core, not choices this plan makes.

**C1 — `fn_name` is an allowlist key, and only package install can add one.**
`prepare_command` (`core/solx-actions/src/exec/command.rs:91`) resolves
`fn_name` against `ConfigService::command_actions`; deny-by-default. The
only producers of that allowlist are `solx_packages::install_package`
(`core/solx-packages/src/lib.rs:498`) and the desktop app's Settings window
(`core/solx-tauri-plugin/src/commands.rs:742`). **There is no REST route that
writes `command_actions`.** So the MCP server's name cannot be part of the
command line — a per-server allowlist key cannot be registered at runtime.
⇒ The background learns its server from the environment, not from argv.

**C2 — the background process is the *start* action's `fn_name`.** An
operation never spawns its own command; `ensure_background` finds the
package's `start` action (same `backgroundNamespace`/`backgroundName`, op
`start`) and runs it through `exec_as`
(`core/solx-actions/src/background/lifecycle.rs:168`). The start action is
found by listing the package's actions, so it may live anywhere under
`/packages/solx-mcp-actions/`.

**C3 — the op string is all the host forwards.** A `CommandRequest` carries
`{id, op, params, invocation_id}` (`core/solx-surface/src/background.rs:32`).
The **action ref is not in the protocol**. `SOLX_ACTION_REF` is set on the
process (`prepare_command`), but it names the *start* action, not the
operation. ⇒ The operation has to carry the tool's identity.

**C4 — `start`, `stop` and `shutdown` are reserved op names.**
`BackgroundConfig::op()` maps `start`/`stop` to special handling and
`shutdown` is rejected outright (`core/solx-surface/src/background.rs:114`).
An MCP tool literally named `start` or `stop` would be misrouted.

**C5 — a background gets no credential and no console env vars.** No
`SOLX_SERVER_URL`/`SOLX_SERVER_TOKEN`, no `SOLX_CONSOLE_*`. It reaches solx
only by sending `exec`/`console.print` requests, which the host serves
in-process. This is why `import`/`remove` cannot be backgrounds (§1).

**C6 — a background's host calls are checked against the *start* action's
scopes**, not the operation's. The MCP background needs no scopes at all, so
this costs nothing — but the start action's row must not declare any it
doesn't need.

**C7 — one instance per `(namespace, name)` per host**, and an OS file lock
(`core/solx-actions/src/background/lock.rs`) keeps it to one per machine per
appdata dir. The lock filename is the name run through `segment()`
(`lock.rs:46`), which maps anything outside `[A-Za-z0-9._-]` to `_`. Two
different server keys that sanitise to the same string would collide.

**C8 — hosts stop their backgrounds on exit** (⇒ the MCP server process dies
with the host, since it is a descendant of the background and the
background's whole tree is contained — Job Object on Windows, process group
on POSIX, `core/solx-actions/src/background/contain.rs`).

**C9 — stdout is the protocol.** Any line that is not a protocol message
closes the instance and kills it. The package must not `println!` in `serve`
mode (`solx-package-lib/src/background.rs:26`).

**C10 — the package manifest's `scopes` is a ceiling** on what the package's
install script may declare (`core/solx-packages/src/lib.rs:295`).

**C11 — a content actor may not give an action declared scopes.** A row
saved over REST with an action token is refused if its `action_config`
declares scopes (`guard_keyed_action`,
`core/solx-actions/src/secrets/config.rs:40`). The imported rows must
therefore declare none — which they do not need to (§9).

---

## 5. The design

### 5.1 Granularity: one background per MCP server

`backgroundNamespace` = `solx-mcp-actions` (the package that owns the
process); `backgroundName` = the MCP server key from `mcp-servers.json`
(`firefox`, `filesystem`, …).

```
key   = ("solx-mcp-actions", "firefox")
lock  = <appdata>/backgrounds/solx-mcp-actions/firefox.lock
```

Every tool imported from `firefox` names that key, so they all share one
`mcp-server` process, one MCP connection, and one browser session. Different
servers get different backgrounds, so a broken server cannot take the others
down with it, and `/builtin/background/list` shows one entry per server.

The alternative — one background holding sessions for every server — was
rejected: it makes a single failing server fatal for the rest, and it makes
the natural "stop the browser server" operation impossible.

### 5.2 How the background learns which server it is: `SOLX_BACKGROUND`

`start_background` sets `SOLX_BACKGROUND` to `<namespace>/<name>`
(`core/solx-actions/src/background/lifecycle.rs:62`). The background reads
it, splits on the first `/`, and takes the name part as the MCP server key:

```rust
let (_, server) = std::env::var("SOLX_BACKGROUND")?
    .split_once('/')
    .ok_or("not started as a solx background")?;
```

This satisfies C1 (nothing is registered at runtime) and C2 (one allowlist
key serves every server).

**Server key constraints.** The key must not contain `/` (it would break the
split) and should be restricted to `[A-Za-z0-9._-]` so `segment()` is the
identity (C7). `import` should validate the key and refuse a server whose
name would not round-trip.

### 5.3 How the background learns which tool to call: `tool:<name>` as the op

**Decided.** Per C3 the op is the only channel, so the operation name
encodes the tool:

```json
"actionConfig": {
  "background": {
    "backgroundNamespace": "solx-mcp-actions",
    "backgroundName": "firefox",
    "backgroundOperation": "tool:new_page"
  },
  "timeout_secs": 300
}
```

The `tool:` prefix sidesteps C4: a tool named `start` or `stop` becomes
`tool:start`/`tool:stop`, which `BackgroundConfig::op()` treats as ordinary
calls. The background strips the prefix and calls `tools/call` with that
name.

*Rejected alternative:* extending the protocol with an `action_ref` field on
`CommandRequest` (`core/solx-surface/src/background.rs` plus the mirror in
`solx-package-lib/src/background.rs`), which would let the background resolve
the tool through the existing manifest. It is more general, but it changes
the wire protocol and solx-core for no gain here: with one background per
server, the tool name is already unambiguous. Revisit only if a second
package needs per-action identity.

### 5.4 Action layout

Per server `X`:

| Path | Name | Op | Purpose |
|---|---|---|---|
| `/packages/solx-mcp-actions/X` | `background-start` | `start` | Spawn the background; idempotent |
| `/packages/solx-mcp-actions/X` | `background-stop` | `stop` | Shut the background down |
| `/packages/solx-mcp-actions/X` | `mcp-X-<tool>` | `tool:<tool>` | One per imported tool |

All three kinds point at the same `fnName` (`solx-mcp-actions-serve`); only
the start action's command is ever run (C2). The names `background-*` cannot
collide with a tool action, which is always `mcp-X-<tool>`.

The control actions are created by `import`, not declared in
`install.solx`: their path carries the server's key, which the user chose
in `mcp-servers.json`. The imported tools have the same property.

A third control action, `background-connect`, was considered and dropped:
`start` cannot report a connection failure, because the core's
`start_background` spawns the process and returns immediately without
waiting for it to become ready. An ordinary operation could have checked
the connection, but the import already connects to the MCP server to list
its tools, so the check was mostly redundant. The setup script starts the
background instead, so it is ready before the first tool call.

### 5.5 Connection lifecycle

**Decided: eager attempt, lazy wait, retry once.**

- **Eager attempt.** The background starts connecting as soon as it is
  spawned, so the connection is warm by the time the first call arrives.
  The attempt runs in its own task; a failure does not kill the process.
- **Lazy wait.** The first tool call waits on that attempt and reports its
  **precise** error ("MCP initialize handshake failed: …"), rather than a
  generic "the background exited". If the eager attempt failed, the call
  re-attempts the connection itself — so a server that comes up late (Firefox
  started after the import) still works.
- **Retry once.** If a call fails because the MCP server died (Firefox
  restarted under it, geckodriver vanished), the background drops the rmcp
  session, **respawns the MCP server process**, reconnects, and retries the
  call once. A second failure is returned as an error.

Respawning the MCP server *process* (not just the rmcp session) is what
makes the retry useful: a `firefox-devtools-mcp` that lost its Firefox
connection is alive but useless, and only a new process re-attaches to
Firefox.

- **Concurrency.** `serve` (concurrent handlers) over
  `serve_one_at_a_time`, because rmcp's `RunningService::call_tool(&self, …)`
  supports concurrent requests on one session, and today's one-shot commands
  run in parallel. `RunningService` is `Send + Sync` (its `Peer` holds an
  `mpsc::Sender` and `Arc<dyn RequestIdProvider + Send + Sync>`), so
  `Arc<McpSession>` fits the `Fn + Send + Sync + 'static` handler bound.
  This needs a compile check to confirm.

### 5.6 Result envelope

The background must return exactly what `run_invoke` returns today, because
the generated `resultTypeRef` describes it:

```json
{ "server": "...", "tool": "...", "content": [ ... ],
  "structured_content": { ... }, "error": "..." }
```

A tool that reports `is_error: true` becomes an `error` field in a
successful result, as today. A *transport* failure (the server died) is a
protocol-level error and becomes `SolxError::Exec` — the distinction the
background's `ok`/`error` response already draws.

### 5.7 Who stops the background

**Decided: `remove` stops it.**

`remove` execs `/packages/solx-mcp-actions/<server>/background-stop` over
REST (`POST /actions/{ref}` executes —
`core/solx-server/src/routes/actions.rs:42`) **before** deleting the rows,
so the server is shut down gracefully while its actions still exist.

This costs **no extra scope**: a package action's implicit grant includes
`exec:/packages/<pkg>/*` (`action_scopes`,
`core/solx-surface/src/scope.rs:734`), so `remove` can already call its own
package's `background-stop`. The stop is idempotent, so calling it for a
server that was never started is a no-op.

---

## 6. Work items

### 6.1 `solx-mcp-actions` — package manifest

`solx-package.json`:

```json
"command_actions": {
  "solx-mcp-actions-import": { "command": ".\\solx-mcp-actions.exe import", "cwd": "bin" },
  "solx-mcp-actions-remove": { "command": ".\\solx-mcp-actions.exe remove", "cwd": "bin" },
  "solx-mcp-actions-invoke": { "command": ".\\solx-mcp-actions.exe invoke", "cwd": "bin" },
  "solx-mcp-actions-serve":  { "command": ".\\solx-mcp-actions.exe serve",  "cwd": "bin" }
},
"scopes": [
  "write:action:/packages/solx-mcp-actions/*",
  "write:command",
  "write:type:/packages/solx-mcp-actions/*"
]
```

`solx-mcp-actions-invoke` is **kept** for one release so actions imported by
an older version keep working until the server is re-imported (§8.1).

### 6.2 `solx-mcp-actions` — the new `serve` subcommand

New module `src/serve.rs`:

```rust
pub async fn serve(home: &Path) -> anyhow::Result<()> {
    let server = server_key()?;                       // from SOLX_BACKGROUND
    let cfg = config::load_servers_config(home)?;
    let def = config::find_server(&cfg, &server)?.clone();
    let base = config::servers_config_path(home).parent().unwrap_or(home).to_path_buf();

    let session = Arc::new(tokio::sync::Mutex::new(Session::empty(def, base)));
    // Eager attempt: start connecting now, so the first call finds it warm.
    let eager = { let s = session.clone(); tokio::spawn(async move { s.lock().await.connect().await }) };

    background::serve(move |call| {
        let session = session.clone();
        async move { handle(&session, call).await }
    })
    .await?;
    Ok(())
}

async fn handle(session: &Arc<Mutex<Session>>, call: Call) -> Result<Value, String> {
    match call.op.as_str() {
        op if op.starts_with("tool:") => {
            let tool = &op["tool:".len()..];
            session.lock().await.call_tool(tool, call.params).await
        }
        "connect" => session.lock().await.status().await,
        op => Err(format!("solx-mcp-actions has no operation '{op}'")),
    }
}
```

Notes:

- `background::is_background()` should be checked first, and the process
  should exit non-zero with a clear message if it is not a background —
  `solx-graph/src/main.rs:30` does exactly this.
- `Session` holds the rmcp `RunningService` plus the `McpSession` wrapper;
  `call_tool` connects on demand, and on a transport failure drops the
  session, respawns the MCP server process, reconnects and retries once.
- The `McpSession` wrapper in `src/mcp_client.rs` needs one change:
  `close()` currently consumes `self` (`src/mcp_client.rs:135`). A background
  needs to drop and replace the session, so it needs a `&mut self` variant
  or the session is replaced wholesale.
- Logging goes through `solx_package_log::{info,warn,error}`; while `serve`
  runs, those route to the host's console instead of the loopback
  (`solx-package-lib/src/lib.rs:105`).
- **No `println!` anywhere in `serve` mode** (C9). `print_json` must not be
  called on that path.

### 6.3 `solx-mcp-actions` — `import` creates the background actions

`import_one_tool` (`src/main.rs:281`) changes the action body:

```rust
let mut action_body = json!({
    "actionType": "command",
    "fnName": "solx-mcp-actions-serve",
    "caption": format!("MCP: {tool_name}"),
    "category": "mcp",
    "description": description,
    "capabilities": vec!["mcp".to_string(), server.to_string()],
    "phrases": vec![...],
    "paramTypeRef": param_type_ref,
    "resultTypeRef": result_type_ref,
    "actionConfig": {
        "background": {
            "backgroundNamespace": "solx-mcp-actions",
            "backgroundName": server,
            "backgroundOperation": format!("tool:{tool_name}"),
        },
        "timeout_secs": 300,
    },
});
```

`import` also creates the two control actions once per server, before the
tool loop:

```
/packages/solx-mcp-actions/<server>/background-start  fnName: solx-mcp-actions-serve
/packages/solx-mcp-actions/<server>/background-stop   fnName: solx-mcp-actions-serve
```

with `actionConfig.background` = `{namespace, name, operation: "start" |
"stop"}`.

All are PUT through the existing `solx::new_action` helper (`src/solx.rs`),
so they are created-or-replaced idempotently.

`import`'s own connection is a **separate, short-lived** MCP session: it
lists the tools and then closes it (`src/main.rs:191`, `src/main.rs:220`).
That is fine and should stay — `import` is an operator action, not a
background op.

### 6.4 `solx-mcp-actions` — `remove` stops the background and deletes the rows

`remove` (`src/main.rs:480`) changes from "delete rows" to:

1. `POST /packages/solx-mcp-actions/<server>/background-stop` — stop the
   background gracefully (§5.7). A failure here is reported but does not
   stop the cleanup.
2. Delete the tool actions and types, as today.
3. Delete the two control actions.

The control actions' names are derived from the server key, so `remove` can
compute them; alternatively the per-server `manifest.json` can record them
explicitly, which keeps the manifest the single source of truth for what
`remove` created — the property `src/config.rs`'s `ManifestEntry` exists to
protect.

### 6.5 `solx-mcp-actions` — validation

`import` should refuse, with a clear message, a server key that:
- contains `/` (breaks the `SOLX_BACKGROUND` split),
- is not `[A-Za-z0-9._-]` (breaks the lock filename identity, C7).

### 6.6 `solx-mcp-actions` — docs

- `README.md`: replace the "Known limitations (phase 1)" bullet about
  per-invocation spawn/teardown; document the background, the two control
  actions, `/builtin/background/list`, and what happens when the MCP server
  dies.
- `HOWTO.md` in the packages repo: the "Background commands" section already
  covers the mechanism; add a note that an MCP server is a background whose
  op carries the tool name.

### 6.7 `solx-firefox` — scopes, teardown ordering, docs

Firefox stays exactly as it is: a detached process managed by
`firefox-start`/`firefox-stop`, both one-shot Command actions that need no
scopes (they never call back into solx).

What changes:

1. **Scope declarations.** `firefox-mcp-setup` and `firefox-mcp-teardown`
   exec into the mcp-actions package, so they need
   `exec:/packages/solx-mcp-actions/*` — declared on both script actions in
   `install.solx`, and listed in `solx-firefox/solx-package.json` (the
   manifest is the ceiling, C10).
2. **The setup script starts the background.** `mcp-setup.solx` gains a third
   step after `import`:

   ```solx
   $background = exec /packages/solx-mcp-actions/firefox/background-start --json {};
   ```

   so the MCP server is connected and warm before the first tool call.
   (Firefox is already running by then — the first step is `firefox-start`.)
3. **The teardown script's order is already right.** `mcp-teardown.solx` does
   `remove` then `firefox-stop`; `remove` now stops the MCP background first
   (§5.7), so the MCP server closes its Firefox connection before the
   browser goes away. The script itself needs no logic change, only the
   scope declaration.
4. **`README.md`**: note that the MCP server is now a background, that
   `background-start` starts it, and that the Firefox instance itself is
   still a detached process with its own lifetime.

### 6.8 solx-core — no changes

Nothing in `core/` needs to change. The one thing that *would* have (the
`action_ref` protocol field) was rejected in §5.3.

---

## 7. Migration, compatibility and operational behaviour

### 7.1 Existing installs

Actions imported by the current version point at `solx-mcp-actions-invoke`
and have no `actionConfig.background`. After upgrading the package they keep
working exactly as today (one-shot, per-call server) until the server is
re-imported. `import` overwrites the tool actions in place (same path and
name), so re-importing is the migration.

`import` should say so in its result, e.g. `"background": true` per tool, or
a top-level `"migrated_to_background": true`.

> **The migration needs the tool actions unlocked first.** A package install
> locks every action under the package's own path
> (`core/solx-packages/src/lib.rs:552`), and the imported tools live under
> it — so after any reinstall they are locked, and `import` runs as content
> (an action token), which cannot change a locked action
> (`guard_locked_action`, `core/solx-actions/src/lock.rs:51`). An operator
> unlocks them first:
>
> ```
> solx action unlock /packages/solx-mcp-actions/firefox/mcp-firefox-list-pages
> ```
>
> once per tool. The package's error message now says so when it hits one.
>
> This is a pre-existing bug, not something the migration introduces: the
> old `import` wrote the same rows the same way, so a re-import after a
> reinstall has always failed. Two related bugs found while implementing
> this are fixed in the same change (§7.1a).

### 7.1a Two pre-existing bugs fixed on the way

Both were found by running the real import against a live solx, and both
are in the path the migration uses.

- **A failed import deleted the tools it could not update.** The "stale"
  set — the tools a narrowing filter should drop — was computed from the
  tools the import had *successfully* written, so a tool whose save failed
  counted as stale and was deleted, and dropped from the manifest, leaving
  an action nothing could clean up. The stale set is now the tools the
  *filter* no longer selects; a tool that failed keeps its manifest entry.
- **A failed import wiped the manifest.** The manifest was written from the
  successful imports alone, so one failure emptied it. It is now the
  previous entries merged with this import's.

### 7.2 What the user sees

- `solx exec /packages/solx-mcp-actions/firefox/background-start` —
  starts the background (idempotent, returns the running instance's
  status).
- `/builtin/background/list` — shows `solx-mcp-actions/firefox` with its pid.
- The first tool call after a host restart pays the MCP server's startup
  cost again; every call after that is fast.
- The host exits → the background and the MCP server are killed (C8).
  Firefox itself survives (it is detached).

### 7.3 Two solx processes

A CLI that is not attached to a running `solx-server` builds a local app and
hosts its own backgrounds. The file lock (C7) refuses a second process
starting the same background against the same appdata dir, naming the
holder. The remedy is the one `HOWTO.md` already gives: keep `solx-server`
or the desktop app running and let clients attach to it.

This matters more for MCP than for other backgrounds: a CLI run that starts
the background and then exits would take the MCP server down with it at
`App::shutdown()` (`core/solx-cli/src/main.rs:224`).

---

## 8. Scopes

### 8.1 The whole picture

| Action | Type | Scopes it needs |
|---|---|---|
| `/packages/solx-mcp-actions/import` | Command, one-shot | `write:action:/packages/solx-mcp-actions/*`, `write:command`, `write:type:/packages/solx-mcp-actions/*` |
| `/packages/solx-mcp-actions/remove` | Command, one-shot | the same three (to delete), **plus nothing extra** for calling `background-stop` — the package's implicit `exec:/packages/solx-mcp-actions/*` covers it |
| `/packages/solx-mcp-actions/<server>/background-start` | background start | **none** |
| `/packages/solx-mcp-actions/<server>/background-stop` | background stop | **none** |
| `/packages/solx-mcp-actions/<server>/mcp-<server>-<tool>` | background op | **none** |
| `/packages/solx-firefox/firefox-start` | Command, one-shot | **none** |
| `/packages/solx-firefox/firefox-stop` | Command, one-shot | **none** |
| `/packages/solx-firefox/firefox-mcp-setup` | Script | `exec:/packages/solx-mcp-actions/*` |
| `/packages/solx-firefox/firefox-mcp-teardown` | Script | `exec:/packages/solx-mcp-actions/*` |

### 8.2 Why the background rows need none

A one-shot Command child is issued an action token carrying
`action_scopes(path, action_config)` (`core/solx-actions/src/exec/command.rs:220`,
`core/solx-surface/src/scope.rs:734`). A background is issued **no token at
all** — `start_background` never calls `issue_action_token`. Instead, the
host checks each host call the background makes against the *start* action's
scopes, in-process (`core/solx-actions/src/background/handle.rs:208`).

The MCP background makes no host calls at all, so it needs no scopes (C6).
And it *couldn't* have them: a content actor is refused a row that declares
scopes (C11). The migration therefore **removes** the token from the
tool-call path — a strict reduction in what the MCP path can do to solx.

### 8.3 The declarations to add

- `solx-mcp-actions/solx-package.json`: add

  ```json
  "scopes": [
    "write:action:/packages/solx-mcp-actions/*",
    "write:command",
    "write:type:/packages/solx-mcp-actions/*"
  ]
  ```

- `solx-mcp-actions/install.solx`: declare the same three on the `import` and
  `remove` actions' `actionConfig.scopes` (the manifest is the ceiling —
  C10).

- `solx-firefox/solx-package.json`: add

  ```json
  "scopes": ["exec:/packages/solx-mcp-actions/*"]
  ```

- `solx-firefox/install.solx`: declare the same on the two script actions.

The same list appears in `core/docs/token-scopes-plan.md:484-492`, which
records what each package's callbacks need.

> **Verify this first** (§10 step 0): it is possible the packages are in
> fact installed with operator-granted scopes (`solx action scopes`), or that
> `scope_enforcement` is set to `audit` in practice. The declarations above
> are what the manifest should say either way — it is what an operator
> reviews before installing.

---

## 9. Risks

| Risk | Mitigation |
|---|---|
| `RunningService` is not `Sync`, so concurrent `serve` handlers won't compile | **Resolved**: it is `Send + Sync`, so `Arc<McpSession>` compiles and calls run concurrently. If it ever stops being, `serve_one_at_a_time` keeps correctness |
| The MCP server dies mid-session (Firefox restarted under it) | Respawn-and-retry-once (§5.5); the second failure is a clear error. **Verified**: killing the MCP server under the background, the next call respawned it and answered |
| A second solx process wants the same background | The file lock refuses it and names the holder (C7); `HOWTO.md` already tells users to attach to a running server |
| The MCP server outlives one call and holds a stale Firefox connection | The reconnect path respawns the MCP server process, which re-attaches to Firefox |
| The background's stdout is corrupted by a stray `println!` | The process is killed with a clear reason ("wrote a line that isn't a protocol message"); no `println!` in `serve` mode (C9) |
| `import` refuses to write rows under `enforce` | §8.3; verified against a live solx |
| The tool's `timeout_secs` is too short for a slow MCP tool | 300 s default; `import` can raise it per tool if a server needs longer |
| **A one-shot `invoke` and the background cannot both hold the MCP server** | They fight over the single Marionette session: with the background running, a one-shot `invoke` hangs for 60 s and then fails. Fine — new imports all use the background — but a stale one-shot action left over from before the upgrade will look broken. Re-importing the server replaces it |

---

## 10. Order of work

**Step 0 — verify the baseline.** Install `solx-mcp-actions` and
`solx-firefox` into a scratch appdata with the default `scope_enforcement`
and confirm that `/packages/solx-mcp-actions/import` can create the action
and type rows, and that `firefox-mcp-setup` / `-teardown` can exec the
mcp-actions package's actions. If they are refused, add the declarations in
§8.3 first. Everything below assumes `import` works.

> **What this step found**: the scope declarations were indeed missing (§8.3),
> and `import` was also blocked by the package lock (§7.1). Both are fixed
> now, except that the lock needs an operator's `solx action unlock` per
> tool before a re-import.

Then:

1. `solx-mcp-actions`: the `serve` subcommand and `Session` (eager attempt,
   lazy wait, respawn-and-retry) — testable on its own against a stub MCP
   server. **Done.**
2. `solx-mcp-actions`: `import` writes the background `actionConfig` and
   creates the two control actions; `remove` stops the background and
   deletes everything. **Done.**
3. `solx-mcp-actions`: server-key validation; `import` result reporting.
   **Done.**
4. `solx-mcp-actions`: docs (README, HOWTO note). **Not done** — the
   behaviour change was accepted as is.
5. `solx-firefox`: scope declarations; `mcp-setup.solx` gains the
   `background-start` step; README updates. **Scopes and the setup step
   done; the README was not updated.**
6. End-to-end: install both packages into a scratch appdata, import the
   `firefox` server, call a tool twice, confirm the same `mcp-server` pid
   answered both times (`/builtin/background/list`), stop it, confirm the
   pid is gone, and confirm the host exit leaves nothing behind. **Done
   against a live solx**: one `mcp-server.exe` (55 MB) serves every call,
   `/builtin/background/list` shows the instance, and `remove` stops it and
   leaves no process behind.

### Measured, against a live solx

| | one-shot | background |
|---|---|---|
| MCP layer per call (spawn + handshake + first call) | ~125 ms | ~2 ms |
| whole call through the CLI | ~140 ms | ~34 ms |
| MCP server processes | 1 per call, 58 MB working set | 1 for the whole run |


### Testing notes

`solx-mcp-actions` has no tests today. The parts worth testing:

- `serve` against a stub MCP server (a small binary that speaks MCP over
  stdio, like `core/solx-actions/tests/fixtures/fake_background.rs` does for
  the solx protocol): connect once, two calls, one server process.
- Respawn-and-retry: kill the stub server between two calls; the second call
  respawns it and succeeds.
- The op parsing: `tool:start`, `tool:stop`, an unknown prefix.
- `import`'s generated `actionConfig.background` shape — a golden-file test
  over the action body.

The stub MCP server is worth building even if the rest slips: it is what
makes "one server process for N calls" a test rather than a claim. rmcp has
a server side, so a few dozen lines of a `tools/call` echo is enough.

---

## 11. Reference: the `solx-graph` shape this follows

`solx-graph/install.solx`:

```solx
save action /packages/solx-graph/start --json {
    "actionType": "command",
    "fnName": "graph-serve",
    "actionConfig": { "background": {
        "backgroundNamespace": "solx-graph",
        "backgroundName": "store",
        "backgroundOperation": "start" } },
};
save action /packages/solx-graph/extract-document --json {
    "actionType": "command",
    "fnName": "graph-serve",
    "actionConfig": { "timeout_secs": 900, "background": {
        "backgroundNamespace": "solx-graph",
        "backgroundName": "store",
        "backgroundOperation": "extract-document" } },
};
```

`solx-graph/src/main.rs`:

```rust
solx_package_log::init("solx-graph");
if !background::is_background() { /* refuse */ }
let graph = Arc::new(Graph::open(&dir)?);
background::serve_one_at_a_time(move |call| handle(graph.clone(), call)).await
```

The differences for `solx-mcp-actions`: the background is **per server** (not
one per package), the server is learned from `SOLX_BACKGROUND` (not
hardcoded), the op is `tool:<name>` (not a fixed operation name), and the
concurrency mode is `serve` (not `serve_one_at_a_time`).

---

## 12. Out of scope

- Making `solx-firefox` itself a background package (a `solx-firefox serve`
  supervisor owning the Firefox child). It would tie the browser's lifetime
  to the host, for no gain in the MCP path.
- Restart policies, health checks, idle shutdown for backgrounds.
- Sharing one background across host processes (CLI plus desktop app).
- Streaming responses or progress updates beyond `console.print`.
