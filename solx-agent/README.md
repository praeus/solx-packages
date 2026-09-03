# solx-agent

A supervised agent thread over the solx action catalogue.

The action registry *is* a tool catalogue — every row carries a name, a
description, and a `paramTypeRef` pointing at a JSON Schema, which is what a
model's tool format wants. This package resolves a catalogue from a search
query, hands it to a model through `solx-ollama`, dispatches whatever the
model asks for back into solx, and renders the whole thing for a human who
can stop it.

Chat-shaped, but not a chat: one message can mean a dozen iterations and
thirty tool calls against real documents, so the work is shown rather than
hidden, and anything destructive stops for a decision.

**One action.** `/packages/solx-agent/agent-widget` returns a
`WidgetDescriptor`; everything else is a document. The harness runs in the
widget bundle, not behind an action — see [DESIGN.md](DESIGN.md) for why.

## Install

```sh
npm install && npm run build
solx install-package ./solx-agent
```

`solx-ollama` must also be installed, with a model that supports tools.

Then exec the action from solx-web's action runner and the widget mounts.
`solx exec /packages/solx-agent/agent-widget` on its own just returns the
descriptor — there is no CLI entry point, which is the one thing this
package gave up by folding (see DESIGN.md, "What this costs").

## Three kinds of document

Everything the agent knows lives in the document store, so it is authored,
searched, edited and inspected with the tools solx already has.

| kind | path | type | written by |
|---|---|---|---|
| Session | `/agent/sessions` | `AgentSession` | the harness, several times per turn |
| Memory | `/agent/memories/<scope>` | `AgentMemory` | the model, through a tool |
| Skill | `/agent/skills` | `AgentSkill` | you |

Grouping by owner rather than by kind is deliberate: it makes the package
footprint one query — `solx search --path /agent` — and it lets the gate
reserve a single prefix, so a fourth kind of document cannot be added and
then forgotten.

### Sessions

One session is one conversation is one document. The widget holds no
transcript of its own, so a reload — or another client, or the CLI — sees the
same thread.

```sh
solx list doc --path /agent/sessions
solx get doc /agent/sessions/wandering-heron
```

Ids are an adjective and a noun, because they are read aloud and pasted into
things. 16,120 pairs is not enough on its own: `entity_save_document` is an
upsert keyed on `(path, name)`, so a collision would not fail — it would
silently write over a running transcript. So a name is checked before it is
taken, and the check **fails closed**.

The session type deliberately does not declare `messages` or `calls`. The
full-text index walks only a type's declared fields, so leaving them out keeps
the whole transcript out of FTS while still storing it — which is what makes
writing the document several times per turn affordable. `title` and `summary`
*are* indexed, and carry what the session was about, so `solx search` can find
one by what it was for.

### Memories

**Off unless you name a scope.** Sessions sharing a scope read each other's
notes, so which sessions pool their memory is a decision, not a default.
Without a scope, nothing is recalled and the two memory tools are never
offered — the model is not even told memory exists.

**A memory scope is a trust boundary.** What comes back is model-written text
re-entering a later prompt. It is framed as reference rather than instruction,
and it can never widen what a session may do — but a wrong or poisoned note
will still mislead a later run. Keep unrelated tasks in separate scopes, and
read a scope occasionally: `solx list doc --path /agent/memories/blogs`.

### Skills

A skill explains how to use particular tools, bound to action refs by glob:

```sh
solx save doc /agent/skills/documents \
  --type /packages/solx-agent/AgentSkill --json '{
  "tools": ["/builtin/document/*"],
  "instructions": "Names are unique per path and entity_save_document is an upsert, so a save with an existing name replaces that document. Search first."
}'
```

It loads only when one of its globs matches a tool actually in the catalogue.
A skill therefore never grants reach — it only explains reach already
granted, which is why skills are on by default. There simply are none until
you write one.

## The gate

Layered, and split across two languages.

**Exclusions are enforced in Rust and never enter this package.** Every
catalogue search and every dispatch-time lookup passes `excludeHidden`, so a
hidden action is gone before anything here sees it. Hidden-ness resolves in
`solx-config` as config rules ∪ the row's own `solx:hidden` capability — the
same `ToolPolicy` solx-mcp uses. Nothing here knows the rules, which means it
cannot skip the check or get it subtly wrong in a second implementation.

What is left is what is specific to *this* caller:

1. **Default deny.** An absent or empty grant is an error, not an empty
   catalogue. There is no `"*"` shorthand.
2. **Structural denies, not overridable.** `/builtin/secrets/*`,
   `/builtin/action/*`, `/builtin/env/set_env`, and this package itself.
   `/builtin/action/*` matters more than it looks: the widget drives
   `action/start`/`poll`/`stop` itself.
3. **Command, Webhook and `/builtin/web/*` need an exact name.** A glob never
   reaches a shell or an arbitrary outbound host. (solx-core separately gates
   *where* an outbound request may go, via `allowed_base_urls`. That is a
   different question from whether the model may make one at all.)
4. **The whole `/agent` root is unwritable by the model**, however wide the
   grant. The session document holds the grant and the dispatch table; a skill
   document is instruction injection into every *future* session.

**The gate re-runs at dispatch, not only when the catalogue is built.** That
makes the stored `tools` map and `destructive` flags non-authoritative: even a
model that found some way to edit its own session document gains nothing.

## Approval

A permitted action can still be destructive, and that is not a gate this
package can close on its own.

Destructive calls are not executed. The turn suspends with each pending call's
`call_id`, resolved ref, and **full arguments**; you decide; anything not
approved is denied. **Approve by call, not by tool** — the `call_id` hashes
the tool name *and* its arguments, so an approval resolves to exactly what you
were shown. Approvals never persist across an iteration.

Destructive-ness is resolved host-side, the same union solx-mcp uses, and
**fails closed**: an action that cannot be read back is treated as destructive
rather than waved through.

## Tests

```sh
npm test          # 78 tests against a fake host, plus the built bundle
npm run typecheck
```

`tests/fakeHost.ts` stands up an in-memory action registry, document store and
scripted model behind the same one-method seam a real host uses. Its honest
limit is that it mirrors solx-core's behaviour *as read out of the source* —
if solx-core changes, these keep passing and the package breaks.

`tests/live.test.ts` is what covers that gap, and is skipped unless
`SOLX_TOKEN` is set:

```sh
solx-server --port 8791 &
SOLX_TOKEN=<token> SOLX_MODEL=qwen3:4b npx vitest run tests/live.test.ts
```

It has already earned its place twice — it caught `typeRef` (the document
write wants `type_ref`; the fake had inherited the wrong spelling and agreed
with the bug) and `q: null` (an explicit null fails schema validation, so
every no-query search silently returned nothing).

## What is not done

- **Context documents have no UI.** The harness resolves and reads them; the
  widget cannot yet add one.
- **Sequential tool dispatch only.** A model can emit several calls per turn;
  they run in order.
- **Memories are never forgotten.** Nothing ages them out, dedupes them, or
  reconciles two that contradict each other.
- **Session documents are never cleaned up.** `uninstall.solx` deliberately
  leaves them: a transcript is a record, and so is a memory.
- **Approval cannot tell a human from a script**, in the sense that the widget
  trusts whoever is driving the page.
