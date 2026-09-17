# solx-xprompt

ExecPrompt (XPrompt) — a chat-style widget that turns a natural-language
instruction into a `multi_inquire` turn, using the existing solx packages
underneath.

The widget is intentionally small. The hard part of "natural-language →
exec" already exists in `solx-inquiry`'s `multi_inquire` action: one LLM
call decides whether an instruction can be answered directly or needs up
to three inquiries fanned out, then does whichever it decided (see
solx-inquiry's README and `intent.rs`). xprompt is the UI in front of
it — a Composer, a transcript, a "Run" button next to every action hit a
turn surfaces, and a Console tab showing that turn's own phases as they
happen.

## Status

**Scaffold.** The wiring is in place and the widget mounts cleanly in
solx-web; a turn runs end to end against `multi_inquire`, tracked and
cancellable, under a named, persisted session — so multi_inquire's own
cross-turn history (`session::history_block`) sees real prior turns, not
an empty one every time. Anything richer — streaming the answer
token-by-token, a real params form on the "Run" button, a `memory_path`
so `result.memories[]` isn't always empty, running a proposed script —
is not yet built. The shape of this package is settled; the missing
pieces are UI affordances, not architecture.

## One action, one document type

Everything in this package lives at `/packages/solx-xprompt/*`. The
document type is here only because a widget's calls are external execs
with no action caller (see `solx-packages/docs/widget-system.md`),
which means the merged-console mechanism can't use the server's
`console-copy` — it tracks its calls in an `XPromptCallLog` document
instead, the same workaround the agent harness documents under
`src/console/`. Session documents (`/xprompt/sessions/<name>`) are the
other kind of document this widget writes, but they're not this
package's own type — `session.ts` saves whatever `session_document`
multi_inquire hands back, typed as solx-inquiry's own
`/packages/solx-inquiry/MultiInquireSession`.

| entity | ref | shape | purpose |
|---|---|---|---|
| action | `/packages/solx-xprompt/xprompt-widget` | `script` (returns `WidgetDescriptor`) | the widget solx-web mounts |
| type | `/packages/solx-xprompt/XPromptCallLog` | `{calls: CallRecord[]}` | durable index of one session's tracked calls, at `/xprompt/call-logs/<name>` |
| *(reused)* | `/packages/solx-inquiry/MultiInquireSession` | `{turns: [...], turnCount, lastInstruction}` | a session's saved turns, at `/xprompt/sessions/<name>` — solx-inquiry's type, not this package's |

There is no `save file widgets/solx-xprompt.js` step in `install.solx`'s
*output* — the bundle is uploaded by `install-package` from the local
`dist/solx-xprompt.js`, so the build has to run first. See "Install"
below.

## How a turn runs

Every prompt is one call to `multi_inquire`. There is no chat-vs-research
heuristic in this package — that decision is `multi_inquire`'s own intent
phase (one schema-constrained LLM call, given real context: skills,
memories, session history), made once, with far more to go on than a
keyword match ever could. `src/dispatch.ts` is pure plumbing around that
one call:

```
   user prompt ─► multi_inquire(instruction, model, session)
                     │
                     ├─ intent.mode == "direct"  → responses[] (no search)
                     └─ intent.mode == "inquire" → up to 3 inquiries fan out,
                                                    then responses[] + hits[]
                                                    + scripts[] + notes[]
```

`dispatch()` starts the call **detached** via `client.invocations.start`
(through this widget's own `startTrackedCall`, see `src/console/`) rather
than a blocking `host.call`. That's what gives a turn an `invocation_id`
the Composer's Stop button can cancel for real via
`client.invocations.stop`, and what lets the Console tab tail that turn's
`[multi_inquire:...]` console lines (recall, intent, each inquiry's terms/
hits/result) as they happen — see solx-inquiry's README for the full tag
vocabulary.

The action hits a turn returns carry `path`, `name`, `details.category`,
and `details.capabilities`; the widget renders each as a row with a "Run"
button that asks the user for a params JSON blob (the call site is
`components/TurnBlock.tsx:ActionHitRow`). Doc hits are listed under
"Cited documents" without a Run button. `scripts[]` — ordered,
already-validated action plans — render read-only for now; nothing in
this widget executes one yet (see the roadmap).

Every turn runs under a named session (`src/session.ts`): a solx-names
name (e.g. `capable-tiger-9f2c1a06`), generated once per new session and
kept in localStorage so a reload stays on it. After each successful turn
the `session_document` multi_inquire returned is saved back to
`/xprompt/sessions/<name>` — that's what gives the *next* turn's intent
phase real history to read, rather than always looking like a first
message. The same name doubles as the merged-console `logId`, so the
call log lands at `/xprompt/call-logs/<name>` — the same root name as
the session document it belongs to, without the two mechanisms knowing
anything about each other. See "Session picking" below for what this
does and doesn't cover yet.

## Install

```sh
npm install                # or `bun install`
npm run build              # tsc --noEmit && vite build → dist/solx-xprompt.js
npm run typecheck          # optional; the build typechecks too
npm run test               # vitest — 20 cases across dispatch + console + session
solx install-package ./solx-xprompt
```

`solx-inquiry` (which depends on `solx-ollama` — or another chat action
— itself) and `solx-names` must also be installed for turns to work. If
either is missing, the widget still mounts but a turn (or starting a new
session) will surface that absence as an error rather than failing the
whole bundle.

To use it, exec `/packages/solx-xprompt/xprompt-widget` from
solx-web's action runner and the bundle mounts in the Widget tab.

## Source layout

```
src/
  main.ts                       defineReactWidget("solx-xprompt-widget", XPromptWidget)
  XPromptWidget.tsx             the React component — session + model pickers, tabs,
                                thread, composer
  theme.ts                      design tokens, re-declared inside the shadow root
  refs.ts                       action references
  types.ts                      Turn / InquireHit / MultiInquireResult / OllamaModel /
                                StoredMultiInquireTurn / XPromptSessionDocument
  dispatch.ts                   dispatch, isRunnableActionHit
  session.ts                    naming (solx-names), listing, saving, and reading a
                                session back as a transcript
  components/
    Composer.tsx                Enter-to-send textarea, mirror of solx-agent's
    TurnBlock.tsx                one rendered turn; answer turns have a Run column
    ConsolePanel.tsx             the Console tab — tails the session's merged console
  console/                      the merged-console mechanism (now load-bearing, not a
                                copied-in scaffold — dispatch.ts tracks every turn here)
    index.ts                    re-exports
    callLog.ts                  loadCallLog, appendCall, startTrackedCall
    merge.ts                    readMergedConsole — client-side merge by invocation_id
    refs.ts                     path/type refs for the call-log document
    types.ts                    CallRecord, CallLog, MergedEntry, MergeCursors

tests/
  dispatch.test.ts              6 cases — call shape, tracked-call logging, result/error
                                propagation, isRunnableActionHit
  session.test.ts               7 cases — naming, save/load round-trip, listing
  console.test.ts               7 cases from the scaffold (untouched)
  fakeClient.ts                 WidgetClient-shaped fake, including a `respondTo` hook
                                dispatch.test.ts uses to script invocation outcomes,
                                and entity-list-documents / random-name support for
                                session.test.ts

dist/
  solx-xprompt.js               the build artifact install.solx uploads; ~386 KB
                                (React + ReactDOM bundled, single self-contained ESM,
                                CSS injected at runtime — see solx-widgets's
                                widgetViteConfig for the constraints)
```

## Conventions this package follows

These are the solx-widgets contract and the existing
solx-agent/solx-widgets conventions this package leans on. Worth
calling out so a future change doesn't accidentally violate one:

- **`process.env.NODE_ENV = "production"`** is set in the vite config
  (inherited from `solx-widgets`'s `createWidgetConfig`). The bundle
  imports React/ReactDOM rather than treating them as externals, so the
  build replaces the runtime `process.env.NODE_ENV` checks at bundle
  time. Without it, the widget throws `process is not defined` at mount
  time, not at build time.
- **Shadow root, not iframe.** The widget runs in the same JS realm as
  its host. `theme.ts` redeclares the design tokens; the host page's
  CSS does not cross the shadow boundary.
- **Scoped client, not raw server access.** `useSolxWidgetClient()` →
  `hostFromClient(client)` → `host.call(ref, params)` / `host.try(ref, params)`.
  `compact()` drops `null`/`undefined` params before the optional-field
  schema check rejects them.
- **No new backend.** Every action this widget calls is an ordinary
  solx action; nothing requires server-side changes. The
  `XPromptCallLog` document type is the only thing this package adds to
  the schema, and it's optional — nothing reads it except this widget.
- **Tests don't need a browser.** `dispatch.test.ts` exercises the call
  shape and result/error propagation against a `WidgetClient`-shaped
  stub; `console.test.ts` is jsdom-light (uses the `fakeClient.ts`
  fixture, not jsdom itself). A rendered-React test would be heavier;
  deferred until there's a feature that needs it.

## Features on the roadmap

The scaffold above is the floor. The ceiling is the surface area
`solx-inquiry/multi_inquire` already exposes — a lot — and a few
neighbouring ideas. Listed roughly in the order the user reaches for
them.

### Auto-run non-destructive actions

When a turn's action inquiries return action hits, the widget currently
renders a "Run" button next to each. The plan is a small "auto-run up to
N non-destructive steps" toggle:

- Read every action hit's `capabilities` from `hit.details.capabilities`
  (the field that already carries `solx:destructive`). Anything carrying
  `solx:destructive`, plus anything of `actionType: command` or
  `webhook`, is skipped — same rule `multi_inquire` itself uses to
  decide what to flag in its own `destructive[]` array.
- Run the rest in order, surfacing each result as a turn the way
  `onRun` already does for a single Run click.
- Stop on the first error and let the user take it from there.

The `N` knob is small but matters: an over-eager auto-run is a way
to discover what `solx:destructive` *doesn't* cover. The widget
should default to `N=1` and surface each next step before running it.

### Act on more of what `multi_inquire` already returns

Every turn already is a `multi_inquire` call (see "How a turn runs"
above) — what's not built yet is acting on everything it hands back:

- **Run a whole plan, not just one action.** `result.scripts[]` is an
  ordered list of `{action_ref, params, capture?}` calls the model
  produced, validated against the action catalogue, with
  `$name`/`$name.field` capture-reference substitution. Today
  `TurnBlock.tsx`'s `ScriptRow` renders a script's title and steps
  read-only; a "Run plan" button that executes each step in order
  (substituting captures itself — nothing in solx-inquiry does that for
  you) is the natural next step.
- **Respect `destructive[]`.** Each script already carries its own
  `destructive[]` — the widget should refuse to auto-run any plan
  where it's non-empty and instead show an explicit "Run anyway"
  affordance (see "Auto-run non-destructive actions" above for the
  same rule applied to bare action hits).
- **Memories.** `result.memories[]` are ready-to-save document
  payloads; the widget could surface them as "the model wants to save
  this — accept / decline / edit" cards. Currently the widget never
  passes a `memory_path`, so this is always empty.
- **Citations as chips.** `result.responses[i].citations[]` is the
  list of document paths each response was grounded in. `TurnBlock.tsx`
  currently renders them as one plain "cites: ..." line per response;
  inline chips linking to each document would make the grounded/
  ungrounded distinction easier to scan.

### Context documents

A natural-language instruction often refers to documents the model
would otherwise have to search for. `multi_inquire`'s `context` field
takes up to ten `/path/name` references and reads each with one
`entity-get-document` call up front, then rides them along into every
prompt of the run — the intent call and every inquiry, regardless of
kind.

The widget's version of this is a "Pin documents" affordance: drag a
document onto the composer, or pick one from a `/pinned` list, and the
next research turn (and every subsequent one in the same session)
includes it as context. The list itself is just one document under
`/xprompt/pinned` — no schema change required, a `string[]` of
`/path/name` refs is enough.

### Session picking

Done: `src/session.ts` names a session with solx-names
(`capable-tiger-9f2c1a06`), saves the `session_document` multi_inquire
returns after every turn, lists existing sessions from
`/xprompt/sessions` in a picker (`XPromptWidget.tsx`'s "Session" row),
and reconstructs a transcript from a picked session's saved `turns[]`.
"New session" mints a fresh name and an empty transcript without
touching the old one. What's still missing:

- **Hits and hindsight aren't part of session history.**
  `StoredMultiInquireTurn` mirrors exactly what solx-inquiry's
  `session::build_document` writes — no `hits[]`, no per-turn `model`,
  no timestamp (solx-inquiry frames history as "orientation, not
  evidence" and stores accordingly). A turn reloaded from a past session
  shows its text and direct/researched mode but not its suggested
  actions, cited documents, or when it happened.
- **A `memory_path`.** Passing one turns memories on for the current
  session; without it, `result.memories[]` is always empty (see "Act on
  more of what multi_inquire already returns" above).
- **A named title, not just the first instruction truncated.**
  `session_document.title` comes straight from solx-inquiry's own
  default (`truncate(first instruction, 80)`); a rename affordance would
  need its own `entity-save-document` call overwriting just `title`.
- A **fork** action — given an existing session name, open a new one
  seeded with the prior session's transcript. Useful when an
  investigation reaches a fork in the road.

### Knobs currently surfaced as zero knobs

`multi_inquire` exposes parameters the widget sets to defaults today
but that a user with a real workload will want to adjust. Worth a small
"Settings" popover:

- **`max_terms`** (default 5, clamped 10) — how many search terms
  the model may produce per inquiry. Lower means a faster, narrower
  search; higher means more recall at the cost of more model output.
- **`max_results`** (default 10, clamped 50) — cap on hits per
  inquiry. Higher is rarely useful; the model's context window is
  the real ceiling.
- **`path_prefix` / `document_path_prefix` / `action_path_prefix`** —
  restrict searches to a prefix. The default ("search everywhere")
  is right for most turns; a "scope to `/research/...`" mode is the
  obvious second mode.
- **`type_ref`** — restrict document searches to one document type,
  for the "show me only InquirySkills" or "show me only sessions"
  turn.
- **`llm_options.num_ctx`** — raise the model context window when
  the recall + history + results would otherwise overflow the model
  default. This is the only knob that touches model behaviour
  directly, so it deserves the loudest affordance.

## What's not built yet

Items the scaffold deliberately leaves out, ordered by how useful they
would be next:

- **Streaming the answer into the transcript itself.** A turn now runs
  detached and is tracked (see "How a turn runs"), and the Console tab
  already tails its `[multi_inquire:...]` phases live — but the
  transcript's own answer bubble still only appears once the whole turn
  is terminal. Reusing the Console tab's tailing loop to update the
  in-progress answer turn directly (rather than requiring a tab switch
  to watch progress) is the obvious next step.
- **A real params form on "Run".** The widget currently prompts for
  params via `window.prompt(..., "{}")`. multi_inquire returns the
  `paramSchema` on each action hit, so a schema-driven form
  (the same one `ActionRunner`'s Fields tab uses) is straightforward
  to wire in.
- **Running a proposed script.** `result.scripts[]` renders read-only
  today (`TurnBlock.tsx`'s `ScriptRow`) — see "Act on more of what
  multi_inquire already returns" above.
- **Memories and session forking.** See "Session picking" above —
  session naming and persistence are done; `memory_path` and a fork
  action aren't.
