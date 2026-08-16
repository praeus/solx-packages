# Phase 2 — streaming via pollable host-side streams

**Status: implemented.** `http_stream_start`/`http_stream_poll`/
`http_stream_close` now exist in `solx-actions/src/internal/http_stream.rs`,
and `generate`/`chat`/`pull_model`/`push_model`/`create_model` in this
package drive them from `src/request.rs`. See the README's "Streaming"
section for the user-facing summary. The rest of this document is the
original design write-up, kept for the reasoning behind the shape (registry
pattern, cursor semantics, reaper) — the "not built" framing below is
historical.

This is the design that was scoped out when `solx-ollama` shipped
with `"stream": false` forced on every request. It lives here because
it required changes to `solx-core`, not to this package, and needed to survive
independently of any one conversation or plan file.

## Why the package can't do this alone

Ollama has **no** job/resume API. Streaming is one long-lived HTTP response
emitting NDJSON, with no request id and no poll endpoint — dropping the
connection aborts generation. So any identifier has to be minted by solx, not
read from Ollama.

Three things rule out doing it inside the guest:

- `/builtin/http_request` awaits the entire response body before returning.
- WASI is stubbed by the host (`WasiCtxBuilder::new().build()` in
  `wasm_host.rs`), so the guest has no sockets of its own.
- Decisively: **a guest has no state across invocations.** `wasm_host.rs`
  builds a fresh `Store` and instantiates a fresh component on every single
  `run()` call. Even a poll *handle* can't live in the guest between an
  `-start` call and the next `-poll` call.

The stream has to live in the host process.

## Shape: three new internal actions

`solx-actions/src/internal/http_stream.rs`, modeled directly on
`solx-actions/src/internal/oauth.rs` — that module already solves the same
problem (a process-static registry keyed by a minted id, with a start/await/stop
triad) for the OAuth loopback listener.

- **`http_stream_start`** — same params as `http_request`, but spawns a task
  that holds the `reqwest::Response`, reads `bytes_stream()`, splits on
  newlines, and appends parsed JSON lines to a registry buffer. Returns
  `{stream_id, status}` as soon as response headers arrive, without waiting
  for the body.
- **`http_stream_poll`** — `{stream_id, cursor, wait_secs?}` →
  `{chunks, next_cursor, done, dropped, status, error}`. `wait_secs` is an
  optional long-poll: block until at least one new chunk exists, or timeout.
- **`http_stream_close`** — abort the task, drop the buffer.

## Cursor-based, not a destructive drain

This is forced by the two consumers this design targets — chosen because
they're the two that work with what solx already has:

- **`.solx` script loops.** A script can't do arithmetic, but with cursors it
  never needs to — it just threads back whatever the previous poll returned:

  ```
  $s = exec /packages/solx-ollama/ollama-chat-start --json '{"model":"llama3.2:1b","messages":[...]}';
  for $i in 0..600;
    $p = exec /packages/solx-ollama/ollama-chat-poll --json '{"stream_id":"$s.result.stream_id","cursor":"$p.result.next_cursor"}';
    if $p.result.done == true; ... endif;
    wait 0.5;
  endfor
  ```

  The first iteration's `$p` is unresolved and passes through literally
  (`substitute_vars_in_token` in `solx-scripts/src/lib.rs`), so `cursor` needs
  to explicitly treat a missing or unresolved value as "from the start" —
  this must be handled in the built-in rather than relied on as an accident
  of the substitution rules.

- **`solx-server` / SSE.** The server drains the registry and re-emits over
  SSE to an HTTP client. This needs multiple readers to be able to attach
  independently and replay from an offset, which is only possible if chunks
  are retained and addressed by index rather than consumed on read.

A destructive drain (return-and-forget) would work for neither: a script's
poll loop can miss a chunk to a race between two callers, and SSE needs
replay for reconnects.

## What's genuinely new, beyond the oauth skeleton

- **A reaper.** Abandoned streams pin a connection open and grow a buffer
  forever if nothing polls them again. Needs a TTL since last poll plus a max
  buffered-bytes cap; on overflow, drop the oldest chunks and report a
  `dropped` count so a late-attaching reader knows it missed data. `oauth.rs`
  has the same leak shape today but a human closes the loop by calling
  `oauth_stop`; nothing does that for an abandoned generation.
- **Caller attribution.** `InternalCtx.caller` should own the stream, so one
  action can't poll or close a stream started by another.
  **As implemented, this was dropped in favor of the same unrestricted,
  bearer-capability model `oauth_await`/`oauth_stop` and
  `console_read`/`tail`/`clear` already use**: access is gated purely on
  knowing the unguessable `stream_id`, no caller check. Caller-scoping would
  have broken this doc's own script-loop example above, where
  `ollama-chat-poll` (a different action_ref, and a different invocation)
  polls a stream `ollama-chat-start` created.
- Seed rows in `solx-actions/src/seed.rs` and param schemas in
  `solx-types/src/seed.rs`, matching the existing built-in registration
  pattern.

## Forward compatibility already in place

`endpoint.rs` in this package records `force_stream_false` per endpoint, so
the endpoint table already knows which endpoints stream. Adding Phase 2 is
additive on the `solx-ollama` side:

- a `streaming: bool` (or a second table) on `Endpoint`,
- a second code path in `request.rs` that targets `http_stream_start` instead
  of `http_request`,
- paired `-start` / `-poll` / `-close` action rows in `install.solx`.

No rewrite of the blocking path — the 13 existing actions keep working
unchanged, whether or not Phase 2 ever lands.

## Recommended first target

**`pull_model` progress**, not chat/generate tokens.
`{"status":"downloading","completed":N,"total":M}` is latency-insensitive and
works fine through a `.solx` poll loop with a 1–2s `wait`, whereas today a
40 GB pull blocks silently for as long as it takes with zero feedback. Token
streaming for chat/generate only pays off once there's a consumer that can
render partial output incrementally — `solx exec` and `.solx script` both
print one final JSON blob, so on the CLI side there's no payoff yet even with
the built-ins in place. SSE through `solx-server` is the consumer that would
actually use it.
