/**
 * The built-in tools, backed by no action row.
 *
 * None of them needs approval, because each is confined by construction:
 * `sys__memory_save` writes only under the session's own scope with a name
 * this package generates, `sys__context_read` opens only the frozen context
 * index, and `sys__tool_search` searches only the session's own grant.
 */

import { invertMap, resolveCatalogue } from "./catalogue";
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
            q: { type: "string", description: "What you need a tool for." },
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
export async function toolSearch(
  host: Host,
  session: Session,
  args: Record<string, unknown>,
): Promise<ToolResult> {
  const known: Record<string, boolean> = {};
  const names = Object.keys(session.tools || {});
  for (const n of names) known[session.tools[n]] = true;

  const budget = Math.max(0, (session.catalogue_cap || DEFAULT_CATALOGUE_CAP) - names.length);
  if (budget === 0) {
    return {
      outcome: "ok",
      content: "no room for more tools; you already hold the maximum for this session",
    };
  }
  const want = clamp((args.limit as number) || budget, 1, budget);
  const q = (args.q as string) || null;
  const cat = await resolveCatalogue(host, q, session.grant, want, known);
  if (cat.tools.length === 0) {
    return {
      outcome: "ok",
      content: "no further tools matched, within what you are permitted to call",
    };
  }

  const addedRefs: string[] = [];
  for (const toolName of Object.keys(cat.map)) {
    session.tools[toolName] = cat.map[toolName];
    addedRefs.push(cat.map[toolName]);
  }
  session.tools_defs = (session.tools_defs || []).concat(cat.tools);
  session.tools_dropped = (session.tools_dropped || 0) + cat.dropped;

  let text =
    "Now available to you:\n" +
    cat.tools.map((t) => "- " + t.function.name + " - " + t.function.description).join("\n");

  // Skills covering the new tools ride back in the tool result, so nothing
  // has to splice a system turn into the middle of a transcript.
  session.skills_seen = session.skills_seen || {};
  const skills = await resolveSkills(host, session.skills, q, addedRefs, session.skills_seen);
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
