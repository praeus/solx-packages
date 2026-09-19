# solx-xprompt

ExecPrompt (XPrompt) — a chat-style widget that turns a natural-language
instruction into a `multi-inquire` turn, using the existing solx packages
underneath.

The widget is intentionally small. The hard part of "natural-language →
exec" already exists in `solx-inquiry`'s `multi-inquire` action: one LLM
call decides whether an instruction can be answered directly or needs up
to three inquiries fanned out, then does whichever it decided (see
solx-inquiry's README and `intent.rs`). xprompt is the UI in front of
it — a Composer, a transcript, a "Run" button next to every action hit a
turn surfaces, and a Console tab showing that turn's own phases as they
happen.

## Status

**Scaffold + auto-run.** The wiring is in place and the widget mounts
cleanly in solx-web; a turn runs end to end against `multi-inquire`,
tracked and cancellable, under a named, persisted session — so
multi-inquire's own cross-turn history (`session::history_block`) sees
real prior turns, not an empty one every time. The widget now also runs
`multi-inquire`'s returned *plans* unattended (hits stay one-at-a-time
and hand-parameterised), with destructive actions gated on a per-session
allowlist, and a multi-turn `next_prompt` loop driven by a configurable
cap that carries each turn's findings into the next. What's not yet built —
streaming the answer token-by-token, a real params form on the "Run"
button, a `memory_path` so `result.memories[]` isn't always empty —
is UI affordances, not architecture.

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
multi-inquire hands back, typed as solx-inquiry's own
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

Every prompt is one call to `multi-inquire`. There is no chat-vs-research
heuristic in this package — that decision is `multi-inquire`'s own intent
phase (one schema-constrained LLM call, given real context: skills,
memories, session history), made once, with far more to go on than a
keyword match ever could. `src/dispatch.ts` is pure plumbing around that
one call:

```
   user prompt ─► multi-inquire(instruction, model, session)
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
`client.invocations.stop`, and what lets this widget tail that turn's
`[multi_inquire:...]` console lines (recall, intent, each inquiry's
terms/hits/start/done) as they happen — rendered as raw lines on the
Console tab and as the progress strip everywhere else (see "Progress").
solx-inquiry's README has the full tag vocabulary.

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
the `session_document` multi-inquire returned is saved back to
`/xprompt/sessions/<name>` — that's what gives the *next* turn's intent
phase real history to read, rather than always looking like a first
message. The same name doubles as the merged-console `logId`, so the
call log lands at `/xprompt/call-logs/<name>` — the same root name as
the session document it belongs to, without the two mechanisms knowing
anything about each other. See "Session picking" below for what this
does and doesn't cover yet.

## Why a bad turn cannot blank the widget

`TurnBlock` reads `result.intent.mode`, `result.hits` and
`result.responses[i].citations` without guards — and a React render that
throws unmounts the whole tree. Because the transcript is persisted to
localStorage, one malformed turn used to blank the widget and *stay*
blank across reloads, with no "Clear" button left to press.

Two layers now:

- **`src/result.ts` normalises every result before it can reach a
  component**, from all three directions one arrives from: a live turn
  (`dispatch.waitForResult`), the localStorage transcript
  (`loadTranscript`), and a session document replayed by
  `session.loadSessionTranscript`. It repairs rather than rejects — a
  missing `scripts[]` costs the scripts, not the answer — and returns
  `null` only when there is nothing recognisable to render. A missing
  `intent.mode` defaults to `direct`, because `inquire` would promise
  research the turn may never have done, and a missing
  `script.destructive` becomes `[]`, because `runScript` gates on it.
- **`ErrorBoundary` is the backstop**, placed per turn as well as at the
  root. A turn that still cannot be rendered becomes one error card while
  the rest of the thread, the composer and the toolbar keep working.

Relatedly, `waitForResult` branches on the invocation's **status**, not on
whether it reported an `error`. The store writes `error` as `""` when
there was none and the read-back turns `""` into absent, so `cancelled`,
`timeout` and `interrupted` can all finish with no message — and a guard
that only checked `error` let those through and returned `result` (`null`
for a run that never produced one) as though it were an answer.

## Progress

A researched turn can run for minutes — one intent model call, then up to
three inquiries fanned out, each its own model call. The widget's entire
progress vocabulary used to be a `busy` boolean rendering the word
`working...`, which cannot tell thinking from searching from stuck.

There is no progress channel in solx. There is a console, and
`multi-inquire` narrates each milestone to it with a machine-readable
`data.ev` envelope beside the human-readable message (see solx-inquiry's
README, "Progress events"). So the strip above the tabs is a fold over
that feed:

```
console entries ─► parseProgressEvent ─► foldProgress ─► ProgressStrip
                   (src/progress/events.ts)  (reducer.ts)

   thinking  ·  2 inquiries running  ·  #1 done  #2 running  #3 7 hits   ▸
