# Agent harness consolidation

**Status: complete.** Improvements #1 (detached chat), #2 (grant removal) and
#3 (human-readable tool names + recap rendering) are implemented and passing
tests. The planner lineage (`solx-inquiry`, `solx-xprompt`, `solx-prompt`) has
been uninstalled from the database and moved to `D:\Projects\solx-retired-packages\`.

The work that followed this change-set left a backlog of its own — see
[Review findings still open](#review-findings-still-open), which is the live
list, not part of the plan recorded below.

## Why this document exists

Four attempts at an agent harness now exist: `solx-agent`, `solx-inquiry`,
`solx-xprompt`, and `solx-prompt`. Only `solx-agent` works reliably. The other
three share a design that keeps failing — a model writes a JSON plan the
harness then parses, with tolerance code (e.g. `solx-prompt/crate/src/parse.rs`,
~380 lines across three tiers: strict JSON → fenced block → hand-rolled XML)
growing to cover every way a model deviates from the schema.

`solx-agent` works because it uses Ollama's native tool-calling: the model is
handed `tools` and returns a structured `tool_calls` array, so there is nothing
to parse. A tool name is resolved through the session's own dispatch map, never
parsed back — the model cannot synthesize a name for an action that was never
listed.

The decision this document records: **build on solx-agent, retire the other
three.** The work is three improvements to solx-agent (below), plus the
retirement of the planner lineage.

## The three improvements

The conversation converged on three. They are listed as discussed, with the
one correction that matters for implementation noted inline.

### 1. Detached chat via start/poll/cancelled (enables headless + cancellation)

Today the loop's model call is a blocking `host.call(CHAT, …)`. Consequence:
Stop does not interrupt a generation in flight (it only lands between
iterations), and there is no streaming — the UI shows "working…" then a burst
of tool-call cards, which is a large part of the "cluttered" complaint.

The fix is the detached invocation loop already written in
`solx-prompt/crate/src/llm.rs` (and used by `solx-ollama`): `action-start` to
detach the chat call, then a loop of `action-cancelled` check →
`action-poll` (long-poll, `wait_secs`) → `console/tail`/`console/copy` to drain
progress. Benefits:

- real mid-call cancellation (bounded by the poll slice, not the model),
- streaming progress via console drain,
- a headless action later becomes the *same* loop with a different entry point
  (started from a page vs. started as an action), so no second implementation.

This is wanted for the UI mode too — see the decision below. Scope for this
change: port `llm.rs`'s `call`/`poll_to_completion`/`drain_console`/`finish`
into the harness, replace the blocking `chat()` in `turn.ts`, keep the
`LONG_LIVED_HOST_MARKER` fallback for bare-CLI use (where cancellation is
unavailable anyway).

**Known constraints (from `llm.rs` and prior work):**

- `action-start` requires a long-lived host (`solx-server`/`solx-mcp`). Under
  bare `solx exec` it refuses; fall back to blocking. The UI runs against
  `solx-server`, so it qualifies.
- Tab-close mid-poll leaves the child `ollama-chat` running until it completes
  (bounded — no orphaned *process*, unlike the stderr-pipe incident recorded
  elsewhere). The cancelled-check must `action-stop` the child, and the
  parent-stop path must propagate to the child. Add a test: close the tab
  mid-generation, confirm the invocation reaches a terminal state.
- Cancellation latency is bounded (~one `wait_secs`), not instant. This is
  cooperative cancellation, not a WIT-level async boundary — `custom-action.wit`
  is fully synchronous for both Rust and quickjs guests.

### 2. Action-search-driven catalogue, drop the restricting UI

**Correction to the framing:** the grant default is *already* `*`
(`DEFAULT_GRANT` in `src/session/store.ts`), and `resolveCatalogue` already
does the search-based tool resolution the planner lineage did as a fixed
phase — but *inside* the loop, re-resolved every turn. So the substance of this
item is **removing the UI that makes reach look restricted**, not changing the
default.

**Decision (confirmed):** drop grant editing entirely. The tool list is
controlled by search alone, and exclusion is handled by the existing
`tool_exclude` config (not an ad-hoc allowlist). The grant becomes a fixed,
unconditional `*`; the gate's *security* boundary is the exclusion policy, not
an operator-curated allowlist.

Specifically:

- Remove the `SetupPanel` grant editor (presets, path picker, `previewTools`/
  `searchActionPaths` wiring, and the `widenGrant`/narrowing surface) from the
  UI. The grant is no longer editable — it is pinned to `*`.
- Reach is limited solely by exclusion, which already works host-side:
  `resolveCatalogue`/`entity-get-action` are called with `excludeHidden: true`,
  and hidden-ness resolves in `solx-config` as `tool_exclude` config rules
  (∪ the legacy `mcp_exclude` key) ∪ the row's `solx:hidden` capability, via
  `ToolPolicy` — the same `ToolPolicy` solx-mcp uses. Nothing here needs to
  know the rules.
- Keep the caller-specific `HARD_DENY`/`DOC_WRITERS` in `refs.ts` (secrets,
  detached spawn, self-modification, `/packages/solx-agent/*`): those genuinely
  cannot be config rules — they are dangerous *because this is the caller*, and
  a config-level `tool_exclude` would not cover them. This is not "ad hoc"; it
  is the residual caller layer.
- `widenGrant`/`addTurn`'s grant plumbing becomes vestigial: it can stay in the
  harness code (harmless — the grant is always `*`), but its UI surface is
  deleted. Decide at implementation time whether to also simplify the harness
  by removing the now-constant grant parameter, or leave it in place to avoid
  churn in `turn.ts`/`gate.ts`.

Note: dropping grant editing removes the "operator is the trust root" affordance
that `widenGrant` existed for. That is acceptable here because the trust
boundary moves to `tool_exclude` (operator-edited config, enforced in Rust) —
which is *stronger* than an in-UI allowlist, not weaker.

### 3. Clean tool UI with human-readable names

The transcript renders the internal encoded name (`act__builtin__document__entity-save-document`)
as the primary label, with the human `ref` only as a hover title. The action
row already carries `caption`/`description` (`catalogue.ts`), unused by the
transcript.

- Thread a `toolName → caption/description` map out of `resolveCatalogue`
  (alongside the existing `toolName → ref` map) so `transcript.ts`/`ToolCallCard.tsx`
  can render the caption as the label and the `ref` as the secondary mono detail.
- Do **not** change the wire tool name: `encodeToolName` exists to avoid
  cross-package name collisions. Label ≠ dispatch name.
- Collapse verbose cards: default non-live turns to the `summariseTurn` recap
  (already computed); show only the current iteration expanded on a live turn;
  keep full `arguments`/`result` JSON one click deep (behind the existing expand).
- Replace the raw `sessionId` chip in `Header.tsx` with a friendly label
  (the session title already computed by `titleFrom`/`sessionTitle`).

## What we are deliberately NOT doing (yet)

- **Not building the headless action.** This change-set only does the parts the
  UI benefits from immediately and that make a later headless port a thin
  wrapper: detached chat + a clean harness/UI boundary. The headless action
  itself is a follow-up.
- **Not de-async-ing the harness for quickjs.** Out of scope here; recorded
  only so the headless follow-up has the right mental model (see below).
- **Not wiring streaming.** Improvement #1 detached the chat call, which is
  also what makes streaming *possible* — but nothing drains the child's console
  yet. See the streaming follow-up section below.

## Notes for the future headless follow-up (not in this change-set)

- The harness is already React-free (`loop.ts` deliberately so); React lives
  only in `AgentWidget.tsx` and `src/components/`. The seam is `Host`
  (`solx-widgets/src/wrap/host.ts`): `call`/`try` over `actions.exec`.
- A headless action is a new `Host` whose `exec` is the guest's
  `action-exec.exec` import (`custom-action.wit`), not `client.actions.exec`.
- The `custom-action` WIT world is fully synchronous; "async" in the Rust action
  is the detached start/poll loop, not `await`. A quickjs guest can do the same
  loop (quickjs builds with `Runtime::OptSizeSync`; `await` is moot against a
  world with no `func async` imports). Headless therefore needs no new async
  machinery — the loop from improvement #1 is reused as-is.
- Headless approval policy: auto-deny destructive calls by default, or take an
  explicit `approve` list. `gateCall` already flags destructive-ness.

## Streaming follow-up (not in this change-set)

Improvement #1 replaced the blocking `chat()` with the detached start/poll loop,
which is what makes cancellation work — but it did **not** wire the second
half of that loop's payoff: streaming progress into the UI.

Today the UI still renders from the session document (`onProgress` in
`AgentWidget.tsx`), so a turn appears as "working…" and then a burst of
tool-call cards when the iteration returns. The detached child streams into
its own console; nothing drains it yet.

What's missing, ported from `solx-prompt/crate/src/llm.rs`'s `drain_console`
(which `chat.ts` deliberately left out):

- `console/tail` against the child's console for presence/pacing, then
  `console/copy` to pull *this* invocation's entries into the widget's own
  console — one bulk copy rather than a `console/print` per token.
- A `console/tail` (or `console/read`) long-poll in the widget's drive loop, so
  token output lands as it is produced rather than at iteration end.

Design point to decide: the harness's `Host` currently has no console surface —
it was added for the chat child but `drain_console` was dropped to keep the
first increment small. Streaming means extending `Host` (or the widget's
`Host` adapter) with a console tail/copy, and threading a per-iteration progress
callback alongside the existing `onProgress(result, session)`. The headless
follow-up shares this: a headless action streams by draining the same child
console, so whatever console surface is added here is reused there.

## Retiring the other three packages

Retirement is staged: mark deprecated first (so dependents are visible), then
remove. Order matters because of a dependency edge.

| package | disposition | why / what survives |
|---|---|---|
| `solx-prompt` | retire | the scaffold that motivated this. Its `parse.rs` is the cost of plan-parsing made visible. **Salvage before deletion:** `llm.rs`'s detached start/poll/cancelled loop is being ported into solx-agent (improvement #1) — port, then retire. The hand-rolled XML parser and `search.rs` budget code die with it. |
| `solx-xprompt` | retire | depends on `solx-inquiry` (`multi_inquire`); a UI over the planner. No unique assets worth keeping once the planner is gone. |
| `solx-inquiry` | retire | the origin of the plan-parsing lineage. Its Rust `intent`/`search`/`script` phases are superseded by solx-agent's native tool-calling + `sys__tool_search`. Retire only after `solx-xprompt` no longer imports it. |

**Sequence:**

1. Port the detached-call loop out of `solx-prompt/crate/src/llm.rs` into
   solx-agent (improvement #1). This is the only code that survives the
   lineage, and it should be carried over *before* any deletion.
2. Land the three solx-agent improvements (see below) and verify against the
   existing vitest suite.
3. Mark `solx-prompt`, `solx-xprompt`, `solx-inquiry` deprecated (README + a
   note in each `solx-package.json`), with a pointer to solx-agent. Leave them
   in place one release so any external dependents surface.
4. Delete `solx-prompt`, `solx-xprompt`, then `solx-inquiry` (last, since
   `solx-xprompt` depends on it). Confirm nothing else in the workspace imports
   them before removal.

## Suggested implementation order

1. **Improvement #1** — port `llm.rs` detached loop into `turn.ts` (or a new
   `chat.ts`), wire `drain_console`/`finish`, replace the blocking `chat()`.
   Update the vitest fake `Host` to the new call shape. This is the foundation
   the other two don't depend on, but it's the largest and riskiest piece, so
   it goes first and is validated alone.
2. **Improvement #2** — remove the grant editor from `SetupPanel`, drop
   `previewTools`/`searchActionPaths` UI wiring and the `widenGrant` surface.
   The grant is pinned to `*`; exclusion is `tool_exclude` config only. Small,
   independent, and unblocks the "tooling config doesn't need to be there" ask.
3. **Improvement #3** — caption-through-to-transcript + collapse rendering +
   friendly session label. Cosmetic, independent, last.
4. **Retirement** — port-already-done, then deprecate → delete in the order
   above.

Items 2 and 3 are independent of each other and of 1; 1 is the only one the
others conceptually build toward but do not block on.

## Review findings still open

A review on 2026-09-20 of the working-tree changes that followed this
change-set — the `hits` → `items` rename, session delete, and tool-catalogue
eviction — turned up ten issues. Four were fixed there and then: the
`resolveSkills` N+1, the `deleteSession` fall-through, the armed-confirm bug,
and the fake host's search shape. The rest are recorded here so they do not
have to be rediscovered.

### The server contract changed under the harness

Everything in this group traces to solx-core commit `330197d`, which changed
`DocManager::search` from `Result<SearchResults>` to `Result<Page<Document>>`
(`solx-surface/src/managers.rs:55`). Two things changed at once and only one
was noticed. The envelope key (`hits` → `items`) was caught. The **payload**
was not: hits used to be shallow `SearchHit`s and are now whole documents,
`contents` included, with `score` gone. `solx-docs/src/lib.rs:641-646` is
explicit that this is so callers "no longer need a second round-trip."

#### 1. `refreshSessions` fetches 50 full session transcripts

`AgentWidget.tsx:102-112` searches `SESSION_PATH` with `limit: 50` to build a
`SessionSummary[]`, and reads three fields off each row: `name`, `title`,
`summary`. Every row now also carries the session's entire `contents` — full
message history, tool definitions, call log. It runs on mount, after every
drive loop, and after every delete.

**This cannot be fixed inside solx-agent.** Neither `SearchQuery` nor
`ListOptions` (`solx-surface/src/query.rs:216-235` and `:21-45`) has a
field-selection option, and `list` returns `Page<Document>` as well, so the
server sends `contents` however the query is phrased. Two ways out:

- **Server-side projection (preferred).** Add an optional `summaryOnly` to
  `SearchQuery` and honour it in `solx-docs`'s `search` by selecting
  `NULL as contents` in place of `d.contents`. Contained to two Rust files
  plus the call site, and every search caller benefits — the list endpoints
  have the same problem waiting for them.
- **Client-side mitigation only.** Lower the limit, and stop re-searching
  after every drive loop and delete (patch the local `sessions` array
  instead). That reduces how often the cost is paid; each fetched row still
  carries a whole transcript.

Deferred deliberately: it is a solx-core API change and did not belong in a
solx-agent change-set.

#### 2. Memory's `summary` duplication is no longer load-bearing

`recallMemories` (`knowledge.ts:80`) and `saveMemory` write the memory text
into `summary` *as well as* `contents.text`, specifically so recall could be
one search with no follow-up gets. Now that search returns `contents`, the
duplication buys nothing. It is harmless, and `summary` is still the right
home for a short form — but both comments assert a constraint that no longer
exists, and anyone reading them will draw the wrong conclusion about what a
search costs.

`resolveContext` (`knowledge.ts:164-212`) likewise discards the `contents` it
is now handed and re-reads each document on demand in `readContext`. That one
is probably right as it stands — the frozen index is deliberately small and
`CONTEXT_READ_CAP` truncates at read time — but it should be a decision
rather than an accident.

#### 3. The fake will drift again

`tests/fakeHost.ts` now matches the server, but nothing keeps it that way.
The one test that would have caught this — `tests/live.test.ts`, which exists
precisely to check the fake against a real server — is skipped unless
`SOLX_TOKEN` is set, and it was skipped when `330197d` landed. That is how a
dead round-trip survived in `resolveSkills` with a fully green suite. Either
run the live test on a schedule, or accept that fake-versus-server drift is
found only by reading solx-core's diffs.

### Tool-catalogue eviction

#### 4. Eviction is FIFO, so it drops the tools most likely in use

**Confirmed in a live run — this is an observed defect, not a predicted one.**

`sysTools.ts:229-236` walks `Object.keys(session.tools)` in insertion order,
so the first things evicted are the turn's *initial* catalogue — the tools
resolved from the user's opening request, which are the ones the model has
actually been calling. Combined with `want` being the whole `cap` once the
budget is exhausted (`sysTools.ts:188`), a single `sys__tool_search` can
replace the entire catalogue mid-task.

Worse than "the initial catalogue goes first": once that is consumed,
*consecutive searches cannibalise each other*, because the previous search's
results are now the oldest entries. Session `/agent/sessions/upland-bramble`,
`catalogue_cap: 16` against 44 matching tools:

```
q:"file"  -> adds file-put, file-copy, file-delete, file-list, ...
q:"save"  -> "To make room, these were dropped: file-copy, file-delete, file-put"
later     -> act__builtin__file__file-put  REFUSED   <- the run dies here
```

The model fetched the tool it needed, lost it one search later, and was
refused when it finally went to write its JavaScript source. `file-list` was
re-fetched by name after being evicted the same way. The eviction *mechanism*
works and the message renders correctly; the *policy* defeats the task.

LRU would serve the stated intent ("swap in a more relevant tool") better than
insertion order, and needs no new state: `session.calls` already records what
was called, so "evict what has not been called recently" is available today.
The narrowest fix that would have saved this run is smaller still: never evict
a tool added during the current turn.

#### 5. The defensive eviction block is unreachable

`sysTools.ts:237-246`. Its own comment reasons correctly that it cannot run:
`encodeToolName` (`catalogue.ts:41`) is a collision-free encoding of a ref,
and `resolveCatalogue` already skips refs present in `known`, so no key in
`cat.map` can collide with an existing key in `session.tools`. The same
reasoning makes the `if (cat.map[tn]) continue` guard on line 235 dead.

Worth deleting rather than keeping. It is also the only eviction path that
does *not* check `cat.map` — so in the world where the invariant did break, it
would evict a tool the search had just added, which is the opposite of what it
is there to prevent.

#### 6. The eviction test does not test eviction

`tests/knowledge.test.ts:273-289`. `expect(Object.keys(s.tools).length).toBe(2)`
holds under both the old "refuse when full" behaviour and the new "evict to
make room" behaviour; only the assertion on the returned message separates
them. It never checks that the newly found tool is in `s.tools`, or that the
evicted one is gone — which is the whole point of the change. Assert both.

### UI

#### 7. A live session can render as "— New session —"

`Header.tsx:93-111` binds the select to `value={sessionId ?? ""}`, but
`sessions` is refreshed only *after* a drive loop completes, while
`setSessionId(current.id)` happens as soon as the session is created. In
between, no `<option>` matches the value, so the browser falls back to the
first one. For the whole of a new session's first turn, the dropdown says you
are on a new session. Render a synthetic option for `sessionId` when it is not
in `sessions`.

#### 8. Consecutive iterations have no separator

`TurnBlock.tsx:74`. Dropping the `iteration N` label removed the only visual
break between iterations under "show work", so narration and tool cards from
successive iterations now abut with a 4px gap. Cosmetic. A hairline rule would
do the job the label was doing, without re-introducing a number nobody needs.

### Tool discovery

Both of these were found by running a real inquiry through the widget
(session `/agent/sessions/upland-bramble`, model `kimi-k2.7-code:cloud`):
*"Go to wikipedia and search for lord of the rings and extract the first 3
documents. To do this you'll need to create a javascript action which extracts
a wikipedia page to a solx document."* It exhausted 24 iterations across two
turns and never built the action.

#### 9. A multi-word `sys__tool_search` matches nothing

The first turn called `sys__tool_search` exactly once, with
`q: "type schema get"`, and got back *"no further tools matched, within what
you are permitted to call"*. That is not a catalogue problem. `fts_match_query`
in solx-docs turns each whitespace-separated term into `"term"*` and **ANDs**
them, so a descriptive phrase only matches an action whose text contains every
word. Measured against the live catalogue:

```
file              16        file store write   0
store              7        type schema get    0
write              8        list files         2
```

Any natural phrasing returns zero. This is **the same bug class already fixed
for skills** — `resolveSkills` has a long comment explaining why it no longer
passes `q` at all, because the turn's user message ANDed to nothing and skills
therefore never loaded. `sys__tool_search` still passes the model's `q`
straight through to `search-actions`, so the one escape hatch from a saturated
catalogue is broken for exactly the queries a model naturally writes.

AND is defensible for document search, where precision matters. For tool
discovery the model wants recall. The fix belongs in `resolveCatalogue` rather
than in `fts_match_query`, so document search is left alone: when a multi-term
`q` returns nothing, retry per-term and merge by rank.

#### 10. A refused name-guess is a dead end, though it could be a recovery

When search returns nothing, the model falls back to *constructing* tool names
— `act__builtin__file__file-list`, `act__builtin__type__search-types` — because
`encodeToolName`'s scheme is guessable from the names it can already see. The
gate refuses correctly, and the refusal text is good: it says names cannot be
guessed and to call `sys__tool_search` instead. But the model repeated the same
guess three times and burned four calls on it.

The harness is holding the answer at that moment. A refused name is a decoded
ref; it can be checked against the catalogue and the refusal can name the
search that would find it, or say plainly that no such action exists. That
turns a dead end into a recovery without weakening the invariant — the point of
the rule is to stop hallucinated names being dispatched, and saying "that one
is real, here is how to ask for it" does not dispatch anything.

#### 11. `catalogue_cap` is low enough to guarantee churn

The session ran with `catalogue_cap: 16` while 44 actions matched
(`tools_dropped: 28`). It therefore *starts* saturated: every subsequent tool
the model needs must displace one it already holds, which is what makes
findings 4 and 9 bite so hard. Raising the cap does not fix either bug, but it
is a one-value change that removes most of the pressure that exposes them.

### The fixes, verified against a live rerun -- and the actual blocker found underneath

Findings 1-11 above were fixed in solx-agent on 2026-09-20 (`catalogue_cap`
16->32, `max_iterations` 12->24, per-term search retry, LRU eviction, the
regression tests). The rebuilt bundle was pushed to the live install and the
identical prompt from `upland-bramble` was rerun as a fresh session,
`hidden-antler`, on `kimi-k2.7-code:cloud`.

The three catalogue fixes held up cleanly and are no longer theoretical:

- **Zero name-guessing refusals.** `upland-bramble` burned 4 calls on refused
  `act__builtin__file__file-list`-style guesses (finding 10). `hidden-antler`
  never guessed a name once, across 57 calls.
- **Zero search dead ends.** `upland-bramble`'s one `sys__tool_search` call
  (`q: "type schema get"`) returned nothing and the turn never recovered
  (finding 9). `hidden-antler` searched repeatedly and always got tools back.
- **No cannibalisation.** `upland-bramble` lost `file-put` to eviction one
  search after fetching it (finding 4). Across four turns and dozens of
  searches, `hidden-antler` never lost a tool it still needed.

It got an order of magnitude further: a real `http-request` fetch of the
Wikipedia page, a `file-put` of real JavaScript, and a compiled, installed wasm
action -- none of which `upland-bramble` ever reached.

**It still did not finish, and raising the iteration cap further would not
have helped.** Three separate turns exhausted at their cap (12, then 24, then
40) while approving the same rebuild over and over, because the model was
stuck: every version of its JavaScript source defined a bare `main()` function
rather than the required `export const runner = { run(actionName, params) {
... } }` (`/agent/skills/javascript-actions`, verified present in the
session's system messages verbatim -- the skill delivery this change-set
depends on worked correctly). Each build *compiled without error* and then
*trapped at run time* with an identical, unattributed wasm backtrace
(`<unknown>!<wasm function N>`, ending in `wasm \`unreachable\` instruction
executed`) on every single attempt. Nothing in that message points at the
export shape, so the model never had the information it would have needed to
self-correct, and instead thinned its own source down to a one-line stub
across four rewrites while getting the exact same trap.

This is a solx-quickjs finding, not a solx-agent one: a JS module built with
the wrong export shape should fail *at build time*, naming the missing
`runner` export, the same way a Rust action fails to compile against a
mismatched WIT interface. An opaque runtime trap is close to the worst
possible signal for a model (or a person) debugging this.

**Fixed, 2026-09-20, in solx-quickjs (`src/main.rs`), not by changing quickjs.**
The user asked whether a bare `main`-style export could simply be made to
work. It can, but not inside solx-quickjs: `componentize-qjs`'s own binding
convention already supports a bare exported function -- see its README quick
start -- it is solx's own `custom-action.wit` that wraps `run` inside an
*interface* export (`export runner;`), which is what forces the
`export const runner = { run(...) {...} }` wrapper in JS. Making a bare
`export function run(...)` bind would mean dropping that interface wrapper --
a breaking change to a WIT contract duplicated across four packages, whose
host-side dispatch (`solx-actions/src/wasm/host.rs`'s
`.sol_actions_runner().call_run(...)`) and every already-compiled action in
the fleet (Rust and JS alike) would need to change and rebuild together. Out
of scope for a live system without a deliberate rollout.

What shipped instead closes the actual gap: `build-javascript-action` and
`build-javascript-file` now check the entry source for an `export ...
runner` before compiling at all, and refuse with the exact fix in the error
message if it is missing -- turning the opaque runtime trap into an immediate,
actionable build failure. The first attempt at this check inspected the
*compiled* component's exports instead of the source, on the theory that it
would be more precise; it was not just imprecise but wrong, and instructively
so. `componentize-qjs` uses `wit-dylib`, which generates a wasm-level export
trampoline for *every* export the WIT world declares, unconditionally,
whether or not the JS backs it -- confirmed by compiling `export function
main() {}` and inspecting the result with
`wasmtime::component::Component::new(...).component_type()`: `run` was
present and well-typed regardless. A structural check of the output cannot
see this bug; only the source can. Verified against the real toolchain in
both directions: the exact `main()` source from the live session now fails
immediately with the fix-it message, and the documented `runner` shape still
builds and uploads normally.

## Prioritized fixes, from the live run

Ordered by how much each would have changed the outcome of
`/agent/sessions/upland-bramble`, not by how interesting the bug is. The task
was "search Wikipedia for Lord of the Rings, extract the first 3 pages, and
build a JavaScript action to do it." It exhausted 24 iterations across two
turns, got as far as creating a type and announcing a correct plan, and was
refused on `file-put` when it went to write its source.

### P0 — Size the session for the model. No code.

`catalogue_cap: 16` against 44 matching actions, and `max_iterations: 12`
against a task whose minimum clean path is roughly twelve steps (find tools,
read two parameter schemas, save a type, `file-put` the source, build the wasm,
run it, save three documents, verify).

This is the change most likely to have flipped the result on its own. Every
tool the run needed was inside those 44 — `file-put`, `http-request`, the
quickjs build actions. At a cap that held them all, `sys__tool_search` would
never have been on the critical path, so **neither finding 9 nor finding 4
could have bitten.** Both turns died inside machinery that only exists because
the catalogue does not fit.

The caveat is the reason the cap exists: `tools_defs` is tokens, and 16 is a
sane budget for a small local model. This session ran on `kimi-k2.7-code:cloud`,
where it is needlessly tight. So the fix is not "raise the constant" but
**make the cap model-aware** — a large-context model should start holding the
whole catalogue. Until that exists, raising the default is a one-value change
worth making today.

### P1 — Multi-term tool search (finding 9)

P0 makes the search path uncritical *for a catalogue this size*. It does not
survive a larger install, and it leaves the agent with no way to recover
whenever the cap does bind. Turn 1 was a total write-off — twelve iterations of
failed searches — purely because of this.

In `resolveCatalogue`: when a `q` of two or more terms returns nothing, retry
per-term and merge by rank. Leave `fts_match_query` alone; AND is right for
document search, and this is the same conclusion `resolveSkills` already
reached when it dropped `q` entirely.

Cheap partial: say "use a single word" in the `sys__tool_search` tool
description. That alone would have saved turn 1, and it is one line — but it
asks the model to work around the bug rather than fixing it, so it is a
stopgap, not the fix.

### P2 — Never evict what the current turn just added (finding 4)

Turn 2 died precisely here: `q:"file"` fetched `file-put`, `q:"save"` evicted
it, and the run was refused when it needed it. With P0 in place this stops
being load-bearing, but it is still wrong, and it will resurface on any install
big enough to make the cap bind again.

Narrow fix: exclude tools added during the current turn from the eviction
candidates. Full fix: order candidates by least-recently-*called* using
`session.calls`, which already carries the data. Do the narrow one first — it
is a few lines and it is what this run needed.

### P3 — Make a refused name-guess a recovery (finding 10)

Four calls in this session went to guessing encoded names and being refused,
with no new information coming back. Decode the refused name, check it against
the catalogue, and either name the search that would find it or say plainly
that no such action exists. Small change, and it converts the most common
failure mode after a bad search from a loop into a step forward.

### P4 — Make the eviction test able to fail (finding 6)

The current test passes under both the old and new behaviour, and it would
have passed just as happily against the thrashing policy that killed this run.
Assert *which* tool survives an eviction, not how many.

### What the run confirmed is already working

Worth recording so it is not re-investigated: skills resolution loaded all
eleven skills straight out of the search result, which is the `resolveSkills`
N+1 fix working against the real server and proof that `search-documents`
really does return `contents`. The session dropdown rendered titles. The
eviction message rendered correctly. The gate refused every guessed name it
should have. The mechanisms are sound; the policies around them are what need
work.

**Update, after the fixes landed and were verified live:** P1 (search) and P2
(eviction) are confirmed, not just argued -- see "The fixes, verified against
a live rerun" above. P0 helped (three turns of real progress instead of one
dead one) but a fixed cap was never going to be enough on its own: the task
that finally surfaced needed real per-problem iteration count, which no
constant can promise. The actual wall this run hit was outside solx-agent
entirely (finding 12) -- worth remembering before spending more tuning effort
on the numbers in `refs.ts`.

## Open questions

- Whether the headless follow-up is Rust-wasm or quickjs. This change-set is
  deliberately neutral on it (the detached loop ports to either). Decide when
  the follow-up is actually scoped.

## Resolved decisions

- **Grant editing is dropped** (item 2): the grant is pinned to `*`, and tool
  visibility is controlled by search plus the `tool_exclude` config (∪ legacy
  `mcp_exclude`, ∪ `solx:hidden`), enforced host-side via `ToolPolicy`. No
  in-UI allowlist remains; the trust boundary is `tool_exclude` config, not an
  operator-curated grant.
