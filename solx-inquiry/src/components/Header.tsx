import { useEffect, useState } from "react";
import type { InquiryStatus, OllamaModel } from "../harness";

/** How each status reads to someone watching. */
const STATUS_LABEL: Record<InquiryStatus, { text: string; cls: string }> = {
  planning: { text: "planning", cls: "chip accent" },
  gathering: { text: "gathering", cls: "chip accent" },
  answering: { text: "answering", cls: "chip accent" },
  idle: { text: "answered", cls: "chip ok" },
  error: { text: "error", cls: "chip danger" },
};

export type ModelsState = "ok" | "unreachable" | "none";

export interface InquirySummary {
  name: string;
  title?: string;
  summary?: string;
}

/**
 * The top bar: model pick, inquiry history, status.
 *
 * Almost identical to solx-agent's, minus the approval and iteration chips --
 * inquiry has neither. The one addition is that switching inquiry or model is
 * always safe here, because an inquiry turn cannot be mid-approval.
 */
export function Header({
  models,
  modelsState,
  model,
  onModel,
  status,
  inquiryId,
  title,
  inquiries,
  onOpen,
  onNew,
  onDelete,
  busy,
}: {
  models: OllamaModel[];
  modelsState: ModelsState;
  model: string;
  onModel: (model: string) => void;
  status: InquiryStatus | null;
  inquiryId: string | null;
  title: string | null;
  inquiries: InquirySummary[];
  onOpen: (id: string) => void;
  onNew: () => void;
  onDelete: () => void;
  busy: boolean;
}) {
  const [confirmDelete, setConfirmDelete] = useState(false);
  const badge = status ? STATUS_LABEL[status] : null;

  // Disarm whenever the inquiry changes, by any route -- the select, the New
  // button, or the widget switching on its own. Without this an armed Delete
  // from a previous inquiry comes back already armed over the next one.
  useEffect(() => setConfirmDelete(false), [inquiryId]);

  return (
    <div className="col" style={{ gap: 5 }}>
      <div className="row" style={{ gap: 6, justifyContent: "space-between" }}>
        {models.length > 0 ? (
          <select
            value={model}
            onChange={(e) => onModel(e.target.value)}
            disabled={!!inquiryId}
            style={{ flex: 1, minWidth: 0 }}
            title="The model that plans and writes"
          >
            {!model && <option value="">Pick a model…</option>}
            {models.map((m) => (
              <option key={m.name} value={m.name}>
                {m.name}
              </option>
            ))}
          </select>
        ) : (
          <input
            placeholder="model name (e.g. qwen3:4b)"
            value={model}
            disabled={!!inquiryId}
            onChange={(e) => onModel(e.target.value)}
            style={{ flex: 1, minWidth: 0 }}
          />
        )}

        <select
          value={inquiryId ?? ""}
          onChange={(e) => {
            const id = e.target.value;
            setConfirmDelete(false);
            if (id) onOpen(id);
            else onNew();
          }}
          disabled={busy}
          title="Switch inquiry"
          style={{ flex: 1, minWidth: 0 }}
        >
          <option value="">— New inquiry —</option>
          {inquiries.map((s) => (
            <option key={s.name} value={s.name}>
              {s.title || s.name}
            </option>
          ))}
        </select>

        {inquiryId &&
          (confirmDelete ? (
            <span className="row" style={{ gap: 4 }}>
              <button onClick={() => { setConfirmDelete(false); onDelete(); }} disabled={busy}>
                Confirm
              </button>
              <button onClick={() => setConfirmDelete(false)} disabled={busy}>
                Keep
              </button>
            </span>
          ) : (
            <button onClick={() => setConfirmDelete(true)} disabled={busy} title="Delete this inquiry">
              Delete
            </button>
          ))}
      </div>

      <div className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        {badge && <span className={badge.cls}>{badge.text}</span>}
        {inquiryId && <span className="chip">{title || inquiryId}</span>}
        {modelsState === "unreachable" && (
          <span className="faint" style={{ fontSize: 11 }}>
            solx-ollama not reachable — type a model name
          </span>
        )}
        {modelsState === "none" && (
          <span className="faint" style={{ fontSize: 11 }}>
            No models found — type a model name
          </span>
        )}
      </div>
    </div>
  );
}
