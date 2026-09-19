/**
 * Turning progress state into the words and chip classes the strip renders.
 *
 * Kept out of the component and free of JSX on purpose: the exact strings are
 * the part worth pinning in tests ("2 inquiries running" is the feature), and
 * vitest runs here without a DOM.
 */

import type { InquiryPhase, InquiryProgress, ProgressState } from "./reducer";

/** The `.chip` variants `theme.ts` defines. `""` is the plain chip. */
export type ChipClass = "" | "ok" | "warn" | "danger" | "accent";

/** What the run as a whole is doing right now. */
export function stageLabel(state: ProgressState): string {
  switch (state.stage) {
    case "idle":
      return "starting…";
    case "recall":
      return "recalling";
    case "context":
      return "reading context";
    case "intent":
      return "thinking";
    case "fanout": {
      // The headline. Inquiries are searched before any of them reaches the
      // model, so an inquire-mode run passes through "searching" on its way
      // to the count the user is actually waiting on.
      const running = count(state, "running");
      if (running > 0) return `${plural(running)} running`;
      const pending = count(state, "planned") + count(state, "searched");
      if (pending > 0) return `${plural(pending)} searching`;
      return state.inquiries.length > 0 ? "assembling" : "planning";
    }
    case "assembling":
      return "assembling";
    case "done":
      return "done";
    case "failed":
      return state.reason === "all_inquiries_failed" ? "every inquiry failed" : "failed";
    case "cancelled":
      return "cancelled";
  }
}

export function stageChipClass(state: ProgressState): ChipClass {
  switch (state.stage) {
    case "done":
      return state.inquiries.some((one) => one.phase === "failed") ? "warn" : "ok";
    case "failed":
      return "danger";
    case "cancelled":
      return "warn";
    default:
      return "accent";
  }
}

/** True while the run is still moving, so the strip can say so. */
export function isLive(state: ProgressState): boolean {
  return state.stage !== "idle" && !["done", "failed", "cancelled"].includes(state.stage);
}

/**
 * How a finished turn ended, as the widget knows it — which is not the same
 * as what the console feed managed to read. See `ProgressStrip`.
 */
export type TurnOutcome = "done" | "error" | "stopped";

export const OUTCOME_LABEL: Record<TurnOutcome, string> = {
  done: "done",
  error: "failed",
  stopped: "stopped",
};

export const OUTCOME_CHIP: Record<TurnOutcome, ChipClass> = {
  done: "ok",
  error: "danger",
  stopped: "warn",
};

/**
 * One inquiry's chip: `#2 running`, `#1 7 hits`, `#3 failed`.
 *
 * `ended` is set once the turn is over. An inquiry still sitting in a
 * non-terminal phase then never reported its completion — the run finished
 * regardless, so saying "running" beside a "done" headline would be a
 * contradiction, and claiming it succeeded would be an invention.
 */
export function inquiryChipLabel(one: InquiryProgress, ended = false): string {
  const n = `#${one.index + 1}`;
  if (ended && (one.phase === "planned" || one.phase === "searched" || one.phase === "running")) {
    return `${n} ended`;
  }
  switch (one.phase) {
    case "planned":
      return `${n} queued`;
    case "searched":
      return one.hits === undefined ? `${n} searched` : `${n} ${one.hits} hits`;
    case "running":
      return `${n} running`;
    case "done":
      return `${n} done`;
    case "failed":
      return `${n} failed`;
  }
}

export function inquiryChipClass(phase: InquiryPhase, ended = false): ChipClass {
  if (ended && phase !== "done" && phase !== "failed") return "";
  switch (phase) {
    case "running":
      return "accent";
    case "done":
      return "ok";
    case "failed":
      return "danger";
    default:
      return "";
  }
}

/**
 * The hover text, as a plain multi-line string for a native `title`.
 *
 * `title` rather than a popover because this widget mounts inside a shadow
 * root (no reliable portal target) and the thread it sits above is
 * `overflow-y: auto` (which would clip an absolutely positioned one). The
 * strip pairs it with a disclosure, since `title` is unusable on touch and
 * invisible to most assistive tech.
 */
export function inquiryTooltip(one: InquiryProgress, ended = false): string {
  const lines = [one.kind ? `#${one.index + 1} ${one.kind}` : `#${one.index + 1}`];
  if (one.question) lines.push(`"${one.question}"`);
  if (one.hits !== undefined) lines.push(`${one.hits} ${one.hits === 1 ? "hit" : "hits"}`);
  lines.push(phaseWord(one, ended));
  return lines.join("\n");
}

/** How one inquiry ended, or that it has not. */
export function phaseWord(one: InquiryProgress, ended = false): string {
  if (ended && one.phase !== "done" && one.phase !== "failed") {
    return `${one.phase} (no completion reported)`;
  }
  if (one.phase === "failed") return one.reason ? `failed (${one.reason})` : "failed";
  // A terminal status that is not `ok` says something the phase alone does
  // not — `timeout` and `cancelled` both land on `done`/`failed`.
  if (one.phase === "done" && one.status && one.status !== "ok") return one.status;
  return one.phase;
}

/** Why the fan-out went sequential, in words. */
export function degradedLabel(reason: string): string {
  return reason === "host_cannot_detach"
    ? "one at a time (host cannot run them in parallel)"
    : "one at a time";
}

function count(state: ProgressState, phase: InquiryPhase): number {
  return state.inquiries.filter((one) => one.phase === phase).length;
}

function plural(n: number): string {
  return n === 1 ? "1 inquiry" : `${n} inquiries`;
}
