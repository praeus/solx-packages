import { useState } from "react";
import { detailOf } from "../transcript";
import type { PlanStep } from "../harness";

const OUTCOME_CLASS: Record<string, string> = {
  ok: "chip ok",
  empty: "chip warn",
  error: "chip danger",
  skipped: "chip",
};

/** The fields a rendered plan row needs; both `RenderedStep` and `ExecutedStep` satisfy it. */
export interface PlanRow {
  verb: string;
  action: string;
  outcome: string;
  count: number;
  error: string | null;
  note: string | null;
}

function pretty(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

/**
 * The plan: what the model decided to look up, and how each lookup went.
 *
 * This is the piece the agent has no analogue for. The agent renders *tool
 * calls* -- wire names the model chose from a catalogue. Inquiry renders the
 * model's *intent* (a verb and some words) beside the deterministic action it
 * resolved to, which is far more legible: a reader can see that "search blogs
 * for rust" became `search-documents {q:"rust", pathPrefix:"/blog"}` and
 * immediately know whether the model understood the question.
 *
 * When a step has run, its outcome and count are shown on the same row; when
 * it has not (a live turn, or `plan_only` output), it shows as pending.
 */
export function PlanCard({ plan, steps }: { plan: PlanStep[]; steps: PlanRow[] }) {
  const [openIndex, setOpenIndex] = useState<number | null>(null);
  const byVerb = steps;

  return (
    <div className="col" style={{ gap: 4 }}>
      <span className="muted" style={{ fontSize: 11 }}>
        lookups the model chose
      </span>
      {plan.map((step, i) => {
        const ran = byVerb[i];
        const outcome = ran?.outcome;
        return (
          <div
            key={i}
            style={{
              border: "1px solid var(--border)",
              borderRadius: "var(--radius)",
              background: "var(--bg-raised)",
              padding: "5px 7px",
            }}
          >
            <div className="row" style={{ justifyContent: "space-between", gap: 8 }}>
              <button
                onClick={() => setOpenIndex(openIndex === i ? null : i)}
                style={{
                  background: "none",
                  border: "none",
                  padding: 0,
                  textAlign: "left",
                  flex: 1,
                  minWidth: 0,
                  fontSize: 12,
                }}
                title={ran?.action || step.verb}
              >
                <span className="faint">{openIndex === i ? "▾" : "▸"} </span>
                <span style={{ fontFamily: "var(--font-mono)" }}>{step.verb}</span>
                <span className="muted"> {detailOf(step)}</span>
              </button>
              {outcome ? (
                <span className={OUTCOME_CLASS[outcome] ?? "chip"}>
                  {outcome === "ok" ? `${ran.count} doc${ran.count === 1 ? "" : "s"}` : outcome}
                </span>
              ) : (
                <span className="chip">pending</span>
              )}
            </div>

            {openIndex === i && (
              <div className="col" style={{ gap: 4, marginTop: 5 }}>
                {ran?.action && (
                  <div className="faint" style={{ fontSize: 11, fontFamily: "var(--font-mono)" }}>
                    {ran.action}
                  </div>
                )}
                {ran && (
                  <>
                    <div className="muted" style={{ fontSize: 11 }}>sent</div>
                    <pre>{pretty(ran)}</pre>
                  </>
                )}
              </div>
            )}

            {ran?.note && (
              <div className="faint" style={{ fontSize: 11, marginTop: 3 }}>
                {ran.note}
              </div>
            )}
            {ran?.error && (
              <div className="faint" style={{ fontSize: 11, marginTop: 3, color: "var(--danger)" }}>
                {ran.error}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
