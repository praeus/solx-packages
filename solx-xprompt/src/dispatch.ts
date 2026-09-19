/**
 * One instruction in, one multi_inquire turn out.
 *
 * multi_inquire's own intent phase decides whether an instruction can be
 * answered directly or needs up to three inquiries fanned out (see
 * solx-inquiry's intent.rs) — one LLM call, schema-constrained, with real
 * context (skills, memories, session history). That is the "chat vs.
 * research" decision, made once and made well; there is no keyword-heuristic
 * routing layer here to duplicate or contradict it.
 *
 * Runs detached via `client.invocations.start` (through this widget's own
 * `startTrackedCall`, see `src/console/`) rather than a blocking `host.call`.
 * That is what gives a turn an `invocation_id` the Composer's Stop button can
 * actually cancel via `client.invocations.stop`, and what lets the Console
 * tab show this turn's recall/intent/inquiry phases as they happen.
 *
 * Action hits multi_inquire returns render with a "Run" button that calls
 * actions.exec with the hit's parameters — the same plumbing the widget's
 * own calls already use, so a user can go from "what actions can do X?" to
 * "run X" without leaving the chat.
 */

import { isTerminalStatus } from "../../solx-widgets/src/shared/widgetClient";
import type { WidgetClient, WidgetInvocation } from "../../solx-widgets/src/shared/widgetClient";
import { compact } from "../../solx-widgets/src/wrap/host";
import type { Host } from "../../solx-widgets/src/wrap/host";
import { startTrackedCall } from "./console";
import { normalizeResult } from "./result";
import { INQUIRY_PATH, MULTI_INQUIRE_FN } from "./refs";
import type {
  InquireHit,
  MultiInquireResult,
  MultiInquireScript,
  ScriptStep,
} from "./types";

/** How long one long-poll waits for the invocation to change before this loop asks again. */
const POLL_WAIT_SECS = 25;

export interface DispatchHandle {
  invocationId: string;
  /** Resolves once the invocation reaches a terminal state; rejects on failure or cancellation. */
  result: Promise<MultiInquireResult>;
}

/**
 * Start one turn, tracked under `logId` so it shows up in the widget's
 * merged Console tab. Returns as soon as the call is accepted — the caller
 * awaits `.result` separately so it can render the invocation id (for Stop)
 * before the turn finishes.
 */
export type ForceKind = "actions" | "documents";

export interface DispatchOptions {
  /**
   * Skip the intent phase and synthesise a single inquiry of the given kind.
   * `actions` is the one a user reaches for when a model won't pick the
   * action path on its own — the inquiry call that follows still gets the
   * scripts schema, so a returned `scripts[]` is what surfaces in the
   * transcript. `documents` is the symmetric path for asking the model to
   * read rather than run.
   */
  forceKind?: ForceKind;
}

export async function dispatch(
  client: WidgetClient,
  host: Host,
  logId: string,
  instruction: string,
  model: string,
  session: string,
  options: DispatchOptions = {},
): Promise<DispatchHandle> {
  const inv = await startTrackedCall(
    client,
    host,
    logId,
    INQUIRY_PATH,
    MULTI_INQUIRE_FN,
    compact({
      instruction,
      model,
      session,
      max_inquiries: 3,
      max_terms: 5,
      max_results: 10,
      force_kind: options.forceKind,
    }),
    "turn",
  );
  return { invocationId: inv.invocation_id, result: waitForResult(client, inv) };
}

/**
 * Long-poll one invocation to a terminal state and hand back its result.
 *
 * Branches on **status**, not on the presence of `error`. The invocation store
 * writes `error` as `""` when there was none and the read-back turns `""` into
 * absent, so `cancelled`, `timeout` and `interrupted` can all finish with no
 * message at all — and a guard that only checked `error` let those fall
 * through and return `result`, which for a run that never produced one is
 * `null`, cast to `MultiInquireResult`. That reached the transcript as an
 * answer turn and took the render down with it.
 */
async function waitForResult(client: WidgetClient, inv: WidgetInvocation): Promise<MultiInquireResult> {
  let current = inv;
  while (!isTerminalStatus(current.status)) {
    const polled = await client.invocations.poll(current.invocation_id, POLL_WAIT_SECS);
    if (polled) current = polled;
  }
  const status = typeof current.status === "string" ? current.status : "unknown";
  if (status !== "ok") {
    throw new Error(current.error || `the turn ended: ${status}`);
  }
  const result = normalizeResult(current.result);
  if (!result) {
    // `ok` with nothing usable under it should not be silently rendered as an
    // empty answer - that reads as "the model had nothing to say".
    throw new Error("the turn reported success but returned no usable result");
  }
  return result;
}

