# solx-conductor

An agent harness built out of the pieces solx already has. The action registry
*is* a tool catalogue — every row carries a name, a description, and a
`param_type_ref` pointing at a JSON Schema, which is what Ollama's tool format
wants. This package resolves a catalogue from a search query, hands it to a
local model through `solx-ollama`, and dispatches whatever the model asks for
back into solx.

One `wasm32-wasip2` component (built from `src/solx-conductor.js` via
solx-quickjs) backs **five** actions, selected by `fn_name`.

> **Status: not yet run against a live model.** The logic is complete and
> covered by 44 tests against a fake host, but no end-to-end run has happened.
> See "What is not done" at the end.

## Actions

| action | `fn_name` | does |
|---|---|---|
| `/packages/solx-conductor/conductor-start` | `start` | resolve a catalogue, seed the transcript, create the session |
| `/packages/solx-conductor/conductor-step` | `step` | run exactly one iteration |
| `/packages/solx-conductor/conductor-run` | `run` | step until done, capped, or stopped for approval |
| `/packages/solx-conductor/conductor-session` | `session` | read a session back |
| `/packages/solx-conductor/conductor-tools` | `tools` | preview everything a session would get, without starting one |

`step` being callable on its own is the point: a `.solx` script, another
package, or a human can drive the loop and interpose between iterations. `run`
is the convenience wrapper, not the primitive.

## Build and install

```bash
solx install-package .
```

`install.solx` stages `src/solx-conductor.js`, compiles it through
`/packages/solx-quickjs/build-javascript-file`, and registers its own rows
against the resulting `solx-conductor.wasm`. solx-quickjs must be installed
first, and its CLI must already be built — see that package's README.

`solx-ollama` must also be installed, with a model that supports tools.

## A first run

```bash
# See what the model would get, before starting anything.
solx exec /packages/solx-conductor/conductor-tools --json '{
  "allow": [{"path": "/packages/solx-media"}],
  "tool_query": "transcode video"
}'

solx exec /packages/solx-conductor/conductor-start --json '{
  "model": "qwen3:4b",
  "goal": "Summarise every document under /blogs written this year.",
  "allow": [
    {"path": "/builtin/document", "actions": ["search_documents", "entity_get_document"]}
  ],
  "memory": {"scope": "blogs"}
}'

solx exec /packages/solx-conductor/conductor-step --json '{"session_id": "wandering-heron"}'
```

Sessions are named in words rather than digits, because the id is typed into
every `step` call.

Each iteration prints to the action console, so a long `run` is tailable:

```bash
solx exec /builtin/console/tail --json '{"action_ref":"/packages/solx-conductor/conductor-run","wait_secs":30}'
```

## Four kinds of document

Everything the conductor knows lives in the document store, so it is authored,
searched, edited, and inspected with the tools solx already has — `solx save
document`, `solx search`, the UI. There is no private storage anywhere in this
package.

| kind | path | type | written by |
|---|---|---|---|
| Session | `/conductor/sessions` | `ConductorSession` | the conductor, every step |
| Memory | `/conductor/memories/<scope>` | `ConductorMemory` | the model, through a tool |
| Skill | `/conductor/skills` | `ConductorSkill` | you |
| Context | anywhere | anything | not the conductor |

Grouping by owner rather than by kind is deliberate. It makes the package
footprint one query — `solx search --path /conductor` — and it lets the gate
reserve a single prefix, so a fifth kind of document cannot be added and then
forgotten. (solx-livejournal goes the other way, writing to `/blogs/...`, and
that is right for documents *about the world*: a second harvester belongs
beside it, and the user cares about blogs rather than about which package
fetched them. These are machinery.)

### Sessions

A guest gets a fresh `Store` per `run()` — nothing survives between
invocations. So the transcript lives at `/conductor/sessions/<id>`
and every `step` is a read-modify-write. That is also what makes the loop
resumable, inspectable while it runs, and immune to the timeout stacking a
single long invocation would hit.

The allowlist is copied into the document at `start` and read from there on
every step, so a later caller cannot widen a running session by passing
different params to `step`.

