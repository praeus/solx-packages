import { useEffect, useRef, useState } from "react";
import type { WidgetClient } from "../../../solx-widgets/src/shared/widgetClient";
import type { Host } from "../../../solx-widgets/src/wrap/host";
import { loadCallLog, readMergedConsole } from "../console";
import type { MergedEntry, MergeCursors } from "../console";

/**
 * The merged console for this widget session's tracked calls — currently
 * one tracked call per turn, its `multi_inquire` invocation (see
 * `dispatch.ts`) — as one time-ordered feed. Reload-safe because the call
 * log itself is a saved document (see `src/console/`).
 *
 * Long-polls `readMergedConsole` while this panel is mounted (i.e. while the
 * Console tab is open) and reloads the call log on every cycle so a turn
 * started after this panel mounted is picked up, not just the ones tracked
 * before it.
 */
export function ConsolePanel({
  host,
  client,
  logId,
}: {
  host: Host;
  client: WidgetClient;
  logId: string;
}) {
  const [entries, setEntries] = useState<MergedEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const cursorsRef = useRef<MergeCursors>({});

  useEffect(() => {
    let cancelled = false;
    setEntries([]);
    setError(null);
    cursorsRef.current = {};

    void (async () => {
      while (!cancelled) {
        try {
          const log = await loadCallLog(host, logId);
          if (log.calls.length === 0) {
            await sleep(1500);
            continue;
          }
          const { entries: batch, cursors } = await readMergedConsole(client, log, cursorsRef.current, {
            limit: 200,
            waitSecs: 25,
          });
          if (cancelled) return;
          cursorsRef.current = cursors;
          if (batch.length > 0) {
            setEntries((prev) => [...prev, ...batch].slice(-500));
          }
        } catch (err) {
          if (cancelled) return;
          setError(err instanceof Error ? err.message : String(err));
          await sleep(3000);
        }
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [host, client, logId]);

  return (
    <div className="col" style={{ gap: 6, flex: 1, minHeight: 120, overflow: "hidden" }}>
      {error && (
        <div className="chip danger" style={{ alignSelf: "flex-start" }}>
          {error}
        </div>
      )}
      <div
        className="col"
        style={{
          gap: 4,
          flex: 1,
          overflowY: "auto",
          background: "var(--bg-sunken)",
          border: "1px solid var(--border)",
          borderRadius: "var(--radius)",
          padding: 8,
        }}
      >
        {entries.length === 0 && (
          <div className="faint" style={{ padding: 10 }}>
            No calls tracked yet. Send a prompt — each turn's recall, intent, and
            inquiry phases stream in here as they run.
          </div>
        )}
        {entries.map((e) => (
          <div
            key={e.invocationId + ":" + e.seq}
            className="row"
            style={{ gap: 6, alignItems: "baseline", flexWrap: "wrap" }}
          >
            <span className="faint" style={{ fontSize: 11 }}>{formatTime(e.ts)}</span>
            <span className="chip accent" style={{ fontSize: 10 }}>{e.label}</span>
            <span style={{ fontSize: 12, fontFamily: "var(--font-mono)", whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
              {e.message ?? ""}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function formatTime(iso: string): string {
  try {
    return new Date(iso).toLocaleTimeString();
  } catch {
    return iso;
  }
}