/**
 * Pull the most plausible parameter object out of an action hit.
 *
 * multi_inquire does not return a hit's full param schema inline — it's
 * fetched separately and stuffed under `hit.details.paramSchema`. We don't
 * have it here at the dispatch layer (this module is pure plumbing), so the
 * caller builds the params object from the schema at the call site. This
 * helper decides which action hit is runnable: any source==="action" hit
 * that has either a `details.category` (package actions carry one) or a
 * `details.paramSchema` (built-in actions carry a schema instead of a
 * category). Without either, there's nothing to render or call.
 */
export function isRunnableActionHit(hit: InquireHit): boolean {
  return hit.source === "action" && (!!hit.details?.category || !!hit.details?.paramSchema);
}

// ─── Auto-run machinery ─────────────────────────────────────────────────────
//
// What `multi_inquire` itself does *not* do:
//   • substitute `$name` / `$name.field` capture references in a step's
//     `params` (script.rs validates that references resolve, but it leaves
//     the actual substitution to the caller — see `script.rs:7`);
//   • execute the steps it hands back;
//   • decide what's destructive at run time (it surfaces a `destructive[]`
//     list, the rest is policy).
//
// Everything in this section is the widget's policy layer on top of those
// returned plans and hits: how to interpret the destructive flag, how to
// substitute captures, and how to drive a sequence of `host.try` calls
// without blocking the UI between them. None of it needs new server surface —
// `host.try` is already what the manual "Run" button uses.

// ── capture substitution ────────────────────────────────────────────────────

/**
 * A string is a capture reference iff it matches exactly
 * `^\$[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z0-9_]+)*$`. Spliced references like
 * `"see $hits for details"` are NOT references — a `params` value is more
 * likely to be prose that happens to contain `$` than a deliberate splice
 * (matches solx-inquiry's `script::capture_reference`).
 */
export function captureReference(s: string): { name: string; path: string[] } | null {
  if (!s.startsWith("$") || s.length < 2) return null;
  const rest = s.slice(1);
  const parts = rest.split(".");
  if (parts.length === 0) return null;
  const name = parts[0];
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) return null;
  for (const p of parts.slice(1)) {
    if (!/^[A-Za-z0-9_]+$/.test(p)) return null;
  }
  return { name, path: parts.slice(1) };
}

/**
 * Walk every string leaf in `params` and replace exact-reference strings
 * (`$name`, `$name.field`) with the matching entry in `captures`. Non-string
 * leaves, strings that aren't exact references, and references to unknown
 * captures are left untouched — `script.rs` already validated that every
 * reference resolves, so leaving an unknown one as `$name` makes a
 * down-stream error obvious instead of silently swallowing it.
 */
export function substituteCaptures(
  params: Record<string, unknown>,
  captures: Record<string, unknown>,
): Record<string, unknown> {
  return walk(params) as Record<string, unknown>;

  function walk(value: unknown): unknown {
    if (typeof value === "string") {
      const ref = captureReference(value);
      if (!ref) return value;
      const root = captures[ref.name];
      if (root === undefined) return value; // unknown — leave as $name
      let cur: unknown = root;
      for (const seg of ref.path) {
        // `hasOwnProperty`, not `in`: `in` walks the prototype chain, so
        // `$doc.constructor` would resolve to a function that then serialises
        // to nothing. Only a capture's own data is addressable.
        if (cur && typeof cur === "object" && Object.prototype.hasOwnProperty.call(cur, seg)) {
          cur = (cur as Record<string, unknown>)[seg];
        } else {
          return value; // path missing — leave as $name.field
        }
      }
      return cur;
    }
    if (Array.isArray(value)) return value.map(walk);
    if (value && typeof value === "object") {
      const out: Record<string, unknown> = {};
      for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
        out[k] = walk(v);
      }
      return out;
    }
    return value;
  }
}

// ── destructive gating ──────────────────────────────────────────────────────

/** The capability tag solx-core enforces. Mirrors solx-inquiry's `DESTRUCTIVE_CAPABILITY`. */
export const DESTRUCTIVE_CAPABILITY = "solx:destructive";

