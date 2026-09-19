import { useSolxWidgetClient } from "../../../solx-widgets/src/wrap/SolxWidgetContext";
import { Composer } from "./components/Composer";

/**
 * The solx-prompt chat widget.
 *
 * Scaffold: this mounts, reports whether it was handed a client, and renders a
 * disabled composer. The turn loop, the session picker, the console panel and
 * the step list all land with the UI (see the plan's step 9) — what is here now
 * exists so the whole install path can be verified end to end before any of
 * that is written.
 *
 * Nothing about this file is load-bearing yet *except* the client preamble: a
 * widget rendered outside a real mount, or by a host that supplied no client,
 * gets `undefined` and must say so rather than throwing. That is the one case
 * the real widget will also have to handle on its very first render.
 */
export interface PromptWidgetFields {
  /** Pre-fill the composer, so an action row can deep-link into a prompt. */
  prompt?: string;
  /** Chat model to select on mount. Falls back to the stored choice. */
  model?: string;
  /** Session to open on mount. A fresh name is minted when absent. */
  session?: string;
}

export function PromptWidget({ fields }: { fields: PromptWidgetFields | undefined }) {
  const client = useSolxWidgetClient();

  return (
    <div className="col" style={{ gap: 10, padding: 10 }}>
      <div className="row" style={{ justifyContent: "space-between", alignItems: "center" }}>
        <strong>Prompt</strong>
        <span className={client ? "chip ok" : "chip warn"}>
          {client ? "connected" : "no solx client"}
        </span>
      </div>

      <span className="faint" style={{ fontSize: 11 }}>
        Scaffold build — the prompt loop is not wired up yet.
        {fields?.session ? ` Session: ${fields.session}` : ""}
      </span>

      <Composer
        disabled
        running={false}
        placeholder={fields?.prompt ?? "What would you like done?"}
        onSend={() => {}}
        onStop={() => {}}
      />
    </div>
  );
}
