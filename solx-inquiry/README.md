# solx-inquiry

Ask a question in plain language and get an answer grounded in solx
documents.

This is the companion to [solx-agent](../solx-agent). Where the agent hands a
model the whole action registry as a tool catalogue and lets it drive, inquiry
hands a model **five fixed read-only verbs** and drives the rest itself. That
one decision is the whole package: the model never has to discover a tool,
never constructs a wire name, never gets a call refused, and never needs a
second iteration to recover from a wrong guess.

```
  "Summarize the articles from ada"
        │
   1. PLAN     one model call, a JSON schema, no tools
        │      → [{verb: "search", q: "article ada", path_prefix: "/blog"}]
   2. GATHER   no model call; five verbs → real builtins
        │      → [the documents, whole, with their contents]
   3. ANSWER   one model call, free text, grounded in those documents
        ▼
  an answer that cites /blog/rust-1, or an honest "I could not find that"
```

Chat-shaped, but not a chat: the three phases are shown, not hidden. It is
also not an agent -- there is no open-ended loop, no approval, and no tool
choice to get wrong.

## Why not just use the agent?

The agent can answer this kind of question. The difference is what it costs
and what it risks:

| | solx-agent | solx-inquiry |
|---|---|---|
| Model sees | a catalogue of ~32 tool definitions | five verbs, described in prose |
| Model must know | `act__builtin__document__search-documents` | `search` |
| Wrong guess | a refused call, then a retry | a dropped step, noted |
| Needs tool-calling | yes | **no** -- the plan call uses a JSON schema |
| Destructive calls | suspended for approval | impossible; every verb is a read |
| Typical model calls | one per iteration, until done | exactly two |

The trade is reach for reliability. Inquiry cannot author an action or send an
email; it can answer a question about what is in the store, which is what it
is for.

## Install

```sh
npm install && npm run build && npm run build:headless
solx install-package ./solx-inquiry
```

`solx-ollama` must also be installed, with a model that can chat. The planner
phase uses `format` (a JSON Schema), which any current Ollama model supports;
the answer phase is an ordinary completion.

Then exec the widget action from solx-web's action runner and it mounts.
`solx exec /packages/solx-inquiry/inquiry-widget` on its own just returns the
descriptor.

## Headless: inquiry

`/packages/solx-inquiry/inquiry` runs one question through the same pipeline
with no UI -- built separately from the widget (`npm run build:headless`, a
plain esbuild bundle, no React) and registered as a `Command` action. It talks
to `solx-server` over HTTP exactly like `tests/live.test.ts` does, using the
same inquiry documents under `/inquiry/sessions` the widget reads and writes.

```sh
solx exec /packages/solx-inquiry/inquiry --json '{
  "question": "Summarize the articles from ada",
  "model": "qwen3:4b",
  "path_prefix": "/blog"
}'
```

The result carries `session_id`. Pass it back to continue the conversation:

- `{"session_id": "...", "question": "..."}` -- the next question.
- `{"session_id": "..."}` alone -- report the inquiry's current state.

There is no `approve` parameter, because nothing ever suspends: every verb is
a read. `model` is required on every call.

**Tuning from the CLI.** `plan_only: true` runs the plan and the lookups but
skips the answer call, returning the plan, the steps and the document refs.
That is the fastest way to see how the model reads a question without waiting
for a written answer:

```sh
solx exec /packages/solx-inquiry/inquiry --json '{
  "question": "...", "model": "qwen3:4b", "plan_only": true
}'
```

**Budget overrides** on any call: `max_docs`, `max_plan_steps`, `doc_budget`,
`include_actions`, `replan`, `chat_timeout_secs`. `path_prefix` and `type_ref`
are hints folded into the question, not filters -- the planner still decides
where to look.

Connecting to `solx-server` needs `SOLX_SERVER_URL` (set on the action's
`actionConfig.env` by `install.solx`) and a bearer token, resolved as
`SOLX_SERVER_TOKEN` (or `SOLX_TOKEN`) from the environment, falling back to
`server_token` in `solx-config.json`.

## The five verbs

The model is given exactly these, and no others:

