import { useState, type KeyboardEvent } from "react";

/**
 * The question box.
 *
 * The textarea is never disabled -- you can draft a follow-up while an answer
 * is being written. `disabled` only gates *sending*, so the draft queues in
 * the box until it is safe. Enter sends; Shift+Enter is a newline.
 *
 * Much simpler than the agent's composer in one way: there is no approval and
 * no mid-turn decision, so the only in-flight state is "writing an answer",
 * and Stop is the only control that appears.
 */
export function Composer({
  disabled,
  running,
  placeholder,
  onAsk,
  onStop,
}: {
  disabled: boolean;
  running: boolean;
  placeholder: string;
  onAsk: (question: string) => void;
  onStop: () => void;
}) {
  const [draft, setDraft] = useState("");

  const ask = () => {
    const text = draft.trim();
    if (!text || disabled) return;
    setDraft("");
    onAsk(text);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      ask();
    }
  };

  return (
    <div className="row" style={{ gap: 6, alignItems: "flex-end" }}>
      <textarea
        rows={2}
        value={draft}
        placeholder={placeholder}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={onKeyDown}
      />
      {running ? (
        <button onClick={onStop} style={{ whiteSpace: "nowrap" }}>
          Stop
        </button>
      ) : (
        <button className="primary" disabled={disabled || !draft.trim()} onClick={ask}>
          Ask
        </button>
      )}
    </div>
  );
}