/**
 * `actionType`s solx-core treats as destructive unconditionally — shell and
 * outbound HTTP — whatever their capabilities say.
 */
const EXECUTABLE_ACTION_TYPES = new Set(["command", "webhook"]);

/**
 * Is this action hit destructive?
 *
 * Deliberately the same two checks as solx-inquiry's `search::is_destructive`,
 * against the same two fields: the `solx:destructive` capability tag, and an
 * **`actionType`** of `command` or `webhook`. Reading `category` here instead
 * was a silent hole — `category` is a descriptive grouping ("ops", "llm"), so
 * the executable half never fired and a Command action without the capability
 * tag read as safe.
 *
 * Neither side's check is complete, and that is worth knowing before trusting
 * it: `solx-config`'s `ToolPolicy::is_destructive` also consults a configured
 * `tool_destructive` list, which no action exposes, so it is invisible from
 * out here just as it is from inside a guest. The built-ins are the live
 * example — `entity-delete-action` is `internal` with empty capabilities and
 * trips neither check available to us. Treat a `false` as "nothing visible
 * says this is destructive", not as "this is safe".
 */
export function isDestructiveHit(hit: InquireHit): boolean {
  if (hit.source !== "action") return false;
  const caps = hit.details?.capabilities ?? [];
  // Exact match, not `includes`: a capability merely *containing* the string
  // (`"not-solx:destructive"`) is a different tag, and solx-inquiry compares
  // the whole string.
  if (caps.some((c) => c === DESTRUCTIVE_CAPABILITY)) return true;
  return EXECUTABLE_ACTION_TYPES.has(hit.details?.actionType ?? "");
}

/** Are any of a script's `destructive[]` refs not yet approved? */
export function unapprovedDestructiveRefs(
  script: MultiInquireScript,
  approved: ReadonlySet<string>,
): string[] {
  return script.destructive.filter((r) => !approved.has(r));
}

// ── run dispatcher ──────────────────────────────────────────────────────────
//
// Only structured plans are executable. A `scripts[]` entry is something the
// model chose — which actions, in which order, with which parameters — and
// `script.rs` then checked every step against what that inquiry's search
// actually surfaced. A `hits[]` entry is only a search result: it matched the
// query terms, nothing selected it as a thing to do, and nothing supplied
// parameters for it. There used to be a `runHitList` that executed those with
// `{}`, which is the moral equivalent of running everything `apropos` printed;
// it was removed rather than made safe, because an action whose parameters are
// all optional can read `{}` as "apply to everything" and no schema
// distinguishes that from a harmless list call. Per-hit "Run" still exists for
// one-offs, with parameters the user types.

/**
 * A single step's outcome. Surfaced through `onStep` so the widget can
 * append a run-turn per step to the transcript.
 */
export interface StepOutcome {
  step: ScriptStep;
  index: number;
  status: "ok" | "error" | "skipped";
  /**
   * Why, not what: the reason alone, with no "skipped"/"error" prefix. The
   * status already carries that, and a caller that renders both was printing
   * "skipped (skipped (destructive: ...))".
   */
  message: string;
  /** The action's raw result, only set when status === "ok". */
  result?: unknown;
}

export interface RunScriptOptions {
  /** Refs already approved for this session. Destructive steps referencing these run unattended. */
  approved?: ReadonlySet<string>;
  /** Called after each step with its outcome. Use to push run-turns to the transcript. */
  onStep?: (outcome: StepOutcome) => void;
  /**
   * If `true`, refuse to run the script and call `onStep` with status
   * `"skipped"` for *every* step when there are unapproved destructive refs.
   * Default `true`. Set `false` to skip just the destructive steps and run
   * the rest.
   */
  refuseOnUnapproved?: boolean;
  /**
   * Checked before every step; `false` abandons the rest of the run.
   *
   * A plan is a sequence of real side effects, so "stop" has to mean stop
   * between them — without this the Stop button cancelled the `multi_inquire`
   * invocation but let an executing plan run to completion, and a session
   * switch mid-run kept appending outcomes to whatever session was open by
   * then. There is no way to interrupt a step already in flight; the
   * guarantee is that no *further* step starts.
   */
  shouldContinue?: () => boolean;
}

