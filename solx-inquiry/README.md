# solx-inquiry

Ask a question, get a grounded answer. One `wasm32-wasip2` component
(`bin/solx-inquiry.wasm`) backs a single registered action, `inquire`, which
runs a three-phase pipeline:

1. **Search terms.** The inquiry is sent to a chat action (by default
   `solx-ollama`'s `ollama-chat`) with a JSON-schema `format`, asking for a
   short list of search terms.
2. **Search.** Each term is run against `/builtin/document/search_documents`
   and/or `/builtin/action/search_actions` (per `scope`). Hits are
   normalized to one shape, deduplicated across terms (keeping the best
   score), and capped at `max_results`.
3. **Summary.** The inquiry plus the merged hits go back to the chat action,
   which answers in plain text, grounded in — and expected to cite — those
   hits.

This is a single straight-line sequence with no interactive loop, which is
exactly what one guest invocation is good for: unlike an agent harness that
has to hold state across turns a user drives, this pipeline starts and
finishes inside one action call.

Both chat calls (phase 1 and phase 3) go through [`src/llm.rs`](src/llm.rs)
rather than a plain nested `exec`. See "Detached llm calls" below for why.

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
`bin/solx-inquiry.wasm`. `install.solx` reads that path as its very **first**
statement, so if you have not built yet the install aborts with a plain
"cannot find the file" error before writing a single type or action row.

`cargo test` runs the pipeline tests on the host target (not wasm), against a
`FakeHost` that replays canned nested-`exec` responses — no Ollama server,
network, or wasm runtime needed. It works because the wit-bindgen surface
lives in `src/guest.rs` behind `#[cfg(target_arch = "wasm32")]`; everything
else is written against the `Host` trait in `src/host.rs`.

**Requires a chat action to already be installed** — by default
`solx-ollama`'s `ollama-chat` (`/packages/solx-ollama/ollama-chat`). Install
that package first, or point `llm_action_ref` at another action taking
`{model, messages, format?}` and returning Ollama-shaped
`{message: {content}}`.

## Usage

```bash
solx exec /packages/solx-inquiry/inquire --json '{
  "inquiry": "how does authentication work in this codebase?",
  "model": "qwen3:4b"
}'
```

```json
{
  "inquiry": "how does authentication work in this codebase?",
  "model": "qwen3:4b",
  "scope": "documents",
  "terms": ["authentication", "session", "login"],
  "hits": [
    { "source": "document", "path": "/notes", "name": "auth", "title": "Auth notes",
      "summary": "how login works", "score": 1.0, "matched_terms": ["authentication"],
      "details": { "body": "Sessions are issued a signed token at login..." } }
  ],
  "summary": "Authentication uses session tokens issued at login, per /notes/auth."
}
```

## Parameters

| param | default | notes |
|---|---|---|
| `inquiry` | — (required) | the question or topic |
| `model` | — (required) | passed to the llm action for both phases |
| `scope` | `documents` | `documents`, `actions`, or `both` |
| `max_terms` | 5 | clamped to 10 |
| `max_results` | 10 | clamped to 50; caps the *final*, merged-and-ranked hit list — each term's own search still fetches at least 10 candidates regardless, so a small `max_results` narrows the answer, not the candidate pool |
| `path_prefix` | — | restricts both search calls |
| `type_ref` | — | restricts document search only |
| `inquiry_prompt` | [`DEFAULT_INQUIRY_PROMPT`](src/prompts.rs) | overrides the search-term system prompt |
| `summary_prompt` | [`DEFAULT_SUMMARY_PROMPT`](src/prompts.rs) | overrides the summary system prompt |
| `llm_action_ref` | `/packages/solx-ollama/ollama-chat` | swap in a different chat action |
| `base_url`, `api_key`, `auth_secret_name`, `headers`, `timeout_secs` | — | forwarded verbatim to every llm call |

## Detached llm calls

Each of the two chat calls is driven by [`llm::call`](src/llm.rs), which
tries `/builtin/action/start` before falling back to a plain blocking `exec`.
Started detached, the chat call runs as its own invocation and this pipeline
loops: check whether `inquire`'s own invocation has been asked to stop,
long-poll the chat call's status (`/builtin/action/poll`, 5s at a time), then
drain and re-print whatever it wrote to its own console
(`/builtin/console/tail` -> `/builtin/console/print`, prefixed `[terms]` /
`[summary]`) — so an operator tailing `inquire`'s console sees the nested
chat call's progress live, not just the final answer. If the cancellation
check trips, the child invocation is stopped (`/builtin/action/stop`) and
`inquire` returns `kind: "cancelled"`.

Two things send a call down the plain blocking path instead:

- **The host is not long-lived.** `action_start` refuses under a bare
  `solx exec` — the process exits the instant `exec` returns, which would
  kill a spawned task before anyone could poll it. Only `solx-server`/
  `solx-mcp` (or a CLI pointed at one via `server_url`) allow it. `inquire`
  detects the refusal by message and falls back transparently, so it still
  works standalone — just without live console echo or cooperative
  cancellation, the same as before this existed.
- **`llm_action_ref` doesn't parse as a `/path/name` reference** (no `/`, or
  an empty path/name segment) — `action_start` takes a path/name pair, not a
  joined ref, so a malformed override falls back rather than hard-failing.