| verb | builtin | use |
|---|---|---|
| `search` | `search-documents` | full-text over documents. Two or three words. |
| `list` | `entity-list-documents` | documents under a path or of a type, no terms |
| `get` | `entity-get-document` | one document by exact path and name |
| `types` | `entity-list-types` | find a document type by name |
| `actions` | `search-actions` | find what the system can do |

`search` and `list` are the same builtin -- a no-`q` search *is* a list -- and
are separate verbs only so the planner can say what it means and the
transcript can read clearly.

## The corpus index

Before planning, the harness issues two capped list calls and renders a short
**index of what exists** -- document paths with counts, and type names. The
planner is told to copy a path from that list rather than invent one.

This is the concrete mechanism behind the "optimize for solx entities" goal.
The agent's catalogue is a *dispatch table* (names the model may call); this
is *vocabulary* (words the model may reuse). Nothing in it is callable, which
is why inquiry needs no gate: the worst a model can do with a path it read
here is search under it.

Turn it up with `include_actions` (in Options, or as a param) to also list
action paths, for "what can this instance do?" questions.

## Document model

Everything lives under `/inquiry`, one type, one document per conversation:

| kind | path | type | written by |
|---|---|---|---|
| Inquiry | `/inquiry/sessions` | `InquirySession` | the pipeline, once per phase |

```sh
solx list doc --path /inquiry/sessions
solx get doc /inquiry/sessions/wandering-heron
```

One inquiry is one conversation is one document. The widget holds no
transcript of its own, so a reload -- or another client, or the CLI -- sees
the same thread. Ids are an adjective and a noun, checked before they are
taken (the check fails closed).

`turns`, `plan` and `documents` are deliberately *undeclared* in the type. The
full-text index walks only a type's declared fields, so leaving them out keeps
the whole transcript -- and every gathered document body -- out of FTS while
still storing it. `title` and `answer` **are** indexed, so `solx search` can
find an inquiry by what it asked or what it concluded.

## The widget

Same shell as the agent's, minus what inquiry does not have:

- **Header** -- model pick, inquiry history, status. No iteration counter.
- **Thread** -- a question, the plan, the documents, the answer.
- **Plan card** -- the model's *intent* (`search q="rust" path=/blog`) beside
  the action it resolved to, with each step's outcome and document count.
- **Documents pane** -- the evidence, collapsed; refs always visible.
- **Options** -- the three budgets and two flags. Live: they apply from the
  next question, because they are read at the top of every turn.
- **Preview last plan** -- re-run the lookups for the last question and show
  the plan without writing an answer. The widget's one feature the CLI does
  not have in the same form.

There is no approval card, because nothing a verb can do is destructive, and
no grant panel, because there is nothing to fence.

## Tests

```sh
npm test          # 58 tests against a fake host, plus the built bundle
npm run typecheck
```

`tests/fakeHost.ts` stands up an in-memory document store, type registry and
action catalogue behind the same one-method seam a real host uses. Its honest
limit is the same as the agent's fake: it mirrors solx-core's behaviour *as
read out of the source*.

`tests/live.test.ts` is what covers that gap, and is skipped unless
`SOLX_TOKEN` is set:

```sh
solx-server --port 8791 &
SOLX_TOKEN=<token> npx vitest run tests/live.test.ts
```

Run it against a **current** `solx-server`. The specific bug it is built to
catch is the `q: null` family: `list` and `search` are the same builtin, and
`list` must send *no* `q` key at all, because an explicit null fails schema
validation and the whole search errors.

## What is not done

- **No embeddings.** Retrieval is FTS5 term-matching plus the corpus index.
  A vector search would be a `search` verb implementation detail, not a
  redesign, but it is not here.
- **One retry, not a loop.** A plan that finds nothing gets exactly one second
  attempt. A question needing three rounds of refinement is out of scope; that
  is what the agent is for.
- **No follow-up rewriting.** A pronoun in a follow-up question ("what about
  *those*?") is not resolved against the previous turn before planning. History
  is supplied to the planner, and a capable model handles it, but nothing
  rewrites the question.
- **The writer is not streamed.** The answer arrives whole. Streaming is
  possible -- `solx-ollama` streams into its console -- but the widget renders
  from the document, not the console, so it is not wired yet.

See [DESIGN.md](DESIGN.md) for the reasoning behind all of the above.