/**
 * A script's `destructive[]` / `steps[]`, defensively.
 *
 * Results are structural only — solx-server never validates result shapes
 * (see `types.ts`) — and a script can also arrive from a session document
 * written by an older build. An unguarded `script.destructive.some(...)`
 * throws a TypeError before the destructive gate has run, which fails closed
 * but silently.
 */
function destructiveRefs(script: MultiInquireScript): string[] {
  return Array.isArray(script?.destructive) ? script.destructive : [];
}

function stepsOf(script: MultiInquireScript): ScriptStep[] {
  return Array.isArray(script?.steps) ? script.steps : [];
}

/** The outcome recorded for whatever step was next when a run was abandoned. */
function cancelledOutcome(step: ScriptStep, index: number): StepOutcome {
  return { step, index, status: "skipped", message: "cancelled before this step ran" };
}

/**
 * Execute one `multi_inquire` script in order, substituting captures
 * between steps. Stops on the first error and surfaces that step's
 * outcome as `"error"`; later steps don't run.
 *
 * Destructive gating: if any step's `action_ref` is in `script.destructive`
 * but not in `approved`, the script refuses (or skips just those steps).
 * Either way, `onStep` is called for each step with its outcome — the
 * transcript gets one run-turn per step regardless.
 */
export async function runScript(
  host: Host,
  script: MultiInquireScript,
  options: RunScriptOptions = {},
): Promise<StepOutcome[]> {
  const approved = options.approved ?? new Set<string>();
  const refuse = options.refuseOnUnapproved ?? true;
  const shouldContinue = options.shouldContinue ?? (() => true);
  const destructive = destructiveRefs(script);
  const steps = stepsOf(script);
  const captures: Record<string, unknown> = {};
  const outcomes: StepOutcome[] = [];

  if (refuse && destructive.some((r) => !approved.has(r))) {
    for (let i = 0; i < steps.length; i++) {
      const step = steps[i];
      const outcome: StepOutcome = {
        step,
        index: i,
        status: "skipped",
        message: `script contains unapproved destructive action: ${unapprovedDestructiveRefs(script, approved).join(", ")}`,
      };
      outcomes.push(outcome);
      options.onStep?.(outcome);
    }
    return outcomes;
  }

  for (let i = 0; i < steps.length; i++) {
    const step = steps[i];
    // Between steps, never mid-step: by here the previous step's side effect
    // has already happened, and this is the last moment before the next one.
    if (!shouldContinue()) {
      const outcome = cancelledOutcome(step, i);
      outcomes.push(outcome);
      options.onStep?.(outcome);
      return outcomes;
    }
    const isDestructive = destructive.includes(step.action_ref);
    if (isDestructive && !approved.has(step.action_ref)) {
      const outcome: StepOutcome = {
        step,
        index: i,
        status: "skipped",
        message: `destructive, not approved: ${step.action_ref}`,
      };
      outcomes.push(outcome);
      options.onStep?.(outcome);
      continue;
    }

    const params = substituteCaptures(step.params ?? {}, captures);
    const r = await host.try<unknown>(step.action_ref, params);
    if (!r.ok) {
      const outcome: StepOutcome = {
        step,
        index: i,
        status: "error",
        message: r.error,
      };
      outcomes.push(outcome);
      options.onStep?.(outcome);
      // stop on first error
      return outcomes;
    }

    if (step.capture) {
      captures[step.capture] = r.value;
    }
    const outcome: StepOutcome = {
      step,
      index: i,
      status: "ok",
      message: `ran ${step.action_ref}`,
      result: r.value,
    };
    outcomes.push(outcome);
    options.onStep?.(outcome);
  }

  return outcomes;
}

// ── follow-up instructions ──────────────────────────────────────────────────

/** How much of one response rides along in a follow-up instruction. */
const FOLLOW_UP_RESPONSE_CAP = 600;
/** Budget on the whole findings block, so it informs the model without drowning it. */
const FOLLOW_UP_BLOCK_CAP = 2000;

