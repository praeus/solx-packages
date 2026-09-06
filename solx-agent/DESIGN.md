# solx-agent: design

## Status

Implemented, and folded. This package replaces two: `solx-conductor` (an
agent harness as six wasm actions) and `solx-chat` (a React widget that drove
it). The harness now runs in the widget bundle.

For the widget system itself — how a bundle is built, published, mounted and
handed a client — see
[`solx-packages/docs/widget-system.md`](../docs/widget-system.md).

## Why the halves were folded

The split looked like backend and frontend. It was not. The loop was
*already* in the browser: `solx-chat` stepped one iteration at a time and
deliberately never called `conductor-run`. What was left server-side was a
single iteration — build a prompt, call `ollama-chat`, gate, dispatch,
persist — and every one of those is an ordinary `exec()`.

That is the whole argument. A wasm guest's only capability *is* `exec`
(`solx-core/solx-wasm/wit/custom-action.wit` imports nothing else), so the
component was using nothing a widget lacks. The boundary was buying a
compile step, not a capability.

Folding bought two things:

- **The iteration loop.** Editing the harness was JS → solx-quickjs →
  componentize-qjs → wasm → reinstall. It is now `vite build`.
- **Freedom to fix the session model**, below — which was the real problem.

Seven actions became one, and eleven registered types became three: eight of
those types existed only to describe action signatures that no longer exist,
and `solx-chat/src/conductor/types.ts` stopped being a hand-written mirror of
them.

### What this costs

Headless drivability. There is no `solx exec agent-start` any more; an agent
run happens because a human has a tab open. Accepted deliberately.

Post-hoc inspection survives intact — sessions are ordinary documents, so
`solx search --path /agent` still works and the CLI can read any transcript.
Only *launching* moved into the browser.

## The port was mechanical, and so were the tests

The guest reached its host through exactly one import, `exec(ref, json)`. A
widget's seam is `client.actions.exec(path, name, params)`. So
`harness/host.ts` is a thin adapter and everything below it moved
near-verbatim; the one real cost is that guest `exec` was synchronous and the
client's is a Promise, so everything below the seam became `async`. Dispatch
was already strictly sequential, so no ordering guarantee was lost.

The test harness moved for the same reason. `solx-conductor`'s fake host had
to rewrite WIT imports to reach a fake; this one does not, because the fake
simply *is* a client.

## A conversation, not a goal

This is the part that is not a port.

A conductor session was goal-shaped. `start` took a `goal`, and that first
turn was special three times over: it titled the document, it selected the
tool catalogue, and it was the only turn a session got until `say` was added
to make conversation possible. Five of those seams had already been worked
around. One could not be:

**the catalogue was frozen at `start` and selected by the first message.**

Ask for a summary of some documents, then ask to email it, and the second turn
could never reach the mail actions. The only escape was to start a new
session — throwing away exactly the continuity `say` had been built to
preserve. `solx-chat`'s setup panel locked once a session existed and said so
plainly, which was an honest UI for a model fighting the interaction.

So one field became two:

- **`grant`** is the security boundary. Nothing the model does widens it, and
  `sys__tool_search` still re-searches only within it.
- **`tools`** is the selection shown to the model, re-resolved from *this
  turn's* message at the top of every turn.

The invariant the gate exists for survives verbatim — no amount of talking
widens what the model may call — because talking now re-selects from the grant
rather than extending it. And `start` and `say` collapse into one `send`,
because turn one stops being special.

Three consequences worth stating:

- **Widening the grant is a human action.** The rule is that the *model*
  cannot widen its own reach, not that reach is immutable; the operator is
  the trust root. So the setup panel stays live, and a change appends a system
  turn — the transcript must record when reach changed, and the model has to
  be told what it can now see.
- **A message that matches nothing falls back to the grant unfiltered.**
  Otherwise "list the files" against a documents-only grant strips the model
  of the tools it was using and it cannot even explain why. The fallback
  re-resolves rather than reusing the previous catalogue, so a path the
  operator just revoked can never linger.
- **`final` was renamed `idle`.** The loop sets it whenever the model replies
  without calling a tool, which covers "the task is done" *and* "which of
  these did you mean?" — and the harness deliberately does not try to tell
  them apart, because the model already signals structurally and guessing from
  prose would contradict the resolve-never-parse principle everywhere else.
  Either way the next thing is a human. The UI always rendered `final` as
  "your turn"; the rename makes the data model agree with the screen instead
  of the screen reinterpreting the data.

`max_iterations` likewise became a per-turn budget (`turn_iteration`, reset
each turn) with `iteration` left monotonic as the audit trail — replacing a
rebase that existed only because the budget had been scoped to a goal.

## The system preamble is the whole contract

The harness injects no base prompt at all. The model sees only the operator's
preamble, the memory/context/skill blocks, the conversation, and the tool
definitions. That makes `session/store.ts`'s `DEFAULT_PREAMBLE` a real
artifact, and it is exposed and editable in the setup panel rather than hidden
in the bundle.

### What goes in it, and what goes in a skill

