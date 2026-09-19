// One level deeper than solx-xprompt's, so every import into the shared toolkit
// carries an extra `../`. See the README's layout note.
import { defineReactWidget } from "../../../solx-widgets/src/wrap/defineReactWidget";
import { PromptWidget, type PromptWidgetFields } from "./PromptWidget";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { WIDGET_STYLES } from "./theme";

/**
 * Root backstop. The per-turn boundaries inside `PromptWidget` keep a bad turn
 * from blanking the thread; this keeps anything outside the thread — the
 * toolbar, the composer, the progress strip — from blanking the page that hosts
 * the widget.
 */
function PromptWidgetRoot({ fields }: { fields: PromptWidgetFields | undefined }) {
  return (
    <ErrorBoundary
      label="widget"
      fallback={(error) => (
        <div className="col" style={{ gap: 6, padding: 10 }}>
          <div className="chip danger" style={{ alignSelf: "flex-start", padding: "6px 10px" }}>
            Prompt could not be displayed: {error.message}
          </div>
          <span className="faint" style={{ fontSize: 11 }}>
            Reloading usually clears this. If it does not, the stored transcript
            is the likely cause — clear this site&apos;s data for the widget.
          </span>
        </div>
      )}
    >
      <PromptWidget fields={fields} />
    </ErrorBoundary>
  );
}

defineReactWidget<PromptWidgetFields>("solx-prompt-widget", PromptWidgetRoot, {
  styles: WIDGET_STYLES,
});
