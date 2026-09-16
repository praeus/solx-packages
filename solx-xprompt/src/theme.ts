/**
 * Design tokens, re-declared inside the widget's shadow root.
 *
 * The host page's global CSS does not cross a shadow boundary, so solx-web's
 * `--bg` / `--border` / `--text-muted` are simply not visible here. These
 * mirror the names and roles used in solx-web's App.css (and solx-agent's
 * own copy) so the widget reads as part of the same product, and they
 * follow the viewer's colour scheme the same way the rest of the app does.
 */
export const WIDGET_STYLES = `
:host {
  --bg: #ffffff;
  --bg-raised: #f7f7f8;
  --bg-sunken: #f0f0f2;
  --border: #e2e2e5;
  --border-strong: #cfcfd4;
  --text: #17171a;
  --text-muted: #6b6b75;
  --text-faint: #9a9aa4;
  --accent: #2563eb;
  --accent-soft: #e8effd;
  --danger: #c0392b;
  --danger-soft: #fdecea;
  --warn: #b7791f;
  --warn-soft: #fdf6e3;
  --ok: #16a34a;
  --radius: 6px;
  --font-mono: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;

  display: block;
  color: var(--text);
  font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
  font-size: 13px;
  line-height: 1.45;
}

@media (prefers-color-scheme: dark) {
  :host {
    --bg: #17171a;
    --bg-raised: #1f1f24;
    --bg-sunken: #121215;
    --border: #2e2e35;
    --border-strong: #3d3d46;
    --text: #ececf1;
    --text-muted: #9a9aa4;
    --text-faint: #6b6b75;
    --accent: #6b9bff;
    --accent-soft: #1b2740;
    --danger: #f0836f;
    --danger-soft: #3a1f1b;
    --warn: #e0b45e;
    --warn-soft: #332a15;
    --ok: #56c98a;
  }
}

* { box-sizing: border-box; }

.muted { color: var(--text-muted); }
.faint { color: var(--text-faint); }
.row { display: flex; align-items: center; gap: 6px; }
.col { display: flex; flex-direction: column; }
`;
