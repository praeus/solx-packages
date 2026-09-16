import { useState, type KeyboardEvent } from "react";

/**
 * Prompt box for the chat thread.
 *
 * Enter sends, Shift+Enter is a newline. Disabled while a turn is in flight
 * so two sends can't race; the Stop button cancels the running turn.
 *
 * Mirrors solx-agent's Composer on purpose — same muscle memory — but
 * stops at the dispatch layer rather than between agent iterations: a
 * single xprompt turn is one ollama-chat or one inquire call, both of
 * which the host can cancel via invocations.stop.
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
