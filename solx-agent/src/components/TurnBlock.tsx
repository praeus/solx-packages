import { useState } from "react";
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
}: {
  turn: RenderedTurn;
  live: boolean;
  /** The newest turn stays open: it is what was just watched, and when a turn
   *  suspends for approval its work is the context for that decision. */
  defaultExpanded: boolean;
}) {
  // Null until the reader says otherwise, so the default can keep changing as
  // newer turns arrive without overriding a deliberate collapse.
  const [override, setOverride] = useState<boolean | null>(null);
  const showWork = live || (override ?? defaultExpanded);
  const hasWork = turn.iterations.length > 0;

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
                <div className="faint" style={{ fontSize: 11 }}>
                  iteration {iteration.index}
                </div>
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
        <div style={{ maxWidth: "95%", whiteSpace: "pre-wrap" }}>
          {turn.answer.trim() === "" ? (
            <span className="faint">(ended the turn without saying anything)</span>
          ) : (
            turn.answer
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
