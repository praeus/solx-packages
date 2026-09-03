import { createContext, useContext } from "react";
import type { WidgetClient } from "../shared/widgetClient";

const SolxWidgetContext = createContext<WidgetClient | undefined>(undefined);

/** Wraps a widget's rendered tree with the scoped client its host supplied (see defineReactWidget.tsx). */
export const SolxWidgetProvider = SolxWidgetContext.Provider;

/**
 * Read the scoped client the widget's host handed it at mount time (see
 * host/mountWidget.ts). `undefined` if the widget is rendered outside a
 * real mount (tests, Storybook) or the host didn't supply one -- callers
 * are expected to handle absence, not treat it as an error.
 */
export function useSolxWidgetClient(): WidgetClient | undefined {
  return useContext(SolxWidgetContext);
}