Ids are an adjective and a noun — `wandering-heron` — from lists in
`src/names.js`, because the id is typed into every `step` call, pasted into
scripts, and read aloud. 16,120 pairs is not enough on its own: a name is a
document name, `entity_save_document` is an upsert keyed on `(path, name)`, and
a collision would not fail — it would silently write over a running transcript.
So `start` checks whether a name is free before taking it, widens to
`wandering-heron-a3f2` after four misses, and falls back to a timestamp that
cannot realistically collide. The check **fails closed**: a read that broke for
any reason other than a definite not-found counts as taken, because the next
thing that happens is a write.

Chronological order never depended on the name — documents carry `createdAt`,
so `solx list document --path /conductor/sessions --sort-by created_at` still
does what you want.

The session type deliberately does **not** declare `messages` or `calls` as
properties. The full-text index walks only a type's declared fields, so leaving
them out keeps them stored and validated while keeping the whole transcript out
of FTS — otherwise every step would re-index every turn written so far.

### Memories

**Off unless you name a scope** — there is no default. Without `memory.scope`,
nothing is recalled and the two memory tools are never offered, so the model is
not even told memory exists. That is because sessions sharing a scope read each
other's notes: which sessions pool their memory is a decision, not a default.

Within a scope, one run writes down what it learned and the next reads it
back.

```bash
solx exec /packages/solx-conductor/conductor-start --json '{
  "model": "qwen3:4b", "goal": "...", "allow": [...],
  "memory": {"scope": "blogs", "limit": 5, "max_writes": 20}
}'
```

At `start`, one search over the scope seeds the top matches as a system turn.
During the run the model reaches them through two built-in tools,
`sys__memory_search` and `sys__memory_save`.

The text is capped at 1000 characters and written to the document's `summary`
as well as its contents. That is not tidiness: a `search_documents` hit carries
`{id, path, name, title, summary, typeRef, score}` and no contents, so a memory
that fits in `summary` makes recall exactly one call with zero follow-up reads.

**A memory scope is a trust boundary.** What comes back is model-written text
re-entering a later prompt. It is framed as reference material rather than
instruction, and it can never widen what a session is allowed to do — the
allowlist and the gate are untouched by anything in a memory. But a wrong or
poisoned note will still mislead a later run. Keep unrelated tasks in separate
scopes, and read a scope occasionally:

```bash
solx search --path /conductor/memories/blogs
```

### Context

Documents you put within reach for one session:

```json
"context": [
  {"ref": "/specs/ingest-pipeline"},
  {"query": "house style", "path": "/docs", "limit": 3}
]
```

Only titles and summaries are injected. The model calls `sys__context_read`
with a ref to open one in full. That is why context is cheap: an entry
nominated by query costs one search and no reads, and a document the model
never needed costs nothing but a line in the index.

The resolved list is frozen at `start` and is the **only** thing
`sys__context_read` can open — the same rule the tool catalogue follows. It is
not a general document reader, so putting a document in `context` is the whole
grant, and nothing else in the store becomes readable.

### Skills

A skill document explains how to use particular tools, bound to action refs by
glob:

```bash
solx save document /conductor/skills/documents \
  --type /packages/solx-conductor/ConductorSkill --json '{
  "tools": ["/builtin/document/*"],
  "instructions": "Names are unique per path and entity_save_document is an upsert, so a save with an existing name replaces that document. Search first."
}'
```

It loads only when one of its globs matches a tool that is actually in the
session's catalogue. A skill therefore never grants reach — it only explains
reach that was already granted, which is why skills are on by default. There
simply are none until you write one.

At `start` the matching skills go in as a system turn. When
`sys__tool_search` adds tools mid-session, the skills covering those tools ride
back in the tool result, so nothing has to splice a system turn into the middle
of a transcript.

## Catalogue by search, not by listing

