/**
 * The built-in tools, backed by no action row.
 *
 * None of them needs approval, because each is confined by construction:
 * `sys__memory_save` writes only under the session's own scope with a name
 * this package generates, `sys__context_read` opens only the frozen context
 * index, and `sys__tool_search` searches only the session's own grant.
 */

import { globMatches, invertMap, resolveCatalogue, searchActionPaths } from "./catalogue";
import {
  clamp,
  readContext,
  recallMemories,
  resolveSkills,
  saveMemory,
  skillBlock,
  type ToolResult,
} from "./knowledge";
import { newMemoryName } from "./session";
import {
  DEFAULT_CATALOGUE_CAP,
  MAX_MEMORY_LIMIT,
  MEMORY_TEXT_CAP,
  SYS_CONTEXT_READ,
  SYS_MEMORY_SAVE,
  SYS_MEMORY_SEARCH,
  SYS_TOOL_SEARCH,
} from "./refs";
import type { Host } from "./host";
import type { Session, ToolDef } from "./types";

export function isSysTool(name: string): boolean {
  return (
    name === SYS_MEMORY_SAVE ||
    name === SYS_MEMORY_SEARCH ||
    name === SYS_CONTEXT_READ ||
    name === SYS_TOOL_SEARCH
  );
}

/** Offered conditionally: a model is never told a capability exists that this
 *  session does not have. */
export function sysToolDefs(session: Session): ToolDef[] {
  const defs: ToolDef[] = [];
  const memory = session.memory;

  if (memory && memory.read) {
    defs.push({
      type: "function",
      function: {
        name: SYS_MEMORY_SEARCH,
        description:
          "Search what you learned in earlier sessions in this memory scope. " +
          "Returns short notes, which may be stale - verify before relying on one.",
        parameters: {
          type: "object",
          required: ["q"],
          properties: {
            q: { type: "string", description: "What to look for." },
            limit: { type: "integer", description: "How many notes to return." },
          },
        },
      },
    });
  }
  if (memory && memory.write) {
    defs.push({
      type: "function",
      function: {
        name: SYS_MEMORY_SAVE,
        description:
          "Write down something you learned that would help a future run of this task: " +
          "facts about how this system is arranged, not the answer to the current question. " +
          "Kept to " +
          MEMORY_TEXT_CAP +
          " characters; longer text is cut.",
        parameters: {
          type: "object",
          required: ["text"],
          properties: {
            text: { type: "string", description: "The note, in one or two sentences." },
            tags: {
              type: "array",
              items: { type: "string" },
              description: "Optional keywords.",
            },
          },
        },
      },
    });
  }
  if ((session.context || []).length > 0) {
    defs.push({
      type: "function",
      function: {
        name: SYS_CONTEXT_READ,
        description:
          "Open one of the reference documents listed for this task, by its ref. " +
          "Only the documents in that list can be opened.",
        parameters: {
          type: "object",
          required: ["ref"],
          properties: { ref: { type: "string", description: "A ref exactly as listed." } },
        },
      },
    });
  }
  if (session.tool_search) {
    defs.push({
      type: "function",
      function: {
        name: SYS_TOOL_SEARCH,
        description:
          "Look for more tools you can use. Searches the same set of actions you were " +
          "granted, so it can reveal tools you were not shown, but never any you are " +
          "not permitted to call. New tools become callable immediately.",
        parameters: {
          type: "object",
          required: ["q"],
          properties: {
            q: {
              type: "string",
              // A phrase works -- `resolveCatalogue` retries term by term when
              // one matches nothing -- but a single keyword ranks better,
              // because then the ranking is the engine's rather than a merge of
              // several searches.
              description: "What you need a tool for. One or two keywords beat a sentence.",
            },
            limit: { type: "integer", description: "How many to add at most." },
          },
        },
      },
    });
  }
  return defs;
}

/**
 * Widens what the model can *see*, never what it may *call*: the search runs
 * against the session's grant, so the gate is untouched. Bounded by whatever
 * is left of the catalogue cap.
 */
/** How many out-of-grant paths a failed search will name. Enough to point at
 *  the right package, short enough not to become a directory listing. */
const OUT_OF_GRANT_PATHS = 5;

/**
 * Paths where a search *would* have matched, had the grant covered them.
 *
 * This is the one place the model is told about actions it cannot call, and
 * it is a deliberate trade. The alternative -- a flat "nothing matched" --
 * reads to a model as "search again", so a model that has heard a tool named
 * anywhere else keeps guessing at how to spell it and spends the turn being
 * refused. Naming the path converts a dead end into something it can hand
 * back to the operator, who could already see these paths anyway.
 *
 * Only *paths* are returned, never the resolved tool names: a name it cannot
 * call is exactly the thing that starts the guessing, and a path is what the
 * operator needs to widen the grant. Grant rules glob-match an action's own
 * path, so a path listed here is one that can be pasted into the grant
 * verbatim -- unlike its parent, which would not match it.
 */
async function outOfGrantPaths(
  host: Host,
  q: string | null,
  grant: Session["grant"],
): Promise<string[]> {
  const facets = await searchActionPaths(host, q || "", 50);
  const covered = (path: string) =>
    (grant || []).some((rule) => globMatches(rule.path || "", path));
  return facets
    .map((f) => f.path)
    .filter((p) => !covered(p))
    .slice(0, OUT_OF_GRANT_PATHS);
}

