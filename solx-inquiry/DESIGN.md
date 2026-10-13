# solx-inquiry: design

## Status

Implemented. A companion to [solx-agent](../solx-agent), built by subtraction:
it is the same shape of thing -- a React widget wrapping a harness that drives
`solx-ollama` over the solx action catalogue -- with most of the machinery
removed and one idea added.

For the widget system itself -- how a bundle is built, published, mounted and
handed a client -- see [`solx-packages/docs/widget-system.md`](../docs/widget-system.md).
For the agent this is a companion to, see [its DESIGN.md](../solx-agent/DESIGN.md).

## The one idea

solx-agent resolves a catalogue of tools per turn and lets the model drive. It
is general, and generality is expensive in three currencies:

1. **Prompt tokens.** Every tool definition rides on every iteration. The
   agent has a catalogue *cap* and a `sys__tool_search` eviction dance on the
   critical path of every task precisely because a small model drowns.
2. **Model competence.** A model has to choose a tool, spell its wire name
   (`act__builtin__document__search-documents`), and get its parameters right
   -- and solx schemas are open, so a misspelled key is dropped in silence.
3. **Risk.** Because the model may call anything the grant reaches, the agent
   needs a default-deny gate, a destructive-call detector, and human approval
   that suspends the turn.

Inquiry buys all three back by *not asking the model what tool to use*. It
fixes the verb set, calls those verbs itself, and asks the model only two
questions it is good at: "what should I look up?" and "what does this say?".

The verbs are all reads. That single constraint is what deletes the gate, the
approval UI, the status set's suspension states, and the entire `pending` /
resume-after-crash machinery the agent's [`turn.ts`](../solx-agent/src/harness/turn.ts)
documents at length. `plan → gather → answer` has no half-finished state to
resume.

## Plan, gather, answer

Three phases, one retry, no loop.

**Plan.** One `ollama-chat` call with `format: PLAN_SCHEMA` -- a JSON Schema --
and **no `tools`**. This is the load-bearing choice. Because the plan call uses
`format` rather than tool-calling, a model with no tool support can still plan;
and because there is no tool loop, a model that answers in the wrong shape is
absorbed by [`plan.ts`](src/harness/plan.ts) rather than looping.

`plan.ts` trusts nothing the model returned: a verb outside the five is dropped
with a note, a `get` missing half its reference is dropped, a limit is clamped
to the hard cap, more steps than the cap are truncated with a note, and a
`direct` alongside real steps is discarded (the steps win). Every repair is a
*note on the step*, never a thrown error -- the package's whole premise is that
a small model cannot be asked for precision, so precision is demanded of the
schema and what slips through is absorbed.

**Gather.** No model call at all. [`execute.ts`](src/harness/execute.ts) maps
each step's verb to a real builtin and runs it. It is fully deterministic, and
it is where every wire-key trap lives: `search`/`list` send camelCase
`pathPrefix`/`typeRef`, and `list` sends **no `q` key at all** -- an explicit
null fails schema validation, which is the `q: null` bug the agent's DESIGN.md
records.

Three budgets bound the output, and they are separate because they fail
differently: a per-step `limit`, a total `max_docs`, and a total character
`doc_budget` applied when the answer is assembled. Too many documents crowd
the writer's prompt; too few characters starve it; and without the per-document
share inside `renderDocuments` one huge document would spend the whole budget
alone.

**Answer.** One free-text `ollama-chat` call, no tools, no format, grounded in
the gathered documents. The writer is told the documents are its only source,
that a short "I could not find that" is a correct answer, and to cite by
reference. A `direct` plan short-circuits this call: the planner already said
no lookup was needed, and re-asking the same model to rewrite its own sentence
would only add latency.

### The retry, and why exactly one

A plan that runs and finds nothing gets one second attempt, with the failure
named in the follow-up prompt, and executes that. Then it stops.

One retry, never two: a model that found nothing twice will not find something
on the third try, and an honest "I could not find that" is more useful than a
fourth attempt. The retry is skipped when disabled, and -- importantly -- when
the first plan was *empty*, because an empty plan means the planner chose
`direct` and re-asking would only repeat it.

## The corpus index is the entity optimization

Before planning, [`corpus.ts`](src/harness/corpus.ts) renders what exists:
document paths with counts (from `entity-list-document-paths`, one row per
path, so a big store returns the same handful of paths a small one does), and
the full refs of the types those documents carry (one sampled row from each of
the top few paths -- see bug 4 for why not the type registry). The planner is
told to copy a path or type from that list rather than invent one, and
`execute.ts` repairs the copying mistakes a model still makes.

This is the concrete answer to "optimize it for the solx entities rather than
requiring the model to figure out what tools to use." It replaces the agent's
catalogue with *vocabulary*:

| | solx-agent | solx-inquiry |
|---|---|---|
| What the model is shown | tool definitions (name, description, schema) | paths and type refs, in prose |
| What it may do with it | **call** it by wire name | **copy** it into a param |
| Enforced by | a gate at dispatch | nothing -- there is nothing to enforce |

