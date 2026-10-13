import { useState } from "react";
import { summariseTurn, detailOf, liveLabel, type RenderedTurn } from "../transcript";
import { PlanCard } from "./PlanCard";
import { DocumentsPane } from "./DocumentsPane";
import { AnswerPane } from "./AnswerPane";

/**
 * One exchange: the question, the lookups it caused, the documents they found,
 * and the answer.
 *
 * The nesting is rendered rather than flattened, the same principle the agent
 * follows: a question here can mean a plan, four lookups and a dozen documents
 * read, and a chat bubble that swallowed all of it would misrepresent what
 * ran. What differs is that inquiry's version is *bounded* -- there is exactly
 * one plan, one step list, one document set, one answer per turn, so the block
 * has a fixed shape rather than an expanding list of iterations.
 */
export function TurnBlock({
  turn,
  live,
  defaultExpanded,
  collapseSignal,
}: {
  turn: RenderedTurn;
  live: boolean;
  defaultExpanded: boolean;
  collapseSignal?: number;
}) {
  const [open, setOpen] = useState<boolean | null>(null);
  const [answerOpen, setAnswerOpen] = useState<boolean | null>(null);
  // The work stays open while live, and once settled follows the caller's
  // default unless the reader has said otherwise.
  const showWork = live || (open ?? defaultExpanded);
  const showAnswer = answerOpen ?? true;

  return (
    <div className="col" style={{ gap: 6 }}>
      <div style={{ alignSelf: "flex-end", maxWidth: "85%" }}>
        <div
          style={{
            background: "var(--accent)",
            color: "#fff",
            padding: "6px 10px",
            borderRadius: 10,
            whiteSpace: "pre-wrap",
          }}
        >
          {turn.question}
        </div>
      </div>

      {(turn.plan.length > 0 || turn.liveStatus) && (
        <div
          className="col"
          style={{
            gap: 6,
            border: "1px solid var(--border)",
            borderRadius: "var(--radius)",
            padding: 7,
            background: "var(--bg)",
          }}
        >
          <button
            onClick={() => setOpen(!showWork)}
            disabled={live}
            style={{ background: "none", border: "none", padding: 0, textAlign: "left" }}
          >
            <span className="muted" style={{ fontSize: 11 }}>
              {live
                ? liveLabel(turn.liveStatus || "planning")
                : `${showWork ? "▾" : "▸"} ${summariseTurn(turn)}`}
            </span>
          </button>

          {showWork && turn.plan.length > 0 && <PlanCard plan={turn.plan} steps={turn.steps} />}

          {showWork && turn.documents.length > 0 && (
            <DocumentsPane documents={turn.documents} collapseSignal={collapseSignal} />
          )}

          {turn.planError && <span className="chip danger">{turn.planError}</span>}
        </div>
      )}

      {turn.answer !== null && (
        <AnswerPane answer={turn.answer} open={showAnswer} onToggle={() => setAnswerOpen(!showAnswer)} />
      )}
    </div>
  );
}

/** Re-exported so a caller can render the one-line recap without importing transcript directly. */
export { summariseTurn, detailOf };
