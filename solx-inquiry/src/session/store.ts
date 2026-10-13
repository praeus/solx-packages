/**
 * Browser-local preferences.
 *
 * Namespaced `solx-inquiry:` because a widget shares its host page's origin --
 * the same localStorage solx-web keeps `solx.serverUrl` in. Deliberately thin,
 * matching solx-agent: the inquiry document is the record, and what lives here
 * is only what a *browser* should remember (which model, how to set up a new
 * inquiry, which one was open). Losing it costs preferences, never history.
 */

import { DEFAULT_MAX_DOCS, DEFAULT_MAX_PLAN_STEPS, DEFAULT_DOC_BUDGET } from "../harness";

const PREFIX = "solx-inquiry:";

export interface SetupPrefs {
  maxDocs: number;
  maxPlanSteps: number;
  docBudget: number;
  includeActions: boolean;
  replan: boolean;
  chatTimeoutSecs: number | null;
}

export const DEFAULT_SETUP: SetupPrefs = {
  maxDocs: DEFAULT_MAX_DOCS,
  maxPlanSteps: DEFAULT_MAX_PLAN_STEPS,
  docBudget: DEFAULT_DOC_BUDGET,
  includeActions: false,
  replan: true,
  chatTimeoutSecs: null,
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

export function loadActive(): string | null {
  try {
    return localStorage.getItem(PREFIX + "active");
  } catch {
    return null;
  }
}

export function saveActive(id: string | null): void {
  try {
    if (id) localStorage.setItem(PREFIX + "active", id);
    else localStorage.removeItem(PREFIX + "active");
  } catch {
    /* see write() */
  }
}
