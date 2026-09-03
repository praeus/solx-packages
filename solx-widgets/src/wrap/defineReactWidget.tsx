import type { ComponentType } from "react";
import { createRoot, type Root } from "react-dom/client";
import type { WidgetClient } from "../shared/widgetClient";
import { SolxWidgetProvider } from "./SolxWidgetContext";

/**
 * Registers `tagName` as a custom element that mounts `Component` into a
 * shadow root (style isolation -- the bundle's CSS never leaks into the
 * host page, and the host page's styles never leak in) and re-renders it
 * whenever `.fields` is assigned. This is the widget-author-facing half of
 * the contract in solx-core/docs/widget-actions.md -- a widget's build
 * entry point calls this once, e.g.:
 *
 *   defineReactWidget("solx-agent-widget", AgentWidget);
 *
 * No-op if the tag is already registered (double-import safety, since
 * mountWidget dedupes by bin_name but a page could still end up importing
 * the same bundle twice via two different code paths).
 */
export interface ReactWidgetOptions {
  /**
   * CSS for the widget's shadow root. A shadow boundary is exactly what
   * keeps the host's styles out, which also means the host's design tokens
   * are not visible in here -- a widget that wants any has to bring them.
   */
  styles?: string;
}

export function defineReactWidget<Fields = unknown>(
  tagName: string,
  Component: ComponentType<{ fields: Fields | undefined }>,
  options: ReactWidgetOptions = {},
): void {
  if (customElements.get(tagName)) return;

  class ReactWidgetElement extends HTMLElement {
    private root: Root | null = null;
    private currentFields: Fields | undefined;
    private currentClient: WidgetClient | undefined;

    connectedCallback(): void {
      const shadow = this.shadowRoot ?? this.attachShadow({ mode: "open" });
      if (options.styles && !shadow.querySelector("style[data-solx-widget]")) {
        const style = document.createElement("style");
        style.setAttribute("data-solx-widget", "");
        style.textContent = options.styles;
        // Outside the React root, so a re-render never touches it.
        shadow.appendChild(style);
      }
      const mount = document.createElement("div");
      shadow.appendChild(mount);
      this.root = createRoot(mount);
      this.render();
    }

    disconnectedCallback(): void {
      this.root?.unmount();
      this.root = null;
    }

    get fields(): Fields | undefined {
      return this.currentFields;
    }

    set fields(value: Fields | undefined) {
      this.currentFields = value;
      this.render();
    }

    get solxClient(): WidgetClient | undefined {
      return this.currentClient;
    }

    set solxClient(value: WidgetClient | undefined) {
      this.currentClient = value;
      this.render();
    }

    private render(): void {
      this.root?.render(
        <SolxWidgetProvider value={this.currentClient}>
          <Component fields={this.currentFields} />
        </SolxWidgetProvider>,
      );
    }
  }

  customElements.define(tagName, ReactWidgetElement);
}
