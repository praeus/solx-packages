/**
 * The gate: what this session may call, decided fresh at every dispatch.
 *
 * Layered, and split across two languages on purpose. **Exclusions are
 * enforced in Rust and never reach this file** -- `search_actions` and
 * `entity_get_action` are called with `excludeHidden: true`, so a hidden
 * action is already gone before anything here sees it. Hidden-ness resolves
 * in `solx-config` as config rules union the row's own `solx:hidden`
 * capability, the same `ToolPolicy` solx-mcp uses. Nothing here knows the
 * rules, which means it cannot skip the check or get it subtly wrong in a
 * second implementation.
 *
 * What is left here is what is dangerous specifically because *this* is the
 * caller. The consequence worth keeping: a bug in this file can only ever be
 * too strict, never too permissive about what the operator hid.
 *
 * Moving the harness into the browser did not move any of that. The parts
 * that matter are still resolved host-side; this was always the
 * caller-specific layer on top.
 */

import { DOC_WRITERS, HARD_DENY } from "./refs";
import type { AllowEntry, Session } from "./types";

/** Mirrors `solx_surface::path::normalize_path`: leading slash, no trailing
 *  slash, root stays "/". */
export function normalizePath(p: string | null | undefined): string {
  let s = String(p ?? "/").replace(/\\/g, "/");
  if (s.charAt(0) !== "/") s = "/" + s;
  while (s.length > 1 && s.charAt(s.length - 1) === "/") s = s.slice(0, -1);
  return s;
}

/**
 * Case-insensitive on purpose. SQLite compares TEXT with a binary collation
 * for UNIQUE but matches LIKE case-insensitively for ASCII, so a
 * differently-cased path is a different document this package would never
 * read back -- but denying it costs nothing and removes the need to reason
 * about that at all.
 */
export function isAtOrUnder(path: string, root: string): boolean {
  const p = normalizePath(path).toLowerCase();
  const r = normalizePath(root).toLowerCase();
  return p === r || p.indexOf(r + "/") === 0;
}

/** A path segment, by the rules solx-surface enforces. */
export function validSegment(s: unknown): boolean {
  if (typeof s !== "string" || s === "" || s === "." || s === "..") return false;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x20 || c === 0x7f) return false;
    const ch = s.charAt(i);
    if (ch === "/" || ch === "\\" || ch === ":") return false;
  }
  return true;
}

export function refOf(path: string, name: string): string {
  const p = normalizePath(path);
  return (p === "/" ? "" : p) + "/" + name;
}

export function splitRef(ref: string): { path: string; name: string } {
  const slash = String(ref).lastIndexOf("/");
  return { path: String(ref).slice(0, slash) || "/", name: String(ref).slice(slash + 1) };
}

/**
 * Mirrors `solx_config::glob_matches`: `*` matches any run of characters
 * including `/`, `?` matches exactly one. Only ever used for the grant and
 * for skill bindings -- exclusion rules are matched host-side, in Rust.
 */
export function globMatches(pattern: string, text: string): boolean {
  let pi = 0;
  let ti = 0;
  let starP = -1;
  let starT = 0;
  while (ti < text.length) {
    if (pi < pattern.length && (pattern[pi] === "?" || pattern[pi] === text[ti])) {
      pi++;
      ti++;
    } else if (pi < pattern.length && pattern[pi] === "*") {
      starP = pi;
      starT = ti;
      pi++;
    } else if (starP !== -1) {
      pi = starP + 1;
      starT++;
      ti = starT;
    } else {
      return false;
    }
  }
  while (pi < pattern.length && pattern[pi] === "*") pi++;
  return pi === pattern.length;
}

export function ruleMatches(rule: AllowEntry, path: string, name: string): boolean {
  if (!globMatches(rule.path || "", path)) return false;
  if (Array.isArray(rule.actions) && rule.actions.length > 0) {
    return rule.actions.indexOf(name) !== -1;
  }
  return true;
}