Because nothing in the index is callable, there is no gate. The worst a model
can do with a path it read here is search under it. That asymmetry is what
makes the whole package smaller than a third of the agent's harness.

## Real bugs the tests and live runs caught, worth recording

### 1. The inquiry found its own transcript

An inquiry's session document stores its plan, and a plan contains the search
terms. So a search for `zzznomatch` **finds the inquiry document that is asking
for it**, because the term is sitting in that document's stored contents.

Left unfiltered, every search returns the thread itself alongside the real
hits. Worse than crowding the budget: the writer would be handed the model's
own previous plan as source material and could cite it as a finding.

The fix is in `execute.ts` -- `isOwnPath` filters rows under `/inquiry` -- and
it is bypassed when a step deliberately scopes to the inquiry root, because
"what have I asked before?" is a legitimate question.

It is recorded here because the *test* found it, not a person: the assertion
was "a term that matches nothing yields zero documents", and it failed because
the pipeline had been quietly gathering the transcript.

### 2. A sentence-shaped query, and the transcript hiding it

Found by running the package against a real server. `q` goes to
`fts_match_query`, which turns each whitespace-separated term into `"term"*`
and **ANDs** them, so a phrase only matches a document containing *every* word.
The observed planner output was `"documents skill how a document is identified
identifier"` -- a sentence, which ANDs to nothing, returning an empty page that
looks exactly like "no such document exists".

The fix is a term-by-term retry when a phrase matches nothing, which
`solx-agent` already does for the same reason. But the retry was **itself**
defeated by bug 1, and this is the subtle part: the sentence genuinely matched
-- *the inquiry's own question*, which had just been saved verbatim into the
session document. The raw page count was non-zero, so no widening fired, and
then the own-transcript filter removed every match, and the search reported
empty. The fix is to judge the retry on **usable** rows -- rows that can
actually ground an answer -- rather than the raw page.

That interaction, between a filter and a fallback that both exist for good
reasons, is the kind of thing no unit test written from the design would have
produced. It needed a real store with a real transcript in it.

### 3. An empty plan, and a turn left permanently live

Two failures of the same kind -- the pipeline treating "the model did not
answer" as "the answer is no".

A model returned `{}` for a perfectly clear question. `cleanPlan` produced an
empty plan, the pipeline executed nothing, and the turn answered "I could not
find that" **without ever having searched**. The retry only fired when a plan
had steps and found nothing, so an empty plan fell through. The distinction
that matters is `direct`: an empty plan with a `direct` answer is a deliberate
decision not to look anything up and must not be retried; an empty plan with
nothing at all is a planner failure and must be. The retry now keys on that.

Separately, an abandoned run (a cancelled MCP call, a stopped `solx exec`)
dies *inside* an `await` mid-phase, so the write-path `catch` never runs and
the document keeps saying `planning` forever. Nothing resumes it -- inquiry has
no `pending[]` the way the agent does -- so two fixes were needed: `settle()`
makes the terminal state the single guaranteed exit on every path, and
`loadInquiry` closes out a live status it reads back once it is stale (no
write for fifteen minutes) -- not on sight, because a run still going in
another process (a headless MCP call, another tab) looks identical from the
reader, and closing it would write a false "interrupted" mid-run. Both are the
agent's durability lesson arriving from a different direction: the agent
persists *more* so a crash loses less, inquiry persists *finally* so a crash
cannot leave a lie.

The same "did not answer" confusion had a third form. A planner that ignored
`format` and wrote prose used to have that prose returned as a `direct`
answer -- ungrounded text with no lookup behind it, the one outcome this
package exists to prevent. `parsePlanReply` now recovers a plan from a code
fence or around a `<think>` block, and treats anything else as an empty plan,
which the retry asks again.

### 4. Vocabulary the model could copy but not use

Found over MCP against a real store. The index listed types by *name* from
`entity-list-types`, and the brief said to copy them "exactly as written". The
registry is dominated by builtin action-parameter types no document has, so
the model copied one -- `type_ref: "SearchDocumentsParams"` -- and, being a bare
name rather than a ref, it would not have matched even a real type. Every such
search returned an empty page. In the same session it also put a document path
in `type_ref` (`"/agent/skills"`), and a question whose answer sat at
`/agent/skills/javascript-actions` found nothing.

Two fixes, one per layer. The index now shows full refs of the types documents
actually carry. And `executePlan` takes the index as a `Vocabulary`: a
`type_ref` that is a known path becomes a `path_prefix`, a bare name resolves
to its one matching ref, and a scoped search that finds nothing sheds a
*suspect* filter (a type the index did not list, a prefix covering no known
path) and runs again, with a note on the step. A valid scope that finds
nothing is left alone -- that is a real answer. Shedding is one cheap lookup;
the alternative was the replan, a whole model call.

