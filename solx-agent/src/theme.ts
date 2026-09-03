/**
 * Design tokens, re-declared inside the widget's shadow root.
 *
 * The host page's global CSS does not cross a shadow boundary, so solx-web's
 * `--bg` / `--border` / `--text-muted` are simply not visible here. These
 * mirror the names and roles used in solx-web's App.css so the widget reads
 * as part of the same product, and they follow the viewer's colour scheme
 * the same way the rest of the app does.
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

button {
  font: inherit;
  color: var(--text);
  background: var(--bg-raised);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius);
  padding: 4px 10px;
  cursor: pointer;
}
button:hover:not(:disabled) { background: var(--bg-sunken); }
button:disabled { opacity: 0.5; cursor: default; }
button.primary {
  background: var(--accent);
  border-color: var(--accent);
  color: #fff;
}
button.primary:hover:not(:disabled) { filter: brightness(1.08); }

input, textarea, select {
  font: inherit;
  color: var(--text);
  background: var(--bg);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius);
  padding: 5px 8px;
  width: 100%;
}
textarea { resize: vertical; }

code, pre { font-family: var(--font-mono); font-size: 12px; }
pre {
  margin: 0;
  padding: 6px 8px;
  background: var(--bg-sunken);
  border-radius: var(--radius);
  overflow-x: auto;
  white-space: pre-wrap;
  word-break: break-word;
}

.muted { color: var(--text-muted); }
.faint { color: var(--text-faint); }
.row { display: flex; align-items: center; gap: 6px; }
.col { display: flex; flex-direction: column; }

.chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 1px 7px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--bg-raised);
  font-size: 11px;
  color: var(--text-muted);
  white-space: nowrap;
}
.chip.ok { color: var(--ok); border-color: color-mix(in srgb, var(--ok) 35%, var(--border)); }
.chip.warn { color: var(--warn); background: var(--warn-soft); border-color: color-mix(in srgb, var(--warn) 35%, var(--border)); }
.chip.danger { color: var(--danger); background: var(--danger-soft); border-color: color-mix(in srgb, var(--danger) 35%, var(--border)); }
.chip.accent { color: var(--accent); background: var(--accent-soft); border-color: color-mix(in srgb, var(--accent) 35%, var(--border)); }
`;
