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
 */
export const DEFAULT_PREAMBLE = `You are working inside solx, with a small set of tools.

Call a tool when you need one. When you want to hand the turn back to the
person you are talking to -- to answer, or to ask them something -- reply
with text and no tool calls. You can also explain what you are doing in the
same reply as a tool call.

Prefer searching before assuming. If a tool fails, read the error and try a
different approach rather than repeating the same call.`;

/** Read-only documents: enough to be useful, nothing that needs approval. */
export const DEFAULT_GRANT: AllowEntry[] = [
  { path: "/builtin/document", actions: ["search_documents", "entity_get_document"] },
];

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