### 5. The own-transcript bug through the bypass

Found in the same session, on a question the store could not answer. The
replan, told to try "a broader path", scoped to `/inquiry/sessions` -- listed in
the index like any other path -- and the bypass from bug 1 let it gather
earlier inquiries *and the one being written*, which holds the question
verbatim. Two fixes: the current session's document is excluded from gathering
whatever the scope, because "what have I asked before?" is about *other*
inquiries; and `/inquiry` is no longer offered in the index, with one line of
the brief saying where past questions live for the question that is really
about them.

## Latency: thinking off for the plan

The plan call sends `think: false`. A thinking model otherwise reasons at
length before emitting a plan the schema already constrains to a verb and a
few words: the same planner call measured 125s with the model's default (a warm
model, 20,100 characters of thinking) and 2s with thinking off, against
qwen-3.6 on a real install. The writer keeps the
model's default, since summarizing documents is where reasoning is worth
paying for. `think` was added to the shared `ChatRequest` in `solx-widgets`
for this, as an optional pass-through; the agent does not set it.

## Deliberate subtractions from the agent

Every one of these is a thing the agent has that inquiry does not, and each is
justified by a verb never being destructive:

- **No gate.** No `HARD_DENY`, no `permitted()`, no allowlist. There is no
  reach to fence; the five verbs map to five fixed read-only builtins.
- **No approval.** No `ApprovalCard`, no `pending[]` for a human decision, no
  `awaiting_approval` status, no `approveAndContinue`.
- **No `call_id` hashing.** Approval binding existed to stop a reused approval
  for different arguments; there is nothing to approve.
- **No skills, memories, or context index.** The agent's three reference
  materials are for a model that must be taught how to use tools. Inquiry's
  model needs orientation about solx, which is in the planner brief, and
  nothing else.
- **No catalogue cap or `sys__tool_search`.** The verb set is fixed and tiny,
  so it is never near a budget.
- **No `blocked` / `exhausted` status.** Those exist for a loop that can run
  out of iterations or fail repeatedly. Three bounded phases cannot.

## Where inquiry kept the agent's shape on purpose

The resemblance is not laziness -- these are load-bearing and were kept
verbatim or near it:

- **The `Host`/`compact()` seam** is imported from
  [`solx-widgets`](../solx-widgets/src/wrap/host.ts), not redefined. It was
  promoted out of the agent once a second widget wanted it; this is that
  second widget.
- **One inquiry is one document**, under one root, with undeclared transcript
  fields so FTS never walks them. Same reasoning, same benefit: writing the
  document once per phase is affordable.
- **Name minting fails closed**, with the same collision check, because
  `entity-save-document` is an upsert and a collision silently overwrites a
  live transcript.
- **The generation-guarded drive loop** (`genRef`) in the widget is copied,
  because the bug it prevents -- a superseded loop publishing into the wrong
  thread -- is identical here.
- **The fake host is a client**, not a WIT-import rewriter, for the same reason
  the agent's is.

## What was promoted, and what is still deferred

- **`chat.ts` — promoted to `solx-widgets`.** The detached
  `action-start`/`poll`/`stop` call now lives in
  [`solx-widgets/src/wrap/chat.ts`](../solx-widgets/src/wrap/chat.ts), beside
  `host.ts`, following the same rule: promote a second time it is wanted. The
  shared module owns the *mechanism* (cancellation, the blocking fallback,
  terminal status, message extraction); each caller owns the *shape*.
  `solx-agent/src/harness/chat.ts` became a thin adapter that binds a session's
  fields and narrows `tool_calls`; this package's `harness/chat.ts` became a
  plain re-export, because the shared generic request was shaped for it — the
  planner needs `format` and no tools, the writer needs neither. The one thing
  promotion changed in this package's own design is that the request type is
  no longer local: `ChatRequest`/`ChatResult` come from the shared module, and
  the local `ChatMessage` went away.
- **Streaming the writer.** `solx-ollama` streams into its own console; the
  widget renders from the document. Wiring one to the other is a feature, not
  a redesign.
- **Embeddings.** The `search` verb is the seam. Today it is FTS5 term-matching
  plus the corpus index; a vector search would change `execute.ts` and add a
  verb, not the pipeline.
- **`serverConfig.ts`** in `headless/` is still a copy of the agent's. It is
  the obvious next promotion if a third headless widget appears.

## The port question, answered

Unlike the agent -- which ported a wasm harness and had to record what that
cost (durability, headless drivability, the frozen catalogue) -- inquiry was
written fresh against the same seam. So there is no porting debt here, and the
interesting comparison is not "what did folding cost" but "what does
*subtraction* cost."

It costs reach, and it buys predictability: two model calls instead of an
open-ended loop, no approval to design, and a failure mode (`plan.ts` drops and
notes) that is a data transformation rather than an error path. For the
question this package answers -- what is in the store, and what does it say --
that is the right trade. For anything else, the agent is next door.