export function hardDenied(path: string, name: string): boolean {
  const full = refOf(path, name);
  for (const pattern of HARD_DENY) {
    if (globMatches(pattern, full) || globMatches(pattern, path)) return true;
  }
  return false;
}

/**
 * Rows a glob must never reach -- an exact name in `grant[].actions` is
 * required instead.
 *
 * Command and Webhook are here because a glob should never reach a shell or
 * an arbitrary outbound host. `/builtin/web/*` is here for the same reason
 * and is the newer entry: solx-core now gates *where* an outbound request may
 * go (`allowed_base_urls`), but that is a different question from whether the
 * model may make one at all. An operator's allowlist may legitimately contain
 * hosts the agent should not be free to reach on its own.
 */
export function needsExactName(action: { actionType?: string; action_type?: string; path?: string; name?: string }): boolean {
  const ty = action.actionType || action.action_type;
  if (ty === "command" || ty === "webhook") return true;
  const path = normalizePath(action.path);
  return isAtOrUnder(path, "/builtin/web");
}

/** Kept as its own predicate: destructive-ness unions this with the row's
 *  `solx:destructive` capability, independent of the exact-name rule. */
export function isExecutableType(actionType: string | undefined): boolean {
  return actionType === "command" || actionType === "webhook";
}

export function allowedByExactName(allow: AllowEntry[], path: string, name: string): boolean {
  for (const rule of allow) {
    if (rule.path !== path) continue;
    if (Array.isArray(rule.actions) && rule.actions.indexOf(name) !== -1) return true;
  }
  return false;
}

/** The gate. No `exclude` parameter: anything reaching here already survived
 *  the host-side filter. */
export function permitted(
  action: { path: string; name: string; actionType?: string; action_type?: string },
  allow: AllowEntry[],
): boolean {
  const { path, name } = action;
  if (hardDenied(path, name)) return false;
  if (needsExactName(action)) return allowedByExactName(allow, path, name);
  for (const rule of allow) {
    if (ruleMatches(rule, path, name)) return true;
  }
  return false;
}

/**
 * Default-deny, enforced where the grant enters rather than where it is used,
 * so an empty list can never be mistaken for an unset one.
 */
export function normalizeAllow(allow: AllowEntry[] | null | undefined): AllowEntry[] {
  if (!Array.isArray(allow) || allow.length === 0) {
    throw new Error(
      "allow is required and must be non-empty: this is default-deny, and " +
        "there is no wildcard. List the path prefixes the model may reach.",
    );
  }
  return allow.map((r) => {
    if (!r || typeof r.path !== "string" || r.path === "") {
      throw new Error("each allow rule needs a non-empty path");
    }
    return { path: r.path, actions: Array.isArray(r.actions) ? r.actions : null };
  });
}

/**
 * This package's own documents are off limits to the model, whatever the
 * grant says. Rewriting the session document is a full escalation: it holds
 * the grant and the `tools` dispatch map. Writing a skill document is worse
 * in a quieter way -- it is instruction injection into every *future*
 * session.
 *
 * The whole `/agent` root is reserved, which covers sessions, memories,
 * skills, and anything added later.
 */
export function reservedDocRoots(session: Pick<Session, "skills">): string[] {
  const roots = ["/agent"];
  const configured = session?.skills?.path;
  if (configured && !isAtOrUnder(configured, "/agent")) roots.push(configured);
  return roots;
}

export function reservedDocWrite(
  session: Pick<Session, "skills">,
  ref: string,
  args: Record<string, unknown> | undefined,
): string | null {
  const key = DOC_WRITERS[ref];
  if (!key) return null;
  const target = (args && (args[key] as string)) || "/";
  for (const root of reservedDocRoots(session)) {
    if (isAtOrUnder(target, root)) {
      return (
        "refused: " +
        normalizePath(target) +
        " is an agent-owned path. Sessions, memories and skills are not " +
        "writable through document actions."
      );
    }
  }
  return null;
}