```

Hovering an inquiry chip gives its scope, question, hit count and phase;
the `▸` toggle shows the same thing as rows, for touch and for screen
readers, which a native `title` serves badly. It is `title` rather than a
popover because the widget mounts in a shadow root (no reliable portal
target) and sits above an `overflow-y: auto` thread that would clip an
absolutely positioned one.

Three properties are worth knowing before changing any of it:

- **The tail loop belongs to `XPromptWidget`, not to a panel.** It used to
  live inside `ConsolePanel`'s effect, so it stopped the moment you left
  the Console tab — which made Chat-tab progress impossible. It is gated
  on `busy || tab === "console"` rather than always-on, so an idle widget
  is not holding a long-poll open forever.
- **The fold is over the whole retained buffer, not incremental.** That
  makes the state a pure function of what has been seen, so a reload —
  which re-reads the run from its own `consoleSeqStart` — reconstructs
  exactly what a live session had, with nothing to persist or resume.
- **The feed can lose, duplicate, reorder and truncate events.** Producer
  prints are best-effort and a busy shared console evicts. So the reducer
  creates an inquiry on first sight whatever phase that is, never moves
  one backwards, and only ever grows its expected count — and the parser
  drops what it does not recognise instead of throwing.

`events.ts` also carries a shim that reconstructs what it can from the old
`[multi_inquire:...]` message tags when an entry has no envelope. That is
not optional polish: the `.wasm` package and this widget bundle install
independently, so a widget running ahead of its package is the normal
state, and without it the strip would simply stay empty against one.

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
  main.tsx                      defineReactWidget(...), wrapping the widget in a root
                                ErrorBoundary
  XPromptWidget.tsx             the React component — session + model pickers, tabs,
                                thread, composer
  theme.ts                      design tokens, re-declared inside the shadow root
  refs.ts                       action references
  types.ts                      Turn / InquireHit / MultiInquireResult / OllamaModel /
                                StoredMultiInquireTurn / XPromptSessionDocument
  dispatch.ts                   dispatch, isRunnableActionHit, runScript, the destructive
                                check, and buildFollowUpInstruction
  result.ts                     normalizeResult / normalizeTurn — the one place a result
                                becomes safe to render
  session.ts                    naming (solx-names), listing, saving, deleting, and
                                reading a session back as a transcript
  components/
    Composer.tsx                Enter-to-send textarea, mirror of solx-agent's
    TurnBlock.tsx                one rendered turn; answer turns have a Run column
    ConsolePanel.tsx             the Console tab — renders the merged console
    ErrorBoundary.tsx            per-turn and root render guards
    SettingsPopover.tsx          auto-loop toggle, turn cap, clear approvals
    ProgressStrip.tsx            stage + per-inquiry chips, with hover and a disclosure
  console/                      the merged-console mechanism (now load-bearing, not a
                                copied-in scaffold — dispatch.ts tracks every turn here)
    index.ts                    re-exports
    callLog.ts                  loadCallLog, appendCall, startTrackedCall
    merge.ts                    readMergedConsole — client-side merge by invocation_id
    useConsoleFeed.ts           the one tail loop, owned by XPromptWidget
    refs.ts                     path/type refs for the call-log document
    types.ts                    CallRecord, CallLog, MergedEntry, MergeCursors
  progress/                     live progress, read off the console (see "Progress")
    index.ts                    re-exports
    events.ts                   parseProgressEvent + the legacy tag shim
    reducer.ts                  foldProgress — events to renderable state
    labels.ts                   the exact words and chip classes the strip renders

tests/
  dispatch.test.ts              6 cases — call shape, tracked-call logging, result/error
                                propagation, isRunnableActionHit
  session.test.ts               13 cases — naming, save/load round-trip, listing, and
                                deletion including its ordering contract
  console.test.ts               9 cases — call log, merge filtering, cursors, ordering
  result.test.ts                17 cases — every field a component dereferences survives a
                                missing, wrong-typed or hostile result
  autorun.test.ts               51 cases — captures, destructive gating, runScript's
                                cancellation and error paths, follow-up instructions
  progress.test.ts              41 cases — the event parser, the legacy shim, the fold's
                                loss/duplication/reordering invariants, and the labels
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
`solx-inquiry/multi-inquire` already exposes — a lot — and a few
neighbouring ideas. Listed roughly in the order the user reaches for
them.

### Running a returned plan

**Built.** A returned `scripts[]` plan renders a "Run plan" button next
to its title, driving `dispatch.runScript`, which:

- Substitutes `$name` / `$name.field` capture references between steps
  (script.rs validates they resolve, but does not substitute — the
  widget does).
- Skips any step whose `action_ref` is in the plan's `destructive[]`
  and not yet approved.
- Stops on the first error, and between steps whenever the run has been
  abandoned (Stop, Clear, or a session switch). A step already in
  flight cannot be interrupted; the guarantee is that no further step
  starts.
- Pushes one run-turn per step into the transcript, `ok` / `error` /
  `skipped`, so the result is visible inline.

Only **plans** are executable. Action *hits* are search results — they
matched the query terms, nothing chose them as things to do, and
nothing supplied parameters for them — so each has its own "Run" button
that asks the user for parameters, and carries a `destructive` chip when
it is one (running one by hand is a deliberate act and is not gated the
way an unattended plan is, but nothing previously said that a `command`
action is shell execution). An earlier "Auto-run all" button
executed the whole hit list with `{}`, which is the moral equivalent of
running everything `apropos` printed; it was removed rather than made
safe, because an action whose parameters are all optional can read `{}`
as "apply to everything" and no schema distinguishes that from a
harmless list call.

What counts as destructive is `isDestructiveHit` in `dispatch.ts`,
deliberately the same two checks as solx-inquiry's
`search::is_destructive`: the `solx:destructive` capability tag, or an
**`actionType`** of `command` or `webhook` (shell and outbound HTTP).
Note `actionType`, not `category` — `category` is a descriptive
grouping ("ops", "llm"), and reading it here was a silent hole in which
a Command action without the capability tag classified as safe. Neither
side's check is complete: `solx-config`'s `tool_destructive` list is
invisible from out here, so a `false` means "nothing visible says this
is destructive", not "safe".

Destructive gating defaults to "refuse the whole plan if any step is
destructive and not yet approved". Clicking "Run plan…" (the ellipsis
is the signal that it asks first) surfaces a `window.confirm` that, on
yes, adds those refs to a per-session allowlist persisted under
`solx-xprompt.approved.<session>`. The grant is durable and per
*action*, not per call: an approved ref then runs with whatever
parameters a later plan gives it, until "Clear approvals" in the
Settings popover drops them.

### Act on more of what `multi-inquire` already returns

Of the items originally enumerated, **scripts and destructive gating
are covered above** (see "Running a returned plan"). What is
still left:

- **Memories.** `result.memories[]` are ready-to-save document
  payloads; the widget could surface them as "the model wants to save
  this — accept / decline / edit" cards. Currently the widget never
  passes a `memory_path`, so this is always empty.
- **Citations as chips.** `result.responses[i].citations[]` is the
  list of document paths each response was grounded in. `TurnBlock.tsx`
  currently renders them as one plain "cites: ..." line per response;
  inline chips linking to each document would make the grounded/
  ungrounded distinction easier to scan.

### Multi-turn `next_prompt` loop

**Built.** `multi-inquire` returns an `intent.next_prompt` field — the
model's idea of what to ask next. The Settings popover has a toggle
"Auto-loop follow-ups" (off by default) and a "Turn cap" (1–10, default
3). When on, every `multi_inquire` turn whose result has a non-empty
`next_prompt` automatically issues a follow-up, up to the cap, using the
same session and model. Each shows as a "continue: …" run-turn rather
than a user-turn. The Stop button cancels mid-loop via the same `genRef`
it already uses for the regular Stop path, and the toggle is read at
call time so switching it off stops the very next follow-up.

**The follow-up is not `next_prompt` verbatim.** That field is written
by the *intent* phase — before that turn searched anything; solx-inquiry
calls it speculative and notes that nothing in the pipeline re-invokes
itself with it. Sent bare, a follow-up would be chosen in ignorance of
what the turn that suggested it went on to find, and nothing downstream
repairs that: the next turn's intent call reads history through
`session::history_block`, which extracts only the instruction and
`responses[0].text`, each truncated to 240 characters — scripts, later
responses, notes and errors never reach it.

So `buildFollowUpInstruction` (`dispatch.ts`) sends the suggestion plus
the previous turn's findings and a preamble saying what the suggestion
is and is not:

```
check token expiry

