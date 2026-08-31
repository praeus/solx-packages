# solx-ollama

Ollama REST API actions for solx-core. One `wasm32-wasip2` component
(`bin/solx-ollama.wasm`) backs **four** registered actions, each selected by
the `fn_name` on its action row — deliberately scoped to chat, listing
installed models, installing a model, and the auth bootstrap those need.
Ollama's broader surface (raw completion, embeddings, model introspection,
registry push, custom model derivation/quantization, copy, and delete) is
intentionally not exposed here; delete in particular is the kind of
irreversible operation this package avoids surfacing at all.

All outbound HTTP goes through solx-actions' HTTP built-ins — the guest has
no sockets of its own, because the host stubs WASI. `chat` and `pull_model`
stream through `/builtin/web/stream/*`; `list_models` is a single blocking
`/builtin/web/http_request` call. See "Streaming" below.

## Build and install

```powershell
rustup target add wasm32-wasip2   # once
.\build.ps1 -Install
```

```bash
rustup target add wasm32-wasip2   # once
./build.sh --install
```

`build.ps1` / `build.sh` compile the component and stage it at
`bin/solx-ollama.wasm`. `install.solx` reads that path as its very **first**
statement, so if you have not built yet the install aborts with a plain
"cannot find the file" error before writing a single type or action row —
there is no half-installed state.

`cargo test` runs the unit tests on the host target (not wasm). It works
because the wit-bindgen surface lives in `src/guest.rs` behind
`#[cfg(target_arch = "wasm32")]`; everything else is written against the
`Host` trait in `src/host.rs`.

## Actions

| action | `fn_name` | endpoint |
|---|---|---|
| `/packages/solx-ollama/ollama-chat` | `chat` | `POST /api/chat` |
| `/packages/solx-ollama/ollama-list-models` | `list_models` | `GET /api/tags` |
| `/packages/solx-ollama/ollama-pull-model` | `pull_model` | `POST /api/pull` (install a model) |
| `/packages/solx-ollama/ollama-set-api-key` | `set_api_key` | — (writes a secret) |

Ollama's response JSON is returned **verbatim** as the action `result`.

```bash
solx exec /packages/solx-ollama/ollama-list-models --json '{}'
solx exec /packages/solx-ollama/ollama-chat --json '{"model":"qwen3:4b","messages":[{"role":"user","content":"hi"}]}'
```

## Streaming

`chat` and `pull_model` always send `"stream": true` — `stream` is not an
accepted parameter on either action; passing it is silently dropped rather
than honoured, the same as before, just with the opposite forced value.

The action's return value is unchanged: one final JSON result, shaped exactly
like the old blocking response (`chat`'s `message.content` is the
concatenation of every token delta and `message.tool_calls` every tool call
made anywhere in the stream; `pull_model` returns the last status object).
What's new is that you can now *watch* the call while it runs, and cancel
it:

- **Live output** — every NDJSON chunk Ollama sends is written to the
  action's own console (`level: "chunk"`, with the raw chunk as `data`) as it
  arrives. Tail it with `console/tail`:

  ```bash
  solx exec /builtin/console/tail --json '{"action_ref":"/packages/solx-ollama/ollama-chat","wait_secs":30}'
  ```

- **Cancellation** — start the call detached with `/builtin/action/start`,
  then `/builtin/action/stop` it. The guest checks
  `/builtin/action/cancelled` between chunks and closes the upstream
  connection promptly rather than only being caught by the outer force-abort
  after the grace period.

This is built on three new solx-core built-ins,
`/builtin/web/stream/{start,poll,close}` (`solx-actions`), which hold the
response in a host-side registry keyed by a minted `stream_id` — the guest
has no sockets and no state across invocations, so the stream has to live in
the host process. Full background:
[docs/streaming-design.md](docs/streaming-design.md) (the design doc that
scoped this out originally; the built-ins described there now exist).

## Tool calling

`chat` supports the full tool loop for any model that supports tools — pass
`tools`, get `message.tool_calls` back, run them, and send the results in as
the next turn:

```bash
solx exec /packages/solx-ollama/ollama-chat --json '{
  "model": "qwen3:4b",
  "messages": [{"role":"user","content":"weather in Paris?"}],
  "tools": [{"type":"function","function":{
    "name":"get_weather",
    "description":"Current weather for a city",
    "parameters":{"type":"object","required":["city"],
                  "properties":{"city":{"type":"string"}}}}}]
}'
```

The reply carries `message.tool_calls`, each entry `{"function": {"name",
"arguments"}}`. Run each one yourself, then call again with the assistant turn
and one `role: "tool"` turn per result appended to `messages`:

```json
{"role": "tool", "tool_name": "get_weather", "content": "18C, clear"}
```

Two details worth knowing:

- **Tool calls arrive mid-stream.** Ollama emits each one complete, in a chunk
  of its own that carries no text, and never in the final `done` chunk — so
  they are collected as they arrive and folded back onto the final
  `message.tool_calls`, in call order. The key is **absent**, not an empty
  array, when the model called nothing.
- **A tool-call chunk shows up in the console** as
  `[tool_call get_weather({"city":"Paris"})]`, so `console/tail` doesn't look
  like the model stalled while it was deciding.

Nothing here checks whether the model supports tools first — `/api/tags`
doesn't report capabilities. A model without tool support rejects the request,
which surfaces as `kind: "http_status"` or `kind: "ollama_error"` naming the
model.

