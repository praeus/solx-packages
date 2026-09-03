import { useState, type KeyboardEvent } from "react";

/**
 * The message box.
 *
 * Enabled whenever nothing is in flight -- including after `idle`, which
 * means the model handed the turn back rather than that the session ended.
 *
 * Stop lands *between* iterations, so it may take a moment: the loop only
 * checks once the in-flight model call returns. There is nothing to force,
 * and nothing is lost by waiting -- every tool result is persisted as it
 * lands, so a stopped turn leaves a true transcript either way.
 */
export function Composer({
  disabled,
  running,
  placeholder,
  onSend,
  onStop,
}: {
  disabled: boolean;
  running: boolean;
  placeholder: string;
  onSend: (message: string) => void;
  onStop: () => void;
}) {
  const [draft, setDraft] = useState("");

  const send = () => {
    const text = draft.trim();
    if (!text || disabled) return;
    setDraft("");
    onSend(text);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    // Enter sends; Shift+Enter is a newline. A message is usually one line,
    // and a multi-line one is rare enough to be worth the modifier.
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  };

  return (
    <div className="row" style={{ gap: 6, alignItems: "flex-end" }}>
      <textarea
        rows={2}
        value={draft}
        placeholder={placeholder}
        disabled={disabled}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={onKeyDown}
      />
      {running ? (
        <button onClick={onStop} style={{ whiteSpace: "nowrap" }}>
          Stop
        </button>
      ) : (
        <button className="primary" disabled={disabled || !draft.trim()} onClick={send}>
          Send
        </button>
      )}
    </div>
  );
}
