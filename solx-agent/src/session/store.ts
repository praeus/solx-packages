import type { AllowEntry } from "../harness";

/**
 * Browser-local preferences.
 *
 * Keys are namespaced `solx-agent:` because a widget shares its host page's
 * origin -- this is the same localStorage solx-web keeps `solx.serverUrl` and
 * `solx:action-runner:*` in, and collisions would be silent.
 *
 * Deliberately thin: the session document is the record. What lives here is
 * what a *browser* should remember (which model you picked, how you like new
 * sessions set up, which session you had open), not what the conversation
 * was. Losing it costs preferences, never history.
 */

const PREFIX = "solx-agent:";

/**
 * The harness injects no base prompt at all -- the model sees only this
 * string, the memory/context/skill blocks, the conversation and the tool
 * definitions. That makes the preamble the entire behavioural contract, which
 * is why it is a real artifact here and editable in the setup panel rather
 * than hidden in the bundle.
 *
 * What belongs here is *orientation* -- what solx is, how a reference is
 * spelled, what an action row means -- because it is true of every session
 * and useless to discover one failed call at a time. What does not belong
 * here is anything tied to a particular family of tools: that goes in an
 * AgentSkill document, which loads only when a matching tool is actually in
 * the catalogue. The split matters because this string is prompt tokens on
 * every iteration of every session, and this harness targets small local
 * models.
 */
export const DEFAULT_PREAMBLE = `You are working inside solx.

solx is a database of actions. Every tool you can call is a row in it, and
every call has the same shape: exec(path, name, params). Documents, actions
and types share one directory-style namespace, so a thing is identified by
its path and its name together and its full reference is the two joined --
for example /builtin/document/search_documents.

Four kinds of thing are stored, each with its own family of builtin actions:
documents (/builtin/document), actions (/builtin/action), types
(/builtin/type) and files (/builtin/file). A type is a JSON Schema; a
document is validated against the type its typeRef names.

An action row says how it runs, in actionType: wasm is a sandboxed
component, webhook a REST call, command a local binary, script a .solx
pipeline, internal a native handler. fnName and binName mean something
different in each. A result comes back wrapped as {action, result, success},
so what you actually asked for is under result.

Two things will mislead you if you assume otherwise:

Parameter names are not spelled uniformly. Entity and search parameters are
camelCase (typeRef, pathPrefix, paramTypeRef); some handlers take snake_case
instead (rel_path, doc_path, stream_id). Schemas are open, so a misspelled
key is silently dropped rather than rejected -- you get a wrong answer, not
an error. Leave an optional parameter out entirely rather than sending null;
an explicit null fails validation and errors the whole call.

Search terms are ANDed and prefix-matched, so searching a whole sentence
matches nothing. Search with two or three distinctive words.

Call a tool when you need one. When you want to hand the turn back to the
person you are talking to -- to answer, or to ask them something -- reply
with text and no tool calls. You can also explain what you are doing in the
same reply as a tool call.

Prefer searching before assuming: if a capability seems to be missing, search
the catalogue or look for more tools before concluding it does not exist. If
a tool fails, read the error and try a different approach rather than
repeating the same call.`;

/**
 * The whole catalogue, so a session is useful without the operator first
 * enumerating paths -- ask for something and the turn's search finds the tool.
 *
 * This is a default, not a hole in the gate. `HARD_DENY` stays unreachable
 * whatever the grant says -- secrets, self-modification, detached spawning,
 * and this package's own actions. What `*` widens is *discovery*: the model
 * can now find a tool it was not handed, which is the whole point of a
 * searchable catalogue. Execution is still gated where it always was --
 * every Command and Webhook row is unconditionally destructive (see
 * `isExecutableType`), so it suspends for a human decision showing the
 * resolved ref and arguments before anything runs.
 *
 * The operator can narrow this at any time, mid-session included.
 */
export const DEFAULT_GRANT: AllowEntry[] = [{ path: "*", actions: null }];

export interface SetupPrefs {
  grant: AllowEntry[];
  memoryScope: string;
  system: string;
  maxIterations: number;
  catalogueCap: number;
  toolSearch: boolean;
}

export const DEFAULT_SETUP: SetupPrefs = {
  grant: DEFAULT_GRANT,
  memoryScope: "",
  system: DEFAULT_PREAMBLE,
  maxIterations: 12,
  catalogueCap: 16,
  toolSearch: true,
};

function read<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(PREFIX + key);
    if (!raw) return fallback;
    return { ...fallback, ...(JSON.parse(raw) as object) } as T;
  } catch {
    // A private window, cleared site data, or storage the browser refuses to
    // hand over. Preferences are not worth failing a render for.
    return fallback;
  }
}

function write(key: string, value: unknown): void {
  try {
    localStorage.setItem(PREFIX + key, JSON.stringify(value));
  } catch {
    /* not worth surfacing: nothing here is unrecoverable */
  }
}

export function loadSetup(): SetupPrefs {
  return read<SetupPrefs>("setup", DEFAULT_SETUP);
}

export function saveSetup(setup: SetupPrefs): void {
  write("setup", setup);
}

export function loadModel(): string {
  try {
    return localStorage.getItem(PREFIX + "model") ?? "";
  } catch {
    return "";
  }
}

export function saveModel(model: string): void {
  try {
    localStorage.setItem(PREFIX + "model", model);
  } catch {
    /* see write() */
  }
}

export function loadActiveSession(): string | null {
  try {
    return localStorage.getItem(PREFIX + "active");
  } catch {
    return null;
  }
}

export function saveActiveSession(id: string | null): void {
  try {
    if (id) localStorage.setItem(PREFIX + "active", id);
    else localStorage.removeItem(PREFIX + "active");
  } catch {
    /* see write() */
  }
}
