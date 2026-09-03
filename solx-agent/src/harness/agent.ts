/**
 * The conversation: creating a session, adding a turn to one, and widening
 * what it may reach.
 *
 * ## Why there is one `send` and not a `start` plus a `say`
 *
 * The package this was ported from had a goal-shaped session: `start` took a
 * `goal`, and that first turn was special in three ways -- it titled the
 * document, it selected the tool catalogue, and it was the only turn a
 * session got until `say` was bolted on to make conversation possible. A
 * conversation has no such turn. `send` is the only entry point, and turn one
 * differs from turn five only in that a document has to be created first.
 *
 * ## Why the catalogue re-resolves and the grant does not
 *
 * The sharp edge in the old model was that the catalogue was frozen at
 * `start` and selected by the first message. Ask for a summary of some
 * documents, then ask to email it, and the second turn could never reach the
 * mail actions -- so the conversation `say` existed to preserve had to be
 * thrown away and restarted.
 *
 * So one field became two:
 *
 * - **`grant`** is the security boundary. Nothing the model does widens it.
 *   `sys__tool_search` re-searches within it, exactly as before.
 * - **`tools`** is the selection shown to the model, re-resolved from *this
 *   turn's* message at the top of every turn.
 *
 * The invariant the gate exists for survives verbatim -- no amount of talking
 * widens what the model may call -- because talking now re-selects from the
 * grant rather than extending it.
 *
 * Widening the grant is a separate, explicit, human action (`widenGrant`),
 * and it writes a system turn, for the same reason newly indexed context
 * documents are announced: the transcript must show when reach changed, and
 * the model has to be told what it can now see.
 */

import { invertMap, resolveCatalogue } from "./catalogue";
import { normalizeAllow } from "./gate";
import {
  contextBlock,
  memoryBlock,
  normalizeMemory,
  normalizeSkills,
  recallMemories,
  resolveContext,
  resolveSkills,
  skillBlock,
} from "./knowledge";
import { newSessionId, saveSession } from "./session";
import { sysToolDefs } from "./sysTools";
import { summarize } from "./turn";
import {
  DEFAULT_CATALOGUE_CAP,
  DEFAULT_CONTEXT_CAP,
  DEFAULT_MAX_ITERATIONS,
} from "./refs";
import type { Host } from "./host";
import type {
  AllowEntry,
  ContextEntry,
  MemorySpec,
  Session,
  SkillSpec,
  StepResult,
  ToolsPreview,
} from "./types";

export interface SendOptions {
  model: string;
  grant: AllowEntry[];
  system?: string | null;
  catalogue_cap?: number | null;
  tool_search?: boolean | null;
  max_iterations?: number | null;
  memory?: Partial<MemorySpec> | null;
  skills?: Partial<SkillSpec> | null;
  context?: ContextEntry[] | null;
  chat_timeout_secs?: number | null;
}

function titleFrom(message: string): string {
  return message.length > 80 ? message.slice(0, 79) + "…" : message;
}

/**
 * Select this turn's catalogue and hang it on the session.
 *
 * Shared by session creation and every later turn, which is the whole point:
 * there is no code path where turn one resolves differently from turn five.
 */
async function resolveTurnCatalogue(
  host: Host,
  session: Session,
  query: string,
): Promise<{ toolCount: number; skillRefs: string[] }> {
  let cat = await resolveCatalogue(host, query, session.grant, session.catalogue_cap, null);

  // A message that matches nothing must not leave the model empty-handed --
  // "list the files" against a documents-only grant would otherwise strip it
  // of the tools it was already using and it could not even explain why. Fall
  // back to whatever the grant permits, unfiltered by this turn's wording.
  //
  // Deliberately a re-resolve rather than keeping the previous turn's
  // catalogue: re-resolving is always within the *current* grant, so it
  // cannot show tools an operator has just revoked.
  if (cat.tools.length === 0) {
    cat = await resolveCatalogue(host, null, session.grant, session.catalogue_cap, null);
  }
  session.tools = cat.map;
  session.tools_dropped = cat.dropped;

  const refs = Object.keys(cat.map).map((n) => cat.map[n]);
  session.skills_seen = session.skills_seen || {};
  const found = await resolveSkills(host, session.skills, query, refs, session.skills_seen);
  if (found.length > 0) {
    for (const s of found) session.skills_seen[s.ref] = true;
    session.messages.push({ role: "system", content: skillBlock(found, invertMap(cat.map)) });
  }

  // Built-in definitions depend on what was settled above, so they are
  // resolved after the catalogue and appended to it.
  session.tools_defs = cat.tools.concat(sysToolDefs(session));
  return { toolCount: cat.tools.length, skillRefs: found.map((s) => s.ref) };
}