The preamble was for a long time nine lines of generic tool-calling advice,
which was honest when there was nothing solx-specific to say and wrong once
there was. A model driving this harness has to know what solx *is* before its
first call: that every tool is a row, that a reference is a path and a name
joined, that a result arrives wrapped with the payload under `result`, that
parameter spelling is not uniform and a wrong key is dropped rather than
refused.

None of that belongs in a skill, because a skill loads only when one of its
globs matches a tool already in the catalogue. That conditionality is exactly
right for "here is how `set_field_at_path` differs from `set_field`" and
exactly wrong for "here is what a reference looks like" — the second is needed
to make the *first* call, before anything has matched.

So the split is: orientation true of every session goes in the preamble and is
paid on every iteration; anything tied to a family of tools goes in an
`AgentSkill` document and is paid only when that family is in play. The
preamble is kept to about thirty lines because this harness targets small local
models, and the seeded skills are kept under a thousand characters each because
`SKILL_TOTAL_CAP` is 8000 across a turn.

## Durability: what one wasm invocation used to give for free

This is the one genuine regression from folding, and the fix makes the result
better than what it replaced.

A `conductor-step` was atomic: one invocation loaded, chatted, dispatched and
saved, and a closed tab could not interrupt it — the invocation kept running
server-side. Driving from the page gives that up.

So the loop persists *more* often rather than less: after the model replies
(with its pending calls, before any dispatch), after **each** dispatched call
records its result, and again at flush. Resumption then falls out of the shape
`completeTurn` already had — a `pending[]` with some results filled and some
null is exactly what it knows how to finish, so `step` checks for one before
asking the model anything new.

The cost is one `entity_save_document` per tool call instead of per iteration.
That is affordable only because the session type does not declare `messages`
or `calls`, so FTS never walks the transcript — the two decisions are load
bearing together.

Net effect is an improvement: a crashed step used to lose a whole iteration
*including tool calls that had already committed their side effects*. Now the
document always describes what actually happened.

## Rendering the work

`transcript.ts` reconstructs the nesting a reader wants — a user message, the
iterations it caused, each call's arguments and outcome — from a flat
`messages[]` plus a parallel `calls[]`. It relies on two invariants from
`completeTurn`: every pending call gets exactly one `role:"tool"` turn, and
both arrays are appended in lockstep, so results pair to calls by position.

- The newest turn stays expanded; older ones collapse to "3 calls · 2
  iterations". A turn suspended for approval must be expanded — its work is
  the context for the decision being asked for.
- System turns (memories, context index, skills, grant changes) collapse into
  a chip. They are reference material handed to the model, not conversation.
- Tool calls always show their arguments one click away. The same tool with
  different arguments is a different action, which is why approval binds to a
  `call_id` covering both.

`transcript.ts` and `harness/` are deliberately free of React, so the loop and
the message segmentation can be tested without a DOM.

## Browser storage

Namespaced `solx-agent:` — a widget shares its host page's origin, which is
the same `localStorage` solx-web keeps `solx.serverUrl` and
`solx:action-runner:*` in. Deliberately thin: the model you picked, how you
like new sessions set up, and which session was open. Losing it costs
preferences, never history.

## What the fake host cannot check

`tests/fakeHost.ts` mirrors solx-core's behaviour *as it was read out of the
source*. If solx-core changes, the tests keep passing and the package breaks.
`tests/live.test.ts` exists for exactly that gap, and has already earned it:

- **`q: null`.** Action params are schema-validated, and an optional string
  field is `{"type":"string"}` — so an explicit null is an *error*, not
  "absent". `{q: query || null}` therefore broke every no-query search
  silently, which is the path a catalogue fallback and a path-only context
  spec both take. `harness/host.ts`'s `compact()` omits null keys instead, and
  the fake now rejects an explicit null so this cannot hide again.

A fake more lenient than the real thing is worse than no fake, which is why
the fake reproduces that strictness rather than forgiving it.

**Verify a wire key against the source, not against a running server.** While
porting, a probe against a `solx-server` binary four days older than commit
`3974d0b` reported that document writes wanted snake_case `type_ref`. They
want camelCase `typeRef` — `DocumentInput` carries `#[serde(rename_all =
"camelCase")]` — and the conductor had it right all along. A stale binary is a
confident liar, and `grep -A` starting at the `struct` line hides the
attribute above it.

- **The skills search.** `resolveSkills` passed the turn's user message to
  `search_documents` as `q`. solx-docs turns each whitespace-separated term
  into `"term"*` and **ANDs** them, so a skill loaded only if its text
  contained every word of the message — which for any real sentence means
  never. Every skill test here happened to send the single word `"document"`,
  so all four passed against a feature that did not work.

  This one is worth separating from the `q: null` bug above, because the fake
  was **not** at fault: `matches()` ANDs its terms exactly as FTS5 does. A
  faithful fake is not enough on its own when the fixtures are unrepresentative
  of the input. The fix was to stop querying at all — the globs were always the
  real selector — and the regression test now sends a full sentence, which is
  the shape of input that broke it.