Every tool definition is prompt tokens on every iteration, and listing
everything an allowlist permits will drown a 4B model. `start` takes a
`tool_query` (defaulting to the goal), runs one search per allow prefix, and
freezes the resulting name→ref map into the session. The catalogue is capped
(16 by default) and `tools_dropped` records how many matches fell outside it,
so truncation is visible in the session rather than inferred from the model
behaving oddly.

`sys__tool_search` lets the model widen that later. It re-runs the same
resolution against the session's **frozen allowlist**, so it grows what the
model can *see* without touching what it may *call* — and it stops at
`catalogue_cap`. Set `"tool_search": false` to keep the catalogue fixed.

This is what makes a narrow start query the right default: offer three tools,
and let the model go looking when it finds it needs a fourth.

## Tool names

Catalogue names use solx-mcp's readable shape
(`act__builtin__document__search_documents`) so a transcript looks familiar,
but this package deliberately does **not** port its decoder. A name is resolved
through the session's own map, never parsed — which is stronger than decoding,
because the model cannot synthesize a valid name for an action that was never
listed. That is also why there is no base32 fallback here.

Built-in tools use a `sys__` prefix, which cannot collide.

## The gate

Layered, and split across two languages.

**Exclusions are enforced in Rust and never enter the guest.** Every catalogue
search and every dispatch-time lookup passes the exclude-hidden flag, so a
hidden action is already gone before this package sees it. Hidden-ness is
resolved in `solx-config` as config rules ∪ the row's own `solx:hidden`
capability — the same `ToolPolicy` solx-mcp uses. Nothing here knows the rules,
which means it cannot skip the check or get it subtly wrong in a second
implementation.

What is left in the guest is what is specific to *this* caller:

1. **Allow — default deny.** An absent or empty `allow` is an error, not an
   empty catalogue. There is no `"*"` shorthand; a caller who wants breadth
   writes the prefixes out.
2. **Structural denies — not overridable.** `/builtin/secrets/*` (secrets
   resolve against the conductor's own `action_config`, so exposing them hands
   the model the keys it runs under), `/builtin/action/*` (self-modification
   and detached spawning), `/builtin/env/set_env` (persists to
   `solx-config.json`), and this package itself.
3. **Command and Webhook need an exact name.** A glob never reaches a shell.
   `guard_executable_action` stops a guest *creating* one of these, but nothing
   stops a guest *executing* one that already exists.
4. **The whole `/conductor` root is unwritable by the model.**
   `entity_save_document`, `entity_delete_document`, `set_field` and
   `set_field_at_path` are refused there however wide the allowlist is, and so
   is a skills path pointed somewhere else. It is one prefix rather than one
   per kind, which is the point: the check cannot fall behind the document
   kinds. The session document holds `allow` and the dispatch table; a skill
   document is instruction injection into every *future* session.

**The gate re-runs at dispatch, not only when the catalogue is built.** Every
call is looked up again and re-checked against `allow` at the moment it runs.
That makes the stored `tools` map and the stored `destructive` flags
non-authoritative: even a model that found some way to edit its own session
document gains nothing by pointing a listed name at an unlisted action.

The consequence worth keeping: a bug in the guest gate can only ever be too
strict, never too permissive about what the operator hid.

## Approval

A permitted action can still be destructive, and that is not a gate this
package can close on its own.

1. The model asks for calls. Non-destructive ones dispatch immediately, but
   their results are held rather than appended, so the turn stays atomic.
2. Destructive ones are not executed. `step` returns
   `status: "awaiting_approval"` with each pending call's `call_id`, resolved
   ref, and **full arguments**.
3. You decide, and re-invoke: `step` with `approve: ["c3f1a8..."]`. Anything
   not named is denied — an empty or missing list denies everything, which is
   the safe direction to fail.
4. Approved calls execute. Every call in the turn gets exactly one
   `role: "tool"` turn appended, then `pending` is cleared.

**Approve by call, not by tool.** The `call_id` hashes the tool name *and* its
arguments, so an approval resolves to exactly what you were shown. Approving a
bare tool name would let a later iteration reuse it for a different `path` —
the same delete, a different document.