One consequence worth knowing: `inquire`'s own cancellation check only does
anything if `inquire` *itself* was started via `action_start` (only then does
its invocation have a row a `stop` call can flip) — a plain `solx exec
inquire` degrades silently to "never cancelled," same as `solx-ollama`'s
existing `/builtin/action/cancelled` check does today.

## Search-term parsing

Phase 1 asks for `format: {"type":"object","properties":{"terms":{...}}}`, so
a schema-compliant model answers with exactly `{"terms": [...]}`. Not every
model honors `format` (older Ollama models predate schema-constrained
decoding), so [`terms::parse_terms`](src/terms.rs) also falls back to a bare
JSON array, a ```-fenced code block containing either shape, and finally a
plain newline/comma-separated list with common markers (`-`, `*`, `1.`)
stripped. Only a genuinely empty or unparseable response fails the call
(`kind: "bad_llm_output"`).

## Merging hits

The same document or action can be found by more than one search term.
Hits are keyed by `(source, path, name)`; a repeat keeps the higher score and
appends the new term to `matched_terms` rather than producing a duplicate row.
Merging happens through a `BTreeMap`, not a `HashMap`, so any hit that still
ties on score sorts deterministically rather than by hash-randomized
iteration order.

Both `search_documents` and `search_actions` order their `items` by FTS5
rank server-side, but neither exposes that rank as a number on the row —
only the array position carries it. So every hit's score, document or
action alike, is its position's reciprocal rank (`1/(position+1)`: first
place scores `1.0`, second `0.5`, third `0.33`, ...) rather than a flat
value or a raw bm25 score. That's what lets a genuinely first-ranked hit
outrank a fifth-ranked one after merging across terms *and* sources,
instead of every hit tying and the "tiebreak" being whatever order they
happened to end up in — and it's also why a `scope: "both"` result doesn't
have documents automatically dominate actions (or vice versa): both sides
are scored the same way now, rather than a real bm25-derived document score
being compared directly against an unrelated action ranking scheme.

Because every score is a reciprocal rank, a genuine tie at the top (1.0) is
common, not rare — several unrelated hits can each legitimately rank #1
under their own single, generic search term (e.g. "list", "install"). The
final sort breaks such ties by `matched_terms.len()` (descending): a hit
corroborated by more of the model's terms outranks one that only ever
matched a single word, instead of falling back to `BTreeMap` key order
(alphabetical by path), which carries no relevance signal at all.

`search_documents`/`search_actions` AND every word *within* one `q` string
together (all of them must co-occur in the same result), so a multi-word
search term is narrower, not broader. That's why the default prompt asks the
model for single keywords rather than 2-4 word phrases — a phrasal term can
silently zero out recall against real documents that only contain some of
its words.

## Content enrichment

`summary`/`caption` alone is often not enough to answer from, especially for
actions, where constructing a genuinely correct call needs the parameter
schema, not just a reference to where it lives.

A document hit needs no enrichment step at all: `search_documents` returns
full `Document` rows, so `contents` is already sitting on the hit as
`details` the moment it comes back from the search call — no second round
trip. (This used to require a separate `entity_get_document` fetch, back
when `search_documents` only returned a slim `title`/`summary` projection;
solx-core's `DocManager::search` was changed to return full rows, mirroring
what `search_actions` already did, specifically to remove that round trip.)

An action hit is different: `search_actions` returns the full `Action` row
too, but a `paramTypeRef`/`resultTypeRef` on it is only a *reference* to
where a schema lives, not the schema itself. So after merging and capping,
[`search::enrich_hits`](src/search.rs) fetches the JSON Schema for whichever
of `paramTypeRef`/`resultTypeRef` are present (each its own `entity_get_type`
call), folding them into `details.paramSchema`/`details.resultSchema`
alongside the category/phrases/paramTypeRef/resultTypeRef already on hand
from `search_actions` (no extra call for those). This is what makes
`scope: "actions"` genuinely useful for a caller building a script or exec
payload from the result, rather than just describing what an action does.
`resultTypeRef` is documentation only — the host never validates an
action's actual output against it — so a present `resultSchema` describes
the *intended* shape, not a verified guarantee.

Every fetch is independent and best-effort: a failure (the type was
deleted, a transient error) just leaves that piece of `details` missing — a
failed `resultSchema` fetch, say, never takes `paramSchema` down with it —
rather than failing the inquiry. `details` is truncated to 1500 chars in the
summarizer prompt so one large blob cannot crowd out every other hit; the
full, untruncated value is still in the returned `hits[].details`.

Note that `.solx` script syntax itself (`save`/`exec`/`;`/`|`/`$var`) is not
taught to the model anywhere in this pipeline — a caller wanting `inquire` to
produce a runnable script needs to supply that via a `summary_prompt`
override; `details.paramSchema` gives the model what it needs to fill in
correct parameters, not how to wrap them in `.solx` syntax.

## Errors

A failed call returns `success: false` with a machine-readable `result`
object carrying `kind`, `error`, and `stage` (`"terms"`, `"search"`, or
`"summary"`):

| `kind` | meaning |
|---|---|
| `bad_params` | `inquiry`/`model` missing, or `scope` invalid |
| `dispatch_error` | the host rejected a nested call (e.g. `llm_action_ref` isn't installed, or `action_poll`/`action_start` itself failed) |
| `llm_error` | the llm action ran but reported failure (bad model, auth, transport, ...); its own output is under `inner` |
| `search_error` | `search_documents`/`search_actions` reported failure; carries `term` and `inner` |
| `bad_llm_output` | the model's response had no extractable search terms, or an empty summary |
| `cancelled` | `inquire`'s own invocation was stopped mid-call; the child chat invocation was stopped too |
| `unknown_action` | the row's `fn_name` isn't `inquire` |

## Layout

```
src/lib.rs        dispatch on fn_name (just "inquire"), wires the three phases together
src/host.rs        the Host trait - the seam that keeps everything host-testable
src/params.rs       parse/default an inquire call's params
src/prompts.rs      default prompts + the search-term JSON schema
src/llm.rs          drive one chat call: detached start/poll/tail/cancel, or a blocking fallback
src/terms.rs        phase 1: generate and parse search terms
src/search.rs       phase 2: run search_documents/search_actions, normalize and merge hits
src/summarize.rs     phase 3: summarize the merged hits against the inquiry
src/guest.rs        wit-bindgen shim (wasm32 only)
wit/                vendored copy of solx-core/solx-wasm/wit/custom-action.wit
```

The WIT is vendored so the package builds without a sibling `solx-core`
checkout. A test asserts it has not drifted, but only when that checkout is
actually present.