## Configuration

### Base URL

Resolved in order:

1. `base_url` param on the call.
2. `/builtin/env/get_env` for `OLLAMA_HOST`.
3. `http://localhost:11434`.

The env route needs an `env_mappings` entry in `solx-config.json`, since the
env store is an allowlist rather than the real process environment:

```json
{ "env_mappings": { "OLLAMA_HOST": "OLLAMA_HOST" } }
```

`OLLAMA_HOST` is conventionally written the way `ollama serve` wants to *bind*,
so the value is normalized before use: a missing scheme becomes `http://`, a
bare host gains `:11434`, a trailing slash is dropped, and `0.0.0.0` is
rewritten to `127.0.0.1` (a wildcard bind address is not dialable on Windows).
An explicit scheme is respected as-is and never gains a port, so
`https://ollama.com` works.

**Remote mode:** `init_env_mappings` runs in the process that owns the managers.
If solx is configured with a `server_url`, the env store lives in the *server*
process, so `OLLAMA_HOST` has to be exported there rather than in your shell.

### Authentication

For Ollama Cloud or a reverse-proxied server. Sent as
`Authorization: Bearer <token>`, resolved first-hit-wins:

1. `api_key` param — convenient, but it lands in shell history and the exec
   log. Prefer one of the others.
2. `auth_secret_name` param — an explicit `get_secret` lookup. **Failure here
   is fatal**: you named a secret, so falling back to an unauthenticated
   request would send it in the clear.
3. The `OLLAMA_API_KEY` secret — the convention. Best-effort and silent, which
   is what lets a plain local Ollama work with no configuration at all.
4. `OLLAMA_API_KEY` via `get_env`.

To store the token:

```bash
solx exec /packages/solx-ollama/ollama-set-api-key --json '{"value":"<token>"}'
```

That action exists to break a chicken-and-egg problem: `/builtin/secrets/set_secret`
encrypts using a key taken from the *calling* action's `action_config.secrets`,
so only an action that already holds the key can write the secret. The secret
name is hardcoded, so this action cannot overwrite an unrelated one.

**Re-installing keeps the key.** `install.solx` generates a fresh AES key with
`random 32` only on a genuine first install; if any action already exists under
`/packages/solx-ollama` it writes the sentinel `"***"` onto the rows instead,
which `save action` resolves back to each row's stored key rather than
overwriting it (`solx-actions::mask::unmask_merge`). So upgrading the package
leaves an already-stored token decryptable — the same approach solx-google uses
for its OAuth key.

The one case that still needs `ollama-set-api-key` re-run is a *partial* prior
install: if some rows exist and others don't, the missing ones have no stored
key to restore and `save action` fails with "there is no stored value to
restore". Delete the surviving rows and install clean.

## Timeouts

Two budgets, and the outer one is always the larger:

- **Inner** — the per-HTTP-request timeout. Set with `timeout_secs` on the
  call; clamped to a per-endpoint ceiling (1800s for chat, 7200s for pull,
  300s for list-models).
- **Outer** — `action_config.timeout_secs` on the action row, a wall-clock
  budget for the whole guest invocation. `install.solx` sets it to the inner
  ceiling plus 60s of headroom for first-call component compilation.

Because the outer always exceeds the inner ceiling, a slow server surfaces as a
real HTTP timeout (`kind: "transport"`) rather than the far less informative
`wasm action timed out`.

## Errors

A failed call returns `success: false` with a machine-readable `result` object
carrying `kind` and `error`:

| `kind` | meaning |
|---|---|
| `transport` | the server was unreachable, or the request never completed |
| `http_status` | Ollama returned a non-2xx; includes `status` and the response `body` |
| `ollama_error` | HTTP 200 but the body carried an `error` field |
| `bad_params` | a required parameter was missing, or params were not an object |
| `unknown_action` | the row's `fn_name` is not one this component serves; `known` lists the valid ones |
| `auth` | an explicitly named secret could not be resolved |
| `non_utf8` | the response body was not valid UTF-8 |

Note that most missing-parameter cases never reach the guest: each action has a
`param_type_ref`, so the host rejects the call during schema validation first.
The guest's own check is defence in depth.

## Notes and limitations

- **Uninstall is not resilient to a partial install**: `delete` on a missing
  row is a hard error, so if an install failed midway, `solx uninstall-package`
  will stop at the first absent row. Delete the surviving rows by hand and
  retry.
- **Uninstall does not remove the stored token.** There is no `delete_secret`
  built-in action, so the `OLLAMA_API_KEY` keyring entry outlives the package.

## Layout

```
src/lib.rs       dispatch on fn_name, plus set_api_key
src/host.rs      the Host trait — the seam that keeps everything host-testable
src/endpoint.rs  the endpoint table: fn_name -> method, path, params, timeouts
src/config.rs    base-url normalization and auth resolution
src/request.rs   marshal to /builtin/web/http_request or /builtin/web/stream/*, interpret the response
src/guest.rs     wit-bindgen shim (wasm32 only)
wit/             vendored copy of solx-core/solx-wasm/wit/custom-action.wit
```

The WIT is vendored so the package builds without a sibling `solx-core`
checkout. A test asserts it has not drifted, but only when that checkout is
actually present.