**Approvals never persist.** `pending` is cleared at the end of every
iteration, so a stale `call_id` is a no-op rather than a standing grant.

Destructive-ness is resolved host-side, the same union solx-mcp uses: config
rules ∪ the row's `solx:destructive` capability ∪ every Command and Webhook
row, unconditionally. The lookup **fails closed** — an action that cannot be
read back is treated as destructive rather than waved through.

Built-in tools never need approval. Each one is confined by construction:
`sys__memory_save` writes only under the session's own scope with a name this
package generates, `sys__context_read` opens only the frozen index, and
`sys__tool_search` searches only the frozen allowlist.

## Schemas

Param-type schemas are normalized on the way out: `{"type":["string","null"]}`
becomes `{"type":"string"}`, with absence from `required` carrying optionality.
Small local models handle union types poorly — they emit the string `"null"`,
or omit the field and then apologise. `parameters` is always an object schema,
even for a type with no properties, because Ollama requires it.

## Errors and failure handling

A dispatch that fails — bad params, a missing action, a gate rejection — goes
back to the model as a `role: "tool"` turn carrying the error. That is how the
agent self-corrects, and a conductor that aborts on first failure is useless.
It is capped: three consecutive iterations where *every* call failed ends the
session as `blocked`.

`status` is one of `running`, `awaiting_approval`, `final`, `blocked`,
`exhausted`, or `cancelled`.

## Timeouts

Nested actions keep their own budgets, so they stack. `conductor-step` is set
to 2400s because it must exceed `ollama-chat`'s 1860s outer budget plus tool
dispatch time. A step that suspends for approval returns immediately — your
thinking time is never inside the budget.

## Tests

```bash
npm test        # or: node --test tests/conductor.test.mjs
```

`src/names.js` is staged next to the entry module and imported as
`./names.js` — solx-quickjs preserves what you list in `source_artifact_names`
and componentize-qjs resolves imports from that root with a real node resolver,
so splitting the guest across files needs nothing but the extra `save file`
line in `install.solx`.

`tests/harness.mjs` stands up a fake solx host: an in-memory action registry, an
in-memory document store, and a scripted model. The guest reaches everything
through one import — `exec(ref, jsonParams)` — which is what makes this cheap.
The loader rewrites that import and appends an export list, so the tests reach
the internals without the source carrying a test-only export.

The honest limit: the fake mirrors solx-core's behaviour as it was read out of
the source (`typeRef` required on create, search hits carrying no contents,
camelCase query keys, hidden actions reported as not-found). If solx-core
changes one of those, these tests keep passing and the package breaks. They
check this package's logic, not its integration.

## What is not done

- **Never run against a live model.** The loop, the gate and the approval
  handshake are covered by tests, but no end-to-end run has happened. Expect
  to find real bugs in the first session.
- **Approval cannot tell a human from a script.** `step` returns pending calls
  to whoever called it, which may be a `.solx` script that auto-approves. The
  conductor cannot distinguish them. Binding approval to a provable caller
  identity is a solx-core question, not one this package can answer.
- **Sequential tool dispatch only.** Ollama can emit several calls per turn;
  they run in order. Simpler, and all the ordering guarantees are free, but it
  is a real cost on slow tools.
- **Memories are never forgotten.** Nothing ages them out, dedupes them, or
  reconciles two that contradict each other. A long-lived scope will drift, and
  pruning it is a manual `solx delete document` for now.
- **Session documents are never cleaned up.** They accumulate under
  `/conductor/sessions`. `uninstall.solx` does not remove them, deliberately —
  a transcript is a record, and so is a memory.

## Layout

```
src/solx-conductor.js    the whole component: gate, catalogue, documents, loop
src/names.js             generated word lists for session names
tests/harness.mjs        fake solx host + the loader that rewrites the imports
tests/conductor.test.mjs 44 tests over the gate, the loop, and the documents
install.solx             stage JS -> compile via solx-quickjs -> register rows
uninstall.solx           remove the rows, types, and artifacts (not documents)
verify.solx              get each action, to confirm an install landed
```
