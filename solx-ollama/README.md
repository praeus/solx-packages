# solx-ollama

Ollama REST API actions for solx-core. One `wasm32-wasip2` component
(`bin/solx-ollama.wasm`, ~150 KB) backs **thirteen** registered actions, each
selected by the `fn_name` on its action row.

All outbound HTTP goes through `/builtin/http_request` — the guest has no
sockets of its own, because the host stubs WASI.

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
| `/packages/solx-ollama/ollama-generate` | `generate` | `POST /api/generate` |
| `/packages/solx-ollama/ollama-chat` | `chat` | `POST /api/chat` |
| `/packages/solx-ollama/ollama-embed` | `embed` | `POST /api/embed` |
| `/packages/solx-ollama/ollama-list-models` | `list_models` | `GET /api/tags` |
| `/packages/solx-ollama/ollama-show-model` | `show_model` | `POST /api/show` |
| `/packages/solx-ollama/ollama-ps` | `ps` | `GET /api/ps` |
| `/packages/solx-ollama/ollama-version` | `version` | `GET /api/version` |
| `/packages/solx-ollama/ollama-pull-model` | `pull_model` | `POST /api/pull` |
| `/packages/solx-ollama/ollama-push-model` | `push_model` | `POST /api/push` |
| `/packages/solx-ollama/ollama-create-model` | `create_model` | `POST /api/create` |
| `/packages/solx-ollama/ollama-copy-model` | `copy_model` | `POST /api/copy` |
| `/packages/solx-ollama/ollama-delete-model` | `delete_model` | `DELETE /api/delete` |
| `/packages/solx-ollama/ollama-set-api-key` | `set_api_key` | — (writes a secret) |

Ollama's response JSON is returned **verbatim** as the action `result`.
`/api/copy` and `/api/delete` answer 200 with an empty body, which becomes
`{"status": "success"}`.

```bash
solx exec /packages/solx-ollama/ollama-version --json '{}'
solx exec /packages/solx-ollama/ollama-chat --json '{"model":"qwen3:4b","messages":[{"role":"user","content":"hi"}]}'
```

## Streaming is not supported

Every request body gets `"stream": false` injected, and `stream` is not an
accepted parameter on any action — passing it is silently dropped rather than
honoured.

This is structural, not an oversight: a WASM action returns exactly one value
and has no channel to emit chunks, so a streamed NDJSON body would arrive as
unparseable text and collapse to `null`.

The practical consequence is on **`pull-model`** and `push-model`: with
streaming off there is no progress output at all, so a first pull of a large
model blocks silently for as long as the download takes (the timeout ceiling
is 2 hours). Prefer `ollama pull` on the command line for large models.

Real streaming would need new `http_stream_start` / `http_stream_poll` /
`http_stream_close` built-ins in solx-core, holding the response in a
host-side registry keyed by a minted id — the shape `internal/oauth.rs`
already uses. That is a solx-core change, not a package change. Full design,
including the cursor-based poll API and why it's shaped that way:
[docs/streaming-design.md](docs/streaming-design.md).

## Configuration

### Base URL

Resolved in order:

1. `base_url` param on the call.
2. `/builtin/get_env` for `OLLAMA_HOST`.
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

That action exists to break a chicken-and-egg problem: `/builtin/set_secret`
encrypts using a key taken from the *calling* action's `action_config.secrets`,
so only an action that already holds the key can write the secret. The secret
name is hardcoded, so this action cannot overwrite an unrelated one.

**Re-installing rotates the key.** `install.solx` generates a fresh AES key with
`random 32` and writes it onto all thirteen rows. Running `solx install-package`
again mints a new one, after which the previously stored token fails to decrypt
(`secret decryption failed`). Recovery is one command: run `ollama-set-api-key`
again.

## Timeouts

Two budgets, and the outer one is always the larger:

- **Inner** — the per-HTTP-request timeout. Set with `timeout_secs` on the
  call; clamped to a per-endpoint ceiling (1800s for generate/chat, 7200s for
  pull/push/create, 600s for embed, 300s elsewhere).
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

- **`/api/create` with `files` or `adapters`** is passed through, but those
  fields reference blobs that must already exist on the server, uploaded via
  `POST /api/blobs/sha256:<digest>`. This package does not implement blob
  upload, so those two fields only work for digests already present.
- **Legacy embeddings**: pass `{"legacy": true}` to `ollama-embed` to use the
  older `/api/embeddings` endpoint, which takes a single `prompt` string and
  returns `embedding` (one vector) rather than `embeddings` (a list). An array
  input is rejected in that mode.
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
src/request.rs   marshal to /builtin/http_request, interpret the response
src/guest.rs     wit-bindgen shim (wasm32 only)
wit/             vendored copy of solx-core/solx-wasm/wit/custom-action.wit
```

The WIT is vendored so the package builds without a sibling `solx-core`
checkout. A test asserts it has not drifted, but only when that checkout is
actually present.
