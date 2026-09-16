import { useMemo } from "react";
import { useSolxWidgetClient } from "../../solx-widgets/src/wrap/SolxWidgetContext";
import { hostFromClient, type Host } from "../../solx-widgets/src/wrap/host";

export interface XPromptWidgetFields {
  /** Optional heading. TODO: replace with the fields this widget actually needs. */
  title?: string;
}

/**
 * TODO: describe what this widget does.
 *
 * Scaffolding only — no UI yet. `useSolxWidgetClient()` returns the scoped
 * client the host (solx-web) injects at mount time; it's `undefined` when
 * rendered outside a real mount (tests, Storybook) or by a read-only host.
 * `hostFromClient` wraps it in `call()`/`try()` (see
 * `solx-widgets/src/wrap/host.ts`) for calling actions by `"path/name"` ref
 * — use `host.call(ref, params)` once this widget actually needs to invoke
 * one. See solx-packages/docs/widget-system.md for the full contract.
 */
export function XPromptWidget({ fields }: { fields: XPromptWidgetFields | undefined }) {
  const client = useSolxWidgetClient();
  const host: Host | null = useMemo(() => (client ? hostFromClient(client) : null), [client]);

  if (!client || !host) {
    return (
      <div className="muted" style={{ padding: 10 }}>
        No solx client — this widget needs a host that supplies one.
      </div>
    );
  }

  return (
    <div className="col" style={{ gap: 8, padding: 10 }}>
      <strong>{fields?.title ?? "solx-xprompt"}</strong>
      <span className="faint">TODO: build the UI.</span>
    </div>
  );
}