export async function toolSearch(
  host: Host,
  session: Session,
  args: Record<string, unknown>,
): Promise<ToolResult> {
  const known: Record<string, boolean> = {};
  const names = Object.keys(session.tools || {});
  for (const n of names) known[session.tools[n]] = true;

  const cap = session.catalogue_cap || DEFAULT_CATALOGUE_CAP;
  const budget = Math.max(0, cap - names.length);
  // A full catalogue is not a dead end. The cap is a token budget, not a
  // security boundary (the grant is), so a search may still *replace* a held
  // tool: resolve matches, then evict the least-recently-touched to make room.
  // `want` is the whole cap when there is no budget left, because the search
  // itself is relevance-ranked and a targeted query typically returns only a
  // few hits anyway.
  const want = clamp((args.limit as number) || (budget > 0 ? budget : cap), 1, cap);
  const q = (args.q as string) || null;
  const cat = await resolveCatalogue(host, q, session.grant, want, known);
  if (cat.tools.length === 0) {
    // "Nothing matched" and "it exists but you may not reach it" are the same
    // sentence to a model, and they call for opposite responses: search again
    // with better terms, versus stop and tell the person. Saying only the
    // former is what makes a model start guessing at tool names.
    const elsewhere = await outOfGrantPaths(host, q, session.grant);
    if (elsewhere.length > 0) {
      return {
        outcome: "ok",
        content:
          "Nothing matched inside what this session is allowed to reach, but " +
          "matching tools do exist at: " +
          elsewhere.join(", ") +
          ". This session's grant does not cover them, and you cannot widen it " +
          "yourself -- only the person you are talking to can. Tell them which " +
          "path you need rather than guessing at tool names; a name you were " +
          "not given will always be refused.",
      };
    }
    return {
      outcome: "ok",
      content: "no further tools matched, within what you are permitted to call",
    };
  }

  const addedRefs: string[] = [];
  session.tools_added = session.tools_added || {};
  for (const toolName of Object.keys(cat.map)) {
    session.tools[toolName] = cat.map[toolName];
    session.tools_added[toolName] = session.iteration;
    addedRefs.push(cat.map[toolName]);
  }
  session.tools_defs = (session.tools_defs || []).concat(cat.tools);
  session.tools_dropped = (session.tools_dropped || 0) + cat.dropped;

  // Evict when the additions pushed past the cap, so a full catalogue still
  // lets a search swap in a more relevant tool. Sys tools are never in
  // `session.tools`, so they are untouched.
  //
  // Least-recently-*touched* wins, where touched means added or called.
  // Insertion order was the obvious rule and it was wrong: once the turn's
  // initial catalogue had been evicted, consecutive searches began
  // cannibalising each other, because the previous search's results were then
  // the oldest entries. A real session did exactly that -- `q:"file"` fetched
  // `file-put`, `q:"save"` evicted it, and the run died refused when it went
  // to write its source. Scoring by last touch fixes it without a special
  // case: a tool added by this very search carries the current iteration, so
  // it sorts last and cannot be evicted to make room for itself.
  const evicted: string[] = [];
  const held = Object.keys(session.tools);
  if (held.length > cap) {
    const touched: Record<string, number> = {};
    held.forEach((tn, i) => {
      // The fractional index keeps ties in insertion order, and stands in for
      // the iteration on sessions written before `tools_added` existed.
      touched[tn] = (session.tools_added || {})[tn] ?? i / held.length;
    });
    for (const call of session.calls || []) {
      if (touched[call.name] !== undefined) {
        touched[call.name] = Math.max(touched[call.name], call.iteration || 0);
      }
    }

    const over = held.length - cap;
    const victims = held.slice().sort((a, b) => touched[a] - touched[b]).slice(0, over);
    for (const tn of victims) {
      delete session.tools[tn];
      if (session.tools_added) delete session.tools_added[tn];
      evicted.push(tn);
    }
    if (evicted.length > 0) {
      const evictSet = new Set(evicted);
      session.tools_defs = (session.tools_defs || []).filter(
        (d) => !evictSet.has(d.function.name),
      );
    }
  }

  let text =
    "Now available to you:\n" +
    cat.tools.map((t) => "- " + t.function.name + " - " + t.function.description).join("\n");
  if (evicted.length > 0) {
    text +=
      "\n\nTo make room, these were dropped from the catalogue (search again to " +
      "bring one back): " +
      evicted.join(", ");
  }

  // Skills covering the new tools ride back in the tool result, so nothing
  // has to splice a system turn into the middle of a transcript.
  session.skills_seen = session.skills_seen || {};
  const skills = await resolveSkills(host, session.skills, addedRefs, session.skills_seen);
  if (skills.length > 0) {
    for (const s of skills) session.skills_seen[s.ref] = true;
    text += "\n\n" + skillBlock(skills, invertMap(cat.map));
  }
  return { outcome: "ok", content: text };
}

export async function runSysTool(
  host: Host,
  session: Session,
  name: string,
  args: Record<string, unknown>,
): Promise<ToolResult> {
  const a = args || {};
  if (name === SYS_MEMORY_SAVE) {
    return saveMemory(host, session, a.text, a.tags, newMemoryName);
  }
  if (name === SYS_CONTEXT_READ) return readContext(host, session, String(a.ref || ""));
  if (name === SYS_TOOL_SEARCH) return toolSearch(host, session, a);

  if (name === SYS_MEMORY_SEARCH) {
    if (!session.memory || !session.memory.read) {
      return { outcome: "refused", content: "memory is not enabled for this session" };
    }
    const limit = clamp((a.limit as number) || session.memory.limit, 1, MAX_MEMORY_LIMIT);
    const found = await recallMemories(
      host,
      { scope: session.memory.scope, read: true, limit },
      (a.q as string) || null,
    );
    if (found.length === 0) return { outcome: "ok", content: "no memories matched" };
    return { outcome: "ok", content: found.map((m) => "- " + m.text).join("\n") };
  }
  return { outcome: "refused", content: "unknown built-in tool " + name };
}
