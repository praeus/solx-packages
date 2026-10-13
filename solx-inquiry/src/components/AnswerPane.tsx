/**
 * The answer: the one piece of real output.
 *
 * Kept open by default and rendered as prose, because unlike the plan and the
 * documents this *is* the thing the person asked for. The preamble asks the
 * model to cite document references inline, so the text is shown with
 * `white-space: pre-wrap` and no further processing -- markdown rendering
 * would be a whole dependency for a feature nobody asked for, and the
 * references are already readable as plain text.
 */
export function AnswerPane({
  answer,
  open,
  onToggle,
}: {
  answer: string;
  open: boolean;
  onToggle: () => void;
}) {
  return (
    <div className="col" style={{ gap: 4, maxWidth: "95%" }}>
      <button
        onClick={onToggle}
        style={{ background: "none", border: "none", padding: 0, textAlign: "left" }}
      >
        <span className="muted" style={{ fontSize: 11 }}>
          {open ? "▾" : "▸"} answer
        </span>
      </button>
      {open && (
        <div style={{ whiteSpace: "pre-wrap" }}>
          {answer.trim() === "" ? (
            <span className="faint">(no answer was written)</span>
          ) : (
            answer
          )}
        </div>
      )}
    </div>
  );
}
