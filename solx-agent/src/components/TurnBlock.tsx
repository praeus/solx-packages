import { useEffect, useRef, useState } from "react";
import { summariseTurn, type RenderedTurn } from "../transcript";
import { ToolCallCard } from "./ToolCallCard";

/**
 * One exchange: what was asked, everything the agent did about it, and what
 * it said back.
 *
 * The work is shown rather than hidden. A single message here can mean a
 * dozen iterations and thirty tool calls against real documents, and a chat
 * bubble that quietly swallowed that would be misrepresenting what ran. A
 * finished turn collapses to a one-line recap, so a long thread stays
 * readable without the detail becoming unreachable.
 */
export function TurnBlock({
  turn,
  live,
  defaultExpanded,
  collapseSignal,
}: {
  turn: RenderedTurn;
  live: boolean;
  /** The newest turn stays open: it is what was just watched, and when a turn
   *  suspends for approval its work is the context for that decision. */
  defaultExpanded: boolean;
  /** Bumped by "Collapse all" -- any change folds this turn's sections. */
  collapseSignal?: number;
}) {
  // Null until the reader says otherwise, so the default can keep changing as
  // newer turns arrive without overriding a deliberate collapse.
  const [override, setOverride] = useState<boolean | null>(null);
  const [answerOverride, setAnswerOverride] = useState<boolean | null>(null);
  const showWork = live || (override ?? defaultExpanded);
  const showAnswer = answerOverride ?? defaultExpanded;
  const hasWork = turn.iterations.length > 0;

  // Only react to an actual change in the signal -- not to its initial value
  // on mount, which would collapse a freshly-opened newest turn.
  const prevSignal = useRef(collapseSignal);
  useEffect(() => {
    if (collapseSignal !== prevSignal.current) {
      prevSignal.current = collapseSignal;
      setOverride(false);
      setAnswerOverride(false);
    }
  }, [collapseSignal]);

  return (
    <div className="col" style={{ gap: 6 }}>
      {turn.systemNotes.length > 0 && <SystemNotes notes={turn.systemNotes} />}

      {turn.user !== null && (
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
            {turn.user}
          </div>
        </div>
      )}

      {hasWork && (
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
            onClick={() => setOverride(!showWork)}
            disabled={live}
            style={{ background: "none", border: "none", padding: 0, textAlign: "left" }}
          >
            <span className="muted" style={{ fontSize: 11 }}>
              {live ? "working…" : `${showWork ? "▾" : "▸"} ${summariseTurn(turn)}`}
            </span>
          </button>

          {showWork &&
            turn.iterations.map((iteration) => (
              <div key={iteration.index} className="col" style={{ gap: 4 }}>
                {iteration.narration && (
                  <div style={{ whiteSpace: "pre-wrap" }}>{iteration.narration}</div>
                )}
                {iteration.calls.map((call, i) => (
                  <ToolCallCard key={`${iteration.index}-${i}`} call={call} />
                ))}
              </div>
            ))}
        </div>
      )}

      {turn.answer !== null && (
        <div className="col" style={{ gap: 4, maxWidth: "95%" }}>
          <button
            onClick={() => setAnswerOverride(!showAnswer)}
            style={{ background: "none", border: "none", padding: 0, textAlign: "left" }}
          >
            <span className="muted" style={{ fontSize: 11 }}>
              {showAnswer ? "▾" : "▸"} agent response
            </span>
          </button>
          {showAnswer && (
            <div style={{ whiteSpace: "pre-wrap" }}>
              {turn.answer.trim() === "" ? (
                <span className="faint">(ended the turn without saying anything)</span>
              ) : (
                turn.answer
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * Seed material and mid-thread context additions. Collapsed by default: it
 * is reference material handed to the model, not part of the conversation,
 * and it is long -- memories, a context index, skill instructions.
 */
function SystemNotes({ notes }: { notes: string[] }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="col" style={{ gap: 4 }}>
      <button
        onClick={() => setOpen((v) => !v)}
        style={{ background: "none", border: "none", padding: 0, textAlign: "left" }}
      >
        <span className="chip">
          {open ? "▾" : "▸"} {notes.length === 1 ? "context given to the model" : `${notes.length} context blocks`}
        </span>
      </button>
      {open &&
        notes.map((note, i) => (
          <pre key={i} style={{ maxHeight: 220, overflowY: "auto" }}>
            {note}
          </pre>
        ))}
    </div>
  );
}
