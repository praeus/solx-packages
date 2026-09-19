import { Component, type ErrorInfo, type ReactNode } from "react";

/**
 * Keeps one component's render failure from taking the widget with it.
 *
 * React unmounts the whole tree when a render throws, and this widget's
 * transcript is persisted to localStorage — so without a boundary, one
 * malformed turn blanked the widget and *stayed* blank across reloads, with
 * no "Clear" button left to press. Recovery meant devtools.
 *
 * `src/result.ts` is the real defence: it normalises every result before it
 * can reach a component. This is the backstop for what that misses, and it is
 * deliberately placed per-turn rather than only at the root, so a bad turn
 * degrades to one error card while the rest of the thread, the composer and
 * the toolbar all keep working.
 *
 * A class component because `getDerivedStateFromError` and `componentDidCatch`
 * have no hook equivalent.
 */
export class ErrorBoundary extends Component<
  { children: ReactNode; fallback: (error: Error) => ReactNode; label?: string },
  { error: Error | null }
> {
  override state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error): { error: Error } {
    return { error };
  }

  override componentDidCatch(error: Error, info: ErrorInfo): void {
    // The widget has no console of its own to write to, and swallowing this
    // entirely would leave a caught crash with no trace anywhere.
    // eslint-disable-next-line no-console
    console.error(`solx-prompt: ${this.props.label ?? "render"} failed`, error, info.componentStack);
  }

  override render(): ReactNode {
    return this.state.error ? this.props.fallback(this.state.error) : this.props.children;
  }
}

/** What a turn that could not be rendered shows in its place. */
export function TurnRenderError({ error }: { error: Error }) {
  return (
    <div className="col" style={{ gap: 4 }}>
      <div
        className="chip danger"
        style={{ alignSelf: "flex-start", padding: "6px 10px" }}
        title={error.stack ?? error.message}
      >
        This turn could not be displayed: {error.message}
      </div>
      <span className="faint" style={{ fontSize: 11 }}>
        The rest of the session is unaffected — "Clear" removes it.
      </span>
    </div>
  );
}
