import { useState } from "react";
import {
  degradedLabel,
  inquiryChipClass,
  inquiryChipLabel,
  inquiryTooltip,
  isSettled,
  OUTCOME_CHIP,
  OUTCOME_LABEL,
  phaseWord,
  stageChipClass,
  stageLabel,
  type InquiryProgress,
  type ProgressState,
  type TurnOutcome,
} from "../progress";

/**
 * What the current turn is doing, as one line of chips.
 *
 * Replaces the widget's entire former progress vocabulary — a `busy` boolean
 * rendering the word "working..." — with what `multi_inquire` actually reports
 * about itself: which phase it is in, and, once it fans out, one chip per
 * inquiry that completes independently of the others.
 *
 * Rendered between the tab row and the tab body, so it is visible on the
 * Console tab as well as on Chat. That is the payoff of hoisting the tail loop
 * out of `ConsolePanel` (see `console/useConsoleFeed`).
 *
 * Detail is available twice over, from the same data: a native `title` for
 * hover, and a disclosure for everyone that cannot hover — see
 * `progress/labels.ts` for why it is `title` and not a popover.
 */
export function ProgressStrip({
  progress,
  busy,
  outcome,
}: {
  progress: ProgressState;
  busy: boolean;
  /** How the widget saw the turn end. See below for why this outranks the feed. */
  outcome: TurnOutcome | null;
}) {
  const [open, setOpen] = useState(false);

  // While a turn is in flight but before its first event has been read, there
  // is nothing truthful to say beyond that it started.
  if (!busy && progress.stage === "idle") return null;

  // The console feed is the live *detail*, but it is not the authority on
  // whether a turn finished, and it cannot be: the feed is gated on `busy`,
  // so the batch carrying `run.done` races the turn's own completion and is
  // discarded whenever it loses. Reading "not settled" as "abandoned" made a
  // perfectly successful turn intermittently report itself as stopped. The
  // widget holds the turn's result, so `outcome` is what it actually knows;
  // the feed only gets to speak when it managed to read the ending itself.
  const settled = isSettled(progress);
  const ended: TurnOutcome | null = busy || settled ? null : (outcome ?? "stopped");
  const live = busy && !settled;
  const { inquiries } = progress;

  return (
    <div className="col" style={{ gap: 4 }}>
      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        <span className={chip(ended ? OUTCOME_CHIP[ended] : stageChipClass(progress), live)}>
          {ended ? OUTCOME_LABEL[ended] : stageLabel(progress)}
        </span>

        {inquiries.map((one) => (
          <span
            key={one.index}
            className={chip(inquiryChipClass(one.phase, !!ended), live && one.phase === "running")}
            title={inquiryTooltip(one, !!ended)}
          >
            {inquiryChipLabel(one, !!ended)}
          </span>
        ))}

        {progress.degraded && (
          <span className="chip warn" title="Detached invocations are unavailable on this host.">
            {degradedLabel(progress.degraded)}
          </span>
        )}

        {inquiries.length > 0 && (
          <button
            onClick={() => setOpen((v) => !v)}
            title={open ? "Hide inquiry detail" : "Show inquiry detail"}
            style={{ fontSize: 11, padding: "0 4px" }}
          >
            {open ? "▾" : "▸"}
          </button>
        )}
      </div>

      {open && inquiries.length > 0 && (
        <div
          className="col"
          style={{
            gap: 3,
            padding: "4px 8px",
            background: "var(--bg-sunken)",
            border: "1px solid var(--border)",
            borderRadius: "var(--radius)",
          }}
        >
          {inquiries.map((one) => (
            <InquiryRow key={one.index} one={one} ended={!!ended} />
          ))}
        </div>
      )}
    </div>
  );
}

function InquiryRow({ one, ended }: { one: InquiryProgress; ended: boolean }) {
  return (
    <div className="row" style={{ gap: 6, alignItems: "baseline", flexWrap: "wrap" }}>
      <span className={"chip " + inquiryChipClass(one.phase, ended)} style={{ fontSize: 10 }}>
        {"#" + (one.index + 1)}
      </span>
      <span style={{ fontSize: 12 }}>{one.question ?? "(question not reported)"}</span>
      <span className="faint" style={{ fontSize: 11 }}>
        {[one.kind, one.hits === undefined ? null : `${one.hits} hits`, phaseWord(one, ended)]
          .filter(Boolean)
          .join(" · ")}
      </span>
    </div>
  );
}

/** `.chip`, plus its variant, plus the pulse while it is the live one. */
function chip(variant: string, pulse: boolean): string {
  return ["chip", variant, pulse ? "pulse" : ""].filter(Boolean).join(" ");
}