/**
 * Compose the instruction an auto-loop follow-up actually sends.
 *
 * `next_prompt` is written by the **intent** phase — before that turn searched
 * anything (see solx-inquiry's `intent.rs`, which calls it speculative and
 * notes that nothing in the pipeline re-invokes itself with it). Sent bare, a
 * follow-up is therefore chosen in ignorance of what the turn that suggested
 * it went on to find, and nothing downstream repairs that: the next turn's
 * intent call reads history through `session::history_block`, which extracts
 * only the instruction and `responses[0].text`, each truncated to 240
 * characters. Scripts, later responses, notes and errors never reach it.
 *
 * So the findings are inlined here, in the instruction itself. That keeps it
 * entirely caller-side — no `context_documents` (whose per-document cap would
 * truncate a session document from its *oldest* turn, and which would also
 * duplicate the history block under contradictory framing) and no handoff
 * document to write and keep in step.
 *
 * Returns `nextPrompt` unchanged when the turn produced nothing worth
 * carrying: a preamble promising results that are not there is worse than no
 * preamble, because the model will tend to satisfy it by inventing them.
 */
export function buildFollowUpInstruction(nextPrompt: string, last: MultiInquireResult): string {
  const findings = summarizeFindings(last);
  if (!findings) return nextPrompt;
  return [
    nextPrompt,
    "",
    "---",
    "The instruction above was suggested by the previous turn's planning phase,",
    "before that turn had searched anything or seen any results. What it actually",
    "found is below. Treat the suggestion as a starting point, not a decision:",
    "narrow it, revise it, or answer directly if it has already been covered.",
    "",
    findings,
  ].join("\n");
}

/** The previous turn, as lines a follow-up can be reasoned about against. */
function summarizeFindings(last: MultiInquireResult): string | null {
  const lines: string[] = [];

  const asked = typeof last?.instruction === "string" ? last.instruction.trim() : "";
  if (asked) lines.push(`Previously asked: ${truncateText(asked, FOLLOW_UP_RESPONSE_CAP)}`);

  for (const r of asArray(last?.responses)) {
    const text = typeof r?.text === "string" ? r.text.trim() : "";
    if (!text) continue;
    const cited = asStrings(r?.citations);
    const suffix = cited.length > 0 ? ` [cited: ${cited.join(", ")}]` : "";
    lines.push(`- found: ${truncateText(text, FOLLOW_UP_RESPONSE_CAP)}${suffix}`);
  }

  for (const script of asArray(last?.scripts)) {
    const title = typeof script?.title === "string" && script.title ? script.title : "(untitled)";
    const steps = asArray(script?.steps).length;
    const destructive = asArray(script?.destructive).length;
    lines.push(
      `- proposed a plan "${title}" (${steps} step(s)` +
        (destructive > 0 ? `, ${destructive} destructive` : "") +
        ", not run)",
    );
  }

  for (const note of asStrings(last?.notes)) {
    if (note.trim()) {
      lines.push(`- note: ${truncateText(note.trim(), FOLLOW_UP_RESPONSE_CAP)}`);
    }
  }

  // Worth its own line rather than folded into the notes: a follow-up written
  // as though everything succeeded is the wrong follow-up when some of it did
  // not, and the count is what says so.
  const errors = asArray(last?.errors).length;
  if (errors > 0) {
    lines.push(`- ${errors} inquir${errors === 1 ? "y" : "ies"} failed, so the findings above are partial`);
  }

  // Nothing but the restated question is not a finding.
  if (lines.length === 0 || (lines.length === 1 && lines[0].startsWith("Previously asked"))) {
    return null;
  }
  return withinBudget(lines, FOLLOW_UP_BLOCK_CAP).join("\n");
}

/**
 * As many lines as fit, in the order given, plus the first unconditionally —
 * the same shape as solx-inquiry's `take_within_budget`, and for the same
 * reason: a clean cutoff at the caller's priority order beats a best-fit that
 * could keep something trivial for being short.
 */
function withinBudget(lines: string[], budget: number): string[] {
  const kept: string[] = [];
  let used = 0;
  for (const line of lines) {
    const sep = kept.length === 0 ? 0 : 1;
    if (kept.length > 0 && used + sep + line.length > budget) break;
    used += sep + line.length;
    kept.push(line);
  }
  return kept;
}

function truncateText(s: string, max: number): string {
  return s.length <= max ? s : s.slice(0, max) + "…";
}

/**
 * Any array-shaped field, as loosely typed rows. Results are structural only
 * (see `types.ts`), and a turn replayed from a session document written by an
 * older build may be missing whole fields.
 */
function asArray(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value) ? (value as Record<string, unknown>[]) : [];
}

/** The same, for a field that should hold plain strings. */
function asStrings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];
}
