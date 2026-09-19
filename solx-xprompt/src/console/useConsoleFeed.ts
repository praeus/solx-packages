import { useEffect, useRef, useState } from "react";
import type { WidgetClient } from "../../../solx-widgets/src/shared/widgetClient";
import type { Host } from "../../../solx-widgets/src/wrap/host";
import { loadCallLog } from "./callLog";
import { readMergedConsole } from "./merge";
import type { MergedEntry, MergeCursors } from "./types";

/** How many entries to keep. Older ones are dropped from the head. */
const RETAIN = 500;
const BATCH_LIMIT = 200;
const TAIL_WAIT_SECS = 25;
const EMPTY_LOG_RETRY_MS = 1500;
const ERROR_BACKOFF_MS = 3000;

/**
 * Long-poll this session's merged console into React state.
 *
 * This used to live inside `ConsolePanel`, which meant it ran only while the
 * Console tab was mounted — so the moment you switched to Chat, the feed
 * stopped, and nothing on the Chat tab could be driven by it. Hoisting it to a
 * hook lets `XPromptWidget` (always mounted) own the one loop and hand the
 * entries to both the Console tab and the progress strip.
 *
 * `active` gates it rather than a mount boundary. An always-on loop would hold
 * a long-poll open and re-read the call-log document every cycle forever on an
 * idle widget; `busy || tab === "console"` is true exactly when the entries
 * are wanted. Cursors live in a ref, so going inactive and back does not
 * re-read what was already seen.
 *
 * The call log is reloaded every cycle, not once, so a turn started after this
 * loop began is picked up rather than only the calls tracked before it.
 */
export function useConsoleFeed(
  host: Host | null,
  client: WidgetClient | null | undefined,
  logId: string,
  opts: { active: boolean },
): { entries: MergedEntry[]; error: string | null } {
  const [entries, setEntries] = useState<MergedEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const cursorsRef = useRef<MergeCursors>({});
  const { active } = opts;

  // Reset only when the *session* changes — a different call log is a
  // different console. Going inactive keeps what was read.
  useEffect(() => {
    setEntries([]);
    setError(null);
    cursorsRef.current = {};
  }, [logId]);

  useEffect(() => {
    if (!host || !client || !active) return;
    let cancelled = false;

    void (async () => {
      while (!cancelled) {
        try {
          const log = await loadCallLog(host, logId);
          if (cancelled) return;
          if (log.calls.length === 0) {
            await sleep(EMPTY_LOG_RETRY_MS);
            continue;
          }
          const { entries: batch, cursors } = await readMergedConsole(client, log, cursorsRef.current, {
            limit: BATCH_LIMIT,
            waitSecs: TAIL_WAIT_SECS,
          });
          if (cancelled) return;
          cursorsRef.current = cursors;
          if (batch.length > 0) {
            setEntries((prev) => [...prev, ...batch].slice(-RETAIN));
          }
        } catch (err) {
          if (cancelled) return;
          setError(err instanceof Error ? err.message : String(err));
          await sleep(ERROR_BACKOFF_MS);
        }
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [host, client, logId, active]);

  return { entries, error };
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
