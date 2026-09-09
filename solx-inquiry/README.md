# solx-inquiry

Ask a question, get a grounded answer — or give an instruction, and get back
prose, memories and runnable scripts. One `wasm32-wasip2` component
(`bin/solx-inquiry.wasm`) backs two registered actions:

| action | in | out |
|---|---|---|
| [`inquire`](#inquire) | a question | one grounded prose answer |
| [`instruct`](#instruct) | an instruction | responses, save-ready memories, and `.solx` scripts |

`instruct` is not a wrapper around `inquire`. It reuses this package's search,
merge and enrichment code directly rather than calling `inquire` as an action,
which is why it lives here.

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

## inquire

`inquire` runs a three-phase pipeline:

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
finishes inside one action call. `instruct` keeps that property — it is a
wider sequence, not a loop — but it is **not** stateless: it reads and writes
a session document. See [Session documents](#session-documents).

Both chat calls (phase 1 and phase 3) go through [`src/llm.rs`](src/llm.rs)
rather than a plain nested `exec`. See "Detached llm calls" below for why.

### Usage

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

### Parameters

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

### Detached llm calls

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

### Search-term parsing

Phase 1 asks for `format: {"type":"object","properties":{"terms":{...}}}`, so
a schema-compliant model answers with exactly `{"terms": [...]}`. Not every
model honors `format` (older Ollama models predate schema-constrained
decoding), so [`terms::parse_terms`](src/terms.rs) also falls back to a bare
JSON array, a ```-fenced code block containing either shape, and finally a
plain newline/comma-separated list with common markers (`-`, `*`, `1.`)
stripped. Only a genuinely empty or unparseable response fails the call
(`kind: "bad_llm_output"`).

### Merging hits

The same document or action can be found by more than one search term.
Hits are keyed by `(source, path, name)`; a repeat folds into the existing row
and appends the new term to `matched_terms` rather than producing a duplicate.
Merging happens through a `BTreeMap`, not a `HashMap`, so any hit that still
ties on score sorts deterministically rather than by hash-randomized
iteration order.

Both `search_documents` and `search_actions` order their `items` by FTS5
rank server-side, but neither exposes that rank as a number on the row —
only the array position carries it. So each term contributes its position's
reciprocal rank (`1/(position+1)`: first place `1.0`, second `0.5`, third
`0.33`, ...) rather than a flat value or a raw bm25 score. It also means a
`scope: "both"` result doesn't have documents automatically dominate actions
(or vice versa): both sides are scored the same way, rather than a real
bm25-derived document score being compared directly against an unrelated
action ranking scheme.

**A hit's `score` is the sum of those contributions across every term that
found it** — Reciprocal Rank Fusion. Keeping the single best contribution
instead throws corroboration away exactly where the terms are weakest: a
document ranked #1 under one generic word scores `1.0` and beats a document
ranked #2 under the precise term at `0.5`, however many other terms also
found the second one. That is not hypothetical, because
[`terms::expand_terms`](src/terms.rs) deliberately searches the individual
words of a phrase, and a common word like "list" will rank *something* first
whether or not it is relevant. Summing makes every additional term that found
a hit add evidence rather than being discarded.

So `score` is unbounded above rather than capped at `1.0` — with three terms a
hit can reach `~1.83`. It is a comparison within one result set, not a
confidence. The final sort still breaks an exact numeric tie by
`matched_terms.len()`, but that is now only a tiebreak: corroboration is
already the first key's doing.

`search_documents`/`search_actions` AND every word *within* one `q` string
together (all of them must co-occur in the same result), so a multi-word
search term is narrower, not broader. That's why the default prompt asks the
model for single keywords rather than 2-4 word phrases — a phrasal term can
silently zero out recall against real documents that only contain some of
its words.

Asking is not enough on its own, so `instruct` also expands what comes back
(`inquire` does not — it returns its terms to the caller as part of its
documented result, and quietly rewriting them there would misreport what the
model said). `max_terms` is a budget of *search calls*, so
[`terms::expand_terms`](src/terms.rs) spends it in priority order: the model's
own single keywords, then the individual words of every multi-word term, then
the phrases themselves if there is room left. Words come before phrases
because they are what carries the reach — a document matching a whole phrase
matches each of its words too, so the words' results are a superset of the
phrase's, and a phrase only ever adds ranking. Ordered the other way round
this was a no-op in the case it exists for: the intent schema caps `terms` at
`max_terms`, so a model answering entirely in phrases filled the budget before
a single word could be added, and every one of those phrases matched nothing.

### Content enrichment

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
alongside the category/capabilities/phrases/paramTypeRef/resultTypeRef already
on hand from `search_actions` (no extra call for those). `capabilities` is
where `solx:destructive` shows up — the one tag solx-core enforces rather than
merely records, since it makes a call stop for a human decision. This is what makes
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

A type reference is fetched at most once per [`search::TypeCache`](src/search.rs),
so two actions sharing a `paramTypeRef` — or the same action surfaced by more
than one of `instruct`'s inquiries — pay for the schema once, not once per hit
that names it. `inquire` builds a fresh cache per call; `instruct` builds one
per run and shares it across every inquiry's search, since that is where
duplication across up-to-three inquiries actually happens. The cache lives
only for that one call or run — a wasm guest instance carries nothing between
separate `exec` invocations, so this is not a persistent cache and cannot be.
A failed or absent reference is cached too, so it is not retried within that
same call or run.

`inquire` does not teach `.solx` syntax to the model anywhere, so a caller
wanting *it* to produce a runnable script has to supply that via a
`summary_prompt` override. `instruct` solves the same problem the other way
round and more safely: the model returns structured steps and this package
renders the syntax — see [Scripts](#scripts).

## instruct

`instruct` takes an instruction rather than a question, and answers with three
things a caller can act on: **responses** (prose), **memories** (responses the
model judged worth keeping, returned ready to save) and **scripts** (`.solx`
text, ready to run).

It saves nothing and runs nothing. Deciding which memories are worth keeping
and which scripts are worth running is the caller's, which is what keeps the
thing that generates model output from also being the thing that acts on it.
The one exception is its own session document.

```text
recall (skills + memories)      no llm
intent                          1 detached llm call
  direct? -> assemble, write the session, return
inquiry fan-out                 N <= 3 detached llm calls, in parallel
assemble                        no llm
session write
```

That is **1 + N** model calls, four at the cap.

### Usage

```bash
solx exec /packages/solx-inquiry/instruct --json '{
  "instruction": "what do I know about authentication, and how would I search for it?",
  "model": "qwen3:4b",
  "session": "/solx-inquiry/sessions/my-thread",
  "memory_path": "/solx-inquiry/memories"
}'
```

Drop `memory_path` and the same call runs with memories off: no recall, no
returned payloads, everything else unchanged.

```json
{
  "intent": {
    "mode": "inquire",
    "inquiries": [
      { "kind": "documents", "question": "what is authentication here?", "terms": ["authentication", "session"] },
      { "kind": "actions", "question": "how do I search documents?", "terms": ["search"] }
    ]
  },
  "responses": [
    { "text": "Authentication issues a signed session token at login.",
      "memory": true, "citations": ["/notes/auth"], "inquiry": 0 }
  ],
  "memories": [
    { "path": "/solx-inquiry/memories", "name": "authentication-issues-a-signed-session-9f2c1b04",
      "typeRef": "/packages/solx-inquiry/InquiryMemory",
      "author": "/packages/solx-inquiry/instruct",
      "summary": "Authentication issues a signed session token at login.",
      "contents": { "text": "...", "tags": [], "instruction": "...", "session": "..." } }
  ],
  "scripts": [
    { "title": "Search for auth notes",
      "actions": ["/builtin/document/search_documents"], "destructive": [], "notes": [],
      "source": "$hits = exec /builtin/document/search_documents --json '{\"q\":\"auth\"}';\n" }
  ],
  "hits": [ "..." ], "notes": [], "errors": [], "warnings": []
}
```

A returned memory is a complete `entity_save_document` payload — pipe it
straight in. A returned script is `.solx` source; register it with
`save file` + `save action --json '{"actionType":"script", ...}'`, or run its
steps yourself from `steps[]`.

### Parameters

| param | default | notes |
|---|---|---|
| `instruction` | — (required) | what the user wants, in natural language |
| `model` | — (required) | used for the intent call and every inquiry |
| `session` | — (required) | full `/path/name` doc ref; created on first use |
| `memory_path` | — (off) | where memories are recalled from and stamped on the returned payloads. **Omit it and memories are off entirely** |
| `max_inquiries` | 3 | **clamped to 3** — see below |
| `max_terms` | 5 | clamped to 10; per inquiry |
| `max_results` | 10 | clamped to 50; per inquiry |
| `document_path_prefix` | — | restricts document inquiries only |
| `action_path_prefix` | — | restricts action inquiries only |
| `type_ref` | — | document inquiries only |
| `skills_path` | `/solx-inquiry/skills` | where skills are read from |
| `recall_limit` | 5 | clamped to 20 |
| `history_limit` | 6 | prior turns summarized into the intent prompt; clamped to 20 |
| `intent_prompt`, `document_prompt`, `action_prompt` | [`src/prompts.rs`](src/prompts.rs) | replace the defaults |
| `llm_action_ref`, `base_url`, `api_key`, `auth_secret_name`, `headers`, `timeout_secs` | — | as for `inquire` |

`max_inquiries` is clamped rather than merely defaulted. Each inquiry is a
detached llm call echoing into this action's console, and the whole run is
bounded by one `timeout_secs` on the action row (8400s); three is what that
budget was sized for.

### The intent phase

One call decides whether the instruction can be answered from what is already
in hand (`mode: "direct"`) or needs looking up (`mode: "inquire"`), and if so,
what to look up. A schema-constrained answer names each inquiry's `kind`
(`documents` or `actions`), its `question`, **its own search terms**, and
optionally a `prompt` amendment for that one inquiry.

Requiring `terms` here is what keeps the pipeline at `1 + N` calls rather than
`1 + 2N`: without it every inquiry would need a term-generation call of its
own, which would not only double the budget but *serialize the fan-out*, since
those calls would have to happen before the searches they feed. A model that
ignores the schema anyway falls back to
[`terms::fallback_terms`](src/terms.rs) — a local stopword split, not another
model call.

The amendment **appends** to the default inquiry prompt; it never replaces it,
so the intent phase can steer an inquiry but cannot talk the pipeline out of
its own grounding rules. A caller who genuinely wants replacement uses
`document_prompt` / `action_prompt`.

Two deliberate leniencies in parsing the result:

- **The content decides the mode, not the declared word.** `"inquire"` with an
  empty inquiry list is a direct answer, and `"direct"` alongside real
  inquiries is not a reason to throw that work away.
- **Unparseable output is a direct answer**, not a failure. A model that
  ignored `format` still said something, and that becomes a response like any
  other. Failing instead would turn a chatty model into a broken pipeline.

An answer and a set of inquiries are **not alternatives**. A model that says
something *and* names what to look up has produced both, so the answer is
returned as a response (with `inquiry: null`) alongside whatever the fan-out
finds. The mode is only a reading of whether there is work to do. One
consequence: "every inquiry failed" is measured against what the *inquiries*
produced, not against `responses` being empty — a direct answer that happened
to arrive alongside them is not evidence that the work they asked for
happened.

### The inquiry fan-out

Every inquiry searches first, then all of them are started back to back with
`/builtin/action/start` before anything is waited on — that ordering is what
makes them concurrent, and there is a test that fails if a poll ever appears
between two starts.

The loop that drives them ([`src/fanout.rs`](src/fanout.rs)) differs from
`inquire`'s single-call loop in three ways that all follow from there being
more than one child:

- **Cancellation is checked first, and stops every outstanding child**, not
  just the one being polled.
- **One `console/tail` covers all of them.** They share an `action_ref`, and a
  console is keyed by `action_ref` alone. This is also the loop's pacing,
  since `tail` waits when there is nothing to read.
- **Each child is polled without `wait_secs`**, which returns immediately, so
  no child's completion is held up behind another's.

That last point creates a trap the first two do not: the shared console also
carries lines from *unrelated* concurrent callers of the same chat action, and
`tail` returns the instant any entry exists. On a busy console it therefore
never waits and the loop would spin, so when a tail returns entries of which
none are ours and nothing finished, one child is long-polled briefly instead.

**A failed inquiry fails alone.** It lands in `errors[]` and the others run on
— a fan-out that sank everything because one call errored would be strictly
worse than running them one at a time. Only *every* inquiry failing fails the
run.

If `action_start` is refused for lack of a long-lived host (a bare `solx exec`
— see [Detached llm calls](#detached-llm-calls)), the inquiries run
sequentially instead. Correct, just not concurrent, and without live console
echo or cooperative cancellation. The fallback is restricted to the *first*
start attempt, because `is_long_lived_host` is process-wide: if job 0 started,
job 1 cannot be refused for that reason, so nothing can be run twice.

### Scripts

An action inquiry does **not** write `.solx`. It returns structured steps —
`{action_ref, params, capture}` — under a `format` schema, and
[`src/script.rs`](src/script.rs) renders the text. That division eliminates
the two ways generated `.solx` goes silently wrong, rather than explaining
them in a prompt and hoping:

- **Quoting.** A `--json` argument is single-quoted, and an *unescaped* `'`
  inside it does not error — it ends the argument right there, and the rest of
  the line becomes unrelated bare tokens that still parse as a valid but
  completely different statement. (`solx-core/solx-scripts/src/lib.rs` records
  this as having broken a real package's `install.solx`.) One tested function
  can now get that wrong instead of every model call.
- **Invented actions.** A step naming an action that does not exist fails at
  run time, long after a plausible-looking script was handed over. Any step
  whose `action_ref` was not among the actions that inquiry's own search
  surfaced is dropped, with the reason recorded in the script's `notes`; a
  script left with no steps is discarded rather than returned empty.
- **Capture references.** `.solx` substitutes `$name` textually *and by the
  captured value's runtime type*: a captured string is inserted raw and
  unquoted, everything else as JSON. So a reference has to be spelled
  `"$name"` where the parameter wants a string and bare `$name` where it wants
  an object, array or number — and the wrong one produces a `--json` body that
  fails to parse at run time. `script.rs` picks the spelling from the callee's
  own `paramSchema`, walking down alongside the value so a nested parameter is
  judged by its own declared type. That schema is already on the hit (see
  [Content enrichment](#content-enrichment)), so this costs no extra call.
  With no schema to consult the reference stays quoted — right for the scalar
  case a model writes most often — and the script's `notes[]` says which
  references were rendered on that assumption.

  Worse than the wrong spelling is a reference to a capture that does not
  exist: solx leaves an undefined `$name` in the text verbatim, so the action
  receives the literal string `"$name"` and either fails late or, quietly,
  succeeds against the wrong input. A step referencing a capture no *earlier
  surviving* step defines is dropped, exactly like one naming an invented
  action. Only a whole-value reference counts as one — a `$` inside longer
  prose is left alone, since a parameter containing a `$` is far more likely
  than one deliberately splicing a capture mid-sentence.

Only `exec` stages are emitted, which is exactly what a `script`-typed action
supports. There is no `return` — a script evaluates to its last statement,
which is why the prompt asks for the answering step last.

**Destructive actions are marked, not refused.** A script containing a step
solx-core would stop for a human decision lists that action in `destructive[]`
and says so in `notes[]`, and the marker reaches the model too — it is told to
pick such an action only when the instruction actually asked for it. Deleting
something can be exactly what was asked for, and this pipeline runs nothing;
but a caller told to run these scripts should not have to read the `.solx` to
discover one of them deletes an action. Not hypothetical: a live 4B run
answered "list every installed action" by proposing `entity_delete_action`.

The check is honest about its own limit. `solx-config`'s
`ToolPolicy::is_destructive` derives that verdict from three things — the
`solx:destructive` capability, an `actionType` of `command` or `webhook`
(unconditionally: shell and outbound HTTP), and a configured
`tool_destructive` list. `search_actions` returns the first two on the row, so
both are checked. **The third is invisible from inside a wasm guest**: it lives
in `solx-config.json` and no built-in action exposes the policy. So
`destructive[]` means "what could be seen from here", not "everything that
will stop" — and the built-ins are the live example of the gap, since they are
seeded with empty `capabilities` and an `internal` `actionType` and therefore
trip neither available check.

The model is still told what it is authoring for
(`prompts::SOLX_SCRIPT_PRIMER`: captures, ordering, no control flow), and the
full `.solx` primer is seeded as a skill at
`/solx-inquiry/skills/solx-scripts` for a human reading a returned script.

### Skills and memories

Both are ordinary documents, and both are recalled with **one call apiece**,
once per run, before the fan-out — so three parallel inquiries share one lookup
instead of repeating it three times. Neither needs a follow-up
`entity_get_document`: both calls return full rows (so a skill's `contents` is
already on the hit) and a memory's text is written into its `summary` (so
recall reads the row directly).

Neither call passes a `q`. `solx-docs`' `fts_match_query` prefix-matches and
ANDs every whitespace-separated term, so handing it an instruction would load
a skill only if that skill's text contained *every* word of it. Selection is
by path prefix, type, and a skill's declared `scope`.

That leaves *ordering* to decide which ones fit under the limit, and the two
want different answers. Skills are read with `search_documents`, whose no-`q`
path orders by `path, name` — stable and predictable for operator-written
guidance competing for one budget. **Memories are read with
`entity_list_documents`, sorted `updated_at` descending**, because the same
alphabetical order is actively wrong for them: a memory's name is a slug of
its own text, so past `recall_limit` memories a session would surface the same
arbitrary five forever and never see anything written since. `list` is the
same single call and exposes `sortBy`, so recency costs nothing. Its type
filter is a substring match rather than an exact one, so the returned rows are
checked against the memory type again in `recall.rs`.

**Skills** (`/solx-inquiry/skills`, type `InquirySkill`) are operator-written
guidance: `{scope, instructions, tools?}`. `scope` is `documents`, `actions`
or `both` — an absent or unrecognized value means `both`, so a typo makes a
skill apply too often rather than silently never. An actions-scoped skill with
`tools` globs only rides along with an inquiry whose search surfaced a
matching action. Six are seeded by `install.solx`; they are package content,
so `uninstall.solx` removes them.

**Memories** (type `InquiryMemory`) are responses a model flagged as durable.
`instruct` returns the payload; the caller saves it.

**Memories are off unless `memory_path` is given.** Omit it and nothing is
recalled — there is not even a search call — and no payload is minted.

There is deliberately no default, because there is no sensible guess to make.
The value is the caller stating where this pipeline's own past output lives,
which is what lets that output be kept out of a later inquiry's evidence (see
below); guessing wrong means a previous run's answers get read back as though a
person had written them. Off is the safe reading of "not specified"; inventing
a location is not. The same value is used for recall *and* stamped on the
returned payloads, so the two cannot disagree — save them where you said they
would go.

**Only a response an inquiry produced can become a memory.** The intent phase
sees skills, memories and history and no search results at all, so a `direct`
answer is model prior: it cites nothing and nothing checked it. Minting one
would write it down and hand it to a later run looking exactly like a grounded
finding — the same loop [this package's own documents are never
evidence](#this-packages-own-documents-are-never-evidence) closes from the
other side, reached through a different door. So a direct answer flagged
`memory` is reported and explained in `notes[]`, never minted.

With memories off, a response still carries the model's `memory` flag: that is
its judgement about the finding, and it is the one thing that tells you whether
switching memories on would be worth anything. If any mintable response was
flagged, a `notes[]` entry says how many, so an empty `memories[]` never reads
as "nothing was worth keeping".

Every document `instruct` writes — the session, and each returned memory —
carries `author: /packages/solx-inquiry/instruct`. That is provenance for
whoever reads the document later: a memory is model output, and a document that
does not say so looks exactly like something a person wrote. **Nothing in this
pipeline branches on it.** Keeping model output out of evidence is the declared
`memory_path`'s job, not an inference from a field a caller is free to change.
The name is a readable slug plus a short FNV-1a hash of the text —
deterministic, because a wasm guest has no random source, and useful, because
`entity_save_document` upserts on `(path, name)`, so re-deriving the same
memory overwrites itself instead of accumulating near-duplicates every time
the instruction is repeated.

Both enter a prompt framed as reference material, never as instruction. A
memory is model-written text re-entering a prompt; a skill is operator-written
guidance. Neither is a fact and neither decides an answer.

### This package's own documents are never evidence

An inquiry's search results have every hit dropped that is `solx-inquiry`'s own
bookkeeping — by **path** (under `skills_path`, or under `memory_path` when one
was given) and by **type**, which covers what path cannot: a session, since the
caller names its full reference and it can live anywhere, and a memory written
by an *earlier* run, which must stay out of evidence even on a run with
memories switched off and no path to compare against. Both tests are needed and
both were learned the hard way, live:

- **Skills and memories** already reach the model through recall. Re-admitting
  them as search hits double-counts them and promotes them from guidance to
  evidence. A run searched "authentication", matched the seeded `memories`
  skill — whose text includes a worked example of a well-written memory, which
  mentions session tokens — and reported that *example* as a finding, cited to
  the skill.
- **Sessions** are worse, because they close a loop. Every run records its
  responses in a session document whose `title` and `summary` are indexed, so
  the next inquiry finds it and cites a previous run's model output as fact.
  The same fabricated sentence came back a second time, now cited to the
  session that had recorded it. A session has no fixed path — the caller names
  its full reference — so only the type check catches it.

Model output must not become evidence by having been written down, and
guidance is not a document about the subject it happens to teach with.

### Session documents

`session` is a full `/path/name` reference, so sessions can live wherever the
caller wants. The document is read before the intent call (its recent turns
become history in that prompt) and rewritten once at the very end, with this
turn appended and the oldest dropped past 50.

The `InstructSession` type declares `turnCount` and `lastInstruction` and
pointedly **does not declare `turns`**. `solx-docs` computes a document's
full-text content by walking only the fields its type declares, so an
undeclared `turns` is stored and returned but never enters the FTS index —
which is what keeps rewriting this document on every instruction affordable.
`title` and `summary` *are* indexed, which is what lets a session be found
later by what it was about.

The write is deliberately last, and a failure to write is a `warnings[]` entry
rather than an error: by then the results already exist, and discarding a
completed instruction over its own bookkeeping would be the worse outcome. A
session document that does not exist yet is an empty session, not an error.

### Console tags

Every milestone is printed to the action's own console with both a tagged
message and a machine-readable `data` object, so a run can be reconstructed
from `/builtin/console/read` without re-running it. The grammar is
`[instruct:<phase>]` or `[instruct:inquiry:<index>:<step>]`, and it is defined
in [`src/console.rs`](src/console.rs) and nowhere else.

There is no `solx console` subcommand; the console is read through the
built-in action:

```bash
solx exec /builtin/console/read --json '{"action_ref":"/packages/solx-inquiry/instruct","limit":50}'
```

| tag | `data` |
|---|---|
| `[instruct:recall]` | `{skills: [refs], memories: [refs]}` |
| `[instruct:intent]` | the parsed intent object |
| `[instruct:inquiry:<i>:terms]` | `{kind, question, terms}` |
| `[instruct:inquiry:<i>:hits]` | `{count, refs}` |
| `[instruct:inquiry:<i>:result]` | `{responses}` or `{scripts, notes}` |
| `[instruct:inquiry:<i>:error]` | the failure's own error object |
| `[instruct:result]` | `{responses, memories, scripts, errors, warnings}` |

Echoed child lines keep the `[instruct:inquiry:<i>]` prefix, so live model
output is attributable to the inquiry that produced it.

### Why there is no result phase

The three phases are intent, inquiry and result — and the result phase does no
model call. Each inquiry's findings are returned as they are.

A synthesis call would cost a fifth round trip to re-say what the responses
already say, and it would be the one place in this pipeline where a model
could contradict its own grounded output with nothing to check it against.
Every other model call here is anchored to something: search hits, or a
parameter schema. A caller who wants one answer out of several can add that
layer — with `inquire`, or with another `instruct` — over output that is still
individually cited.

## Errors

A failed call returns `success: false` with a machine-readable `result`
object carrying `kind`, `error`, and `stage` — `"terms"`, `"search"` or
`"summary"` for `inquire`; `"intent"`, `"inquiry"` or `instruct:inquiry:<i>`
for `instruct`:

| `kind` | meaning |
|---|---|
| `bad_params` | `inquiry`/`model` missing or `scope` invalid; for `instruct`, `instruction`/`model`/`session` missing or `session` not a `/path/name` ref |
| `dispatch_error` | the host rejected a nested call (e.g. `llm_action_ref` isn't installed, or `action_poll`/`action_start` itself failed) |
| `llm_error` | the llm action ran but reported failure (bad model, auth, transport, ...); its own output is under `inner` |
| `inquiry_error` | `instruct` only: *every* inquiry failed. Each entry in `errors` carries its own kind, since the causes can differ |
| `search_error` | `search_documents`/`search_actions` reported failure; carries `term` and `inner` |
| `bad_llm_output` | the model's response had no extractable search terms, or an empty summary. `instruct` does not raise this: unparseable intent output becomes a direct answer, and unparseable inquiry output becomes an uncited response or a note |
| `cancelled` | the action's own invocation was stopped mid-call; every outstanding child chat invocation was stopped too. `instruct` carries what it had under `partial` |
| `unknown_action` | the row's `fn_name` is neither `inquire` nor `instruct` |

Two failures `instruct` deliberately does **not** raise, because they are
partial rather than total:

- **One inquiry failing** (a failed search, a failed model call, a refused
  `action_start`) lands in `errors[]` and the run continues.
- **A session document that could not be written** lands in `warnings[]`. The
  results already exist by then.

Neither is a memory that was refused for being ungrounded, either: a direct
answer flagged `memory` produces a `notes[]` entry, not an error. The run
succeeded; one thing the model wanted kept was not kept, and it says so.

## Layout

```
src/lib.rs             dispatch on fn_name ("inquire" / "instruct")
src/host.rs            the Host trait - the seam that keeps everything host-testable
src/guest.rs           wit-bindgen shim (wasm32 only)

shared by both actions
src/llm.rs             drive one chat call: detached start/poll/tail/cancel, or a blocking fallback
src/prompts.rs         every default prompt and every structured-output schema
src/search.rs          run search_documents/search_actions, normalize, merge, enrich
src/terms.rs           generate search terms from a model, or derive them locally
src/params.rs          parse/default an inquire call's params, and the shared llm overrides

inquire
src/summarize.rs       summarize the merged hits against the inquiry

instruct
src/instruct.rs        the orchestrator, and minting save-ready memory payloads
src/instruct_params.rs parse/default an instruct call's params, and every cap
src/recall.rs          skills and memories: two lookups, and the blocks they become
src/intent.rs          decide: answer directly, or name what to look up
src/inquiry.rs         one inquiry's search, its payload, and parsing what comes back
src/fanout.rs          run N chat calls at once: start all, then poll/tail/cancel as one
src/script.rs          render structured steps into .solx, dropping unsurfaced actions
src/session.rs         read the session for history, write it back with this turn
src/console.rs         the [instruct:...] console tag vocabulary

tests/dispatch.rs      the inquire pipeline, against a FakeHost
tests/instruct.rs      the instruct pipeline, against a FakeHost that scripts concurrent children
wit/                   vendored copy of solx-core/solx-wasm/wit/custom-action.wit
```

The WIT is vendored so the package builds without a sibling `solx-core`
checkout. A test asserts it has not drifted, but only when that checkout is
actually present.
