import { useState, type KeyboardEvent } from "react";

/**
 * Prompt box for the chat thread.
 *
 * Enter sends, Shift+Enter is a newline. Disabled while a turn is in flight
 * so two sends can't race; the Stop button cancels the running turn.
 *
 * Copied from solx-xprompt's, which copied solx-agent's - same muscle memory.
 * What Stop *means* is the one thing that differs in each: solx-agent stops
 * between agent iterations, xprompt cancels one detached multi_inquire
 * invocation, and here it has to halt a multi-step run part-way through - the
 * `prompt` call, or whichever step of the returned plan is currently executing.
 * That third meaning is why this stays copied rather than shared (see README).
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
        <button
          className="primary"
          disabled={disabled || !draft.trim()}
          onClick={send}
        >
          Send
        </button>
      )}
    </div>
  );
}