---
The instruction above was suggested by the previous turn's planning phase,
before that turn had searched anything or seen any results. What it actually
found is below. Treat the suggestion as a starting point, not a decision:
narrow it, revise it, or answer directly if it has already been covered.

Previously asked: how does auth work?
- found: Auth uses session tokens. [cited: /notes/auth]
- proposed a plan "rotate tokens" (3 step(s), 1 destructive, not run)
- 1 inquiry failed, so the findings above are partial
```

Entirely caller-side: no `context_documents` (whose per-document cap
would truncate a session document from its *oldest* turn, and which
would duplicate the history block under contradictory framing) and no
handoff document to write and keep in step. When the turn produced
nothing worth carrying, the suggestion is sent unchanged — a preamble
promising results that are not there is worse than none, because the
model tends to satisfy it by inventing them.

The transcript shows the bare suggestion, not the augmented
instruction: the findings are already on screen in the answer turn
above it.

A follow-up is also **not** issued when saving the session document
failed. That document is the only thing carrying this turn into the
next one's history, so a follow-up issued after a failed save would run
as if the turn had never happened.

### Context documents

A natural-language instruction often refers to documents the model
would otherwise have to search for. `multi-inquire`'s `context` field
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
(`capable-tiger-9f2c1a06`), saves the `session_document` multi-inquire
returns after every turn, lists existing sessions from
`/xprompt/sessions` in a picker (`XPromptWidget.tsx`'s "Session" row),
and reconstructs a transcript from a picked session's saved `turns[]`.
"New session" mints a fresh name and an empty transcript without
touching the old one.

**Deleting.** A session owns two documents and a localStorage key:
`/xprompt/sessions/<name>`, `/xprompt/call-logs/<name>`, and
`solx-xprompt.approved.<name>`. `session.deleteSession` removes the
**call log first and the session document last**, which is the contract:
a failure partway then leaves the session still listed and the operation
retryable, where the other order strands a call log whose session has
vanished from the picker with nothing left that could reach it. The call
log is best-effort (a session that never had a turn tracked under it
never had one), the session document is not.

Two affordances, because the picker is a native `<select>` whose value is
always the *current* session — so a button beside it can only ever delete
the one you are on:

- **"Delete…"** in the Session row, for the session in front of you.
  Deleting the current one falls through to `newSession()`, so the widget
  is never left pointing at something that no longer exists.
- **A Sessions list in the Settings popover**, with a `×` per row, for
  cleaning up the others without switching to each one first and loading
  its transcript on the way.

Two things the dialog says out loud, because neither is guessable:

- **Console output is not removed.** Console entries are keyed by
  `action_ref`, so every session's lines share one console with every
  other session and every other caller of `multi-inquire`;
  `/builtin/console/clear` scopes to `before_seq`, not to an invocation.
  They age out on that console's own ring buffer and TTL instead.
- **Nothing upstream gates the delete.** `entity-delete-document` is an
  `internal` action with empty capabilities, so it trips neither half of
  solx-core's destructive test — the widget's own confirm is the only
  gate there is.

`SESSION_LIST_LIMIT` is 200, raised from 50 once deleting existed: a
session past the cap is invisible to *every* affordance the widget has,
so the limit bounds not just what is listed but what can ever be cleaned
up. `listSessions` returns `total` alongside, and the popover says
"showing N of M" rather than implying the list is complete.

What's still missing:

- **Hits and hindsight aren't part of session history.**
  `StoredMultiInquireTurn` mirrors exactly what solx-inquiry's
  `session::build_document` writes — no `hits[]`, no per-turn `model`,
  no timestamp (solx-inquiry frames history as "orientation, not
  evidence" and stores accordingly). A turn reloaded from a past session
  shows its text and direct/researched mode but not its suggested
  actions, cited documents, or when it happened.
- **A `memory_path`.** Passing one turns memories on for the current
  session; without it, `result.memories[]` is always empty (see "Act on
  more of what multi-inquire already returns" above).
- **A named title, not just the first instruction truncated.**
  `session_document.title` comes straight from solx-inquiry's own
  default (`truncate(first instruction, 80)`); a rename affordance would
  need its own `entity-save-document` call overwriting just `title`.
- A **fork** action — given an existing session name, open a new one
  seeded with the prior session's transcript. Useful when an
  investigation reaches a fork in the road.
- **Bulk or age-based cleanup** — "delete everything not touched in 30
  days". `listSessions` already returns `updatedAt` sorted descending, so
  the data is there; only the affordance is missing.

### Knobs currently surfaced as zero knobs

`multi-inquire` exposes parameters the widget sets to defaults today
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
  detached and is tracked (see "How a turn runs"), its phases drive the
  progress strip live (see "Progress"), and the Console tab shows the
  same feed as raw lines — but the transcript's own answer bubble still
  only appears once the whole turn is terminal. Note that streaming the
  model's *tokens* is a larger job than it looks: `multi_inquire` copies
  each child chat call's console into its own, but `console-copy` stamps
  copied rows with the **source** invocation id, so the client-side
  merge filters them out. Getting tokens here means tracking the child
  invocations too, or changing `copy`.
- **A real params form on "Run".** The widget currently prompts for
  params via `window.prompt(..., "{}")`. multi-inquire returns the
  `paramSchema` on each action hit, so a schema-driven form
  (the same one `ActionRunner`'s Fields tab uses) is straightforward
  to wire in.
- **Running a proposed script.** `result.scripts[]` renders read-only
  today (`TurnBlock.tsx`'s `ScriptRow`) — see "Act on more of what
  multi-inquire already returns" above.
- **Memories and session forking.** See "Session picking" above —
  session naming and persistence are done; `memory_path` and a fork
  action aren't.