/** Create a session around its first message. Nothing runs; `step` does that. */
export async function createSession(
  host: Host,
  message: string,
  opts: SendOptions,
): Promise<Session> {
  if (!opts.model) throw new Error("a model is required");
  if (!message) throw new Error("a message is required");
  const grant = normalizeAllow(opts.grant);
  const memory = normalizeMemory(opts.memory);
  const skills = normalizeSkills(opts.skills);

  const id = await newSessionId(host);
  const session: Session = {
    id,
    model: opts.model,
    title: titleFrom(message),
    status: "running",
    iteration: 0,
    turn_iteration: 0,
    max_iterations: opts.max_iterations || DEFAULT_MAX_ITERATIONS,
    chat_timeout_secs: opts.chat_timeout_secs || null,
    grant,
    tools: {},
    tools_defs: [],
    tools_dropped: 0,
    catalogue_cap: opts.catalogue_cap || DEFAULT_CATALOGUE_CAP,
    tool_search: opts.tool_search !== false,
    memory,
    memories_written: 0,
    context: await resolveContext(host, opts.context, DEFAULT_CONTEXT_CAP),
    skills,
    skills_seen: {},
    messages: [],
    calls: [],
    pending: [],
    consecutive_failures: 0,
    answer: null,
  };

  // The seed transcript. Everything injected here is framed as reference
  // material: memories are model-written text re-entering a prompt, and
  // context documents are whatever the operator pointed at.
  if (opts.system) session.messages.push({ role: "system", content: opts.system });
  const memories = memory ? await recallMemories(host, memory, memory.query || message) : [];
  if (memories.length > 0) {
    session.messages.push({ role: "system", content: memoryBlock(memories) });
  }
  if (session.context.length > 0) {
    session.messages.push({ role: "system", content: contextBlock(session.context) });
  }

  const { toolCount } = await resolveTurnCatalogue(host, session, message);
  if (toolCount === 0) {
    // Not about the wording: resolveTurnCatalogue already retried with no
    // query, so an empty catalogue here means the grant itself reaches
    // nothing.
    throw new Error(
      "the grant resolves to no tools at all; the model would have nothing " +
        "to call. Widen the allowed paths.",
    );
  }

  session.messages.push({ role: "user", content: message });
  await saveSession(host, id, session);
  return session;
}

/**
 * Append a user turn and make the session runnable again.
 *
 * Refuses outright while a session is `awaiting_approval`: an outstanding
 * approval is a decision the operator already owes, and taking a new
 * instruction on top of it would resolve those calls by silently denying
 * them -- not something to infer from an unrelated message.
 */
export async function addTurn(
  host: Host,
  session: Session,
  message: string,
  opts: { max_iterations?: number | null; context?: ContextEntry[] | null } = {},
): Promise<StepResult> {
  if (!message) throw new Error("a message is required");
  if (session.status === "awaiting_approval") {
    throw new Error(
      "session " + session.id + " is awaiting approval; resolve that before saying more",
    );
  }

  if (opts.context) {
    // Operator-supplied, so this does not violate the rule the gate exists
    // for: the model still cannot widen its own reach.
    const existing: Record<string, boolean> = {};
    for (const e of session.context) existing[e.ref] = true;
    const resolved = await resolveContext(host, opts.context, DEFAULT_CONTEXT_CAP);
    const added = [];
    for (const entry of resolved) {
      if (existing[entry.ref]) continue;
      if (session.context.length >= DEFAULT_CONTEXT_CAP) break;
      existing[entry.ref] = true;
      session.context.push(entry);
      added.push(entry);
    }
    // The index must never hold entries the model was never told about:
    // sys__context_read refuses a ref that is not in it, and the model cannot
    // ask for one it has not seen named.
    if (added.length > 0) {
      session.messages.push({ role: "system", content: contextBlock(added) });
    }
  }

  session.messages.push({ role: "user", content: message });

  // Re-select the catalogue for what was *just* asked. This is the fix for
  // the frozen-catalogue problem; see this module's header.
  await resolveTurnCatalogue(host, session, message);

  // A new instruction reopens a session that had settled. The answer is stale
  // the moment there is more to do, and a human intervening is exactly what
  // breaks a consecutive-failure streak -- "try it another way" is the
  // recovery path out of `blocked`.
  session.answer = null;
  session.consecutive_failures = 0;
  session.turn_iteration = 0;
  if (opts.max_iterations) session.max_iterations = opts.max_iterations;
  session.status = "running";

  await saveSession(host, session.id, session);
  return summarize(session, "running");
}

/**
 * Widen (or narrow) what a running session may reach.
 *
 * Only a human does this, from the setup panel. It is safe precisely because
 * the operator is the trust root -- the rule the gate enforces is that *the
 * model* cannot widen its own reach, not that reach is immutable. Freezing it
 * against the operator too was the thing that made a wandering conversation
 * impossible.
 *
 * The change is announced as a system turn so the transcript records when
 * reach changed, and the next turn's catalogue resolution picks it up.
 */
export async function widenGrant(
  host: Host,
  session: Session,
  grant: AllowEntry[],
): Promise<Session> {
  const next = normalizeAllow(grant);
  const before = new Set(session.grant.map((r) => r.path));
  const added = next.map((r) => r.path).filter((p) => !before.has(p));

  session.grant = next;
  if (added.length > 0) {
    session.messages.push({
      role: "system",
      content:
        "The operator changed what you are allowed to reach. Now included: " +
        added.join(", ") +
        ". Tools for it appear when you next take a turn, or you can look now " +
        "with a tool search.",
    });
  }
  await saveSession(host, session.id, session);
  return session;
}

/**
 * Resolve exactly what a turn would get, without creating a session or
 * spending a model call. The only way to exercise the gate for free, which is
 * what makes the setup panel's catalogue preview worth having.
 */
export async function previewTools(
  host: Host,
  grant: AllowEntry[],
  query: string | null,
  cap: number,
): Promise<ToolsPreview> {
  const cat = await resolveCatalogue(
    host,
    query,
    normalizeAllow(grant),
    cap || DEFAULT_CATALOGUE_CAP,
    null,
  );
  return { tools: cat.tools, refs: cat.map, dropped: cat.dropped };
}
