# Plan: MCP Server as an Action Type (NOT IMPLEMENTED)

## Context

Sol's action system dispatches execution by `action_type` (WASM, Prompt, Actions, Webhook, Command). The user wants to explore adding **MCP (Model Context Protocol) server** as a new type, surfacing MCP tools, resources, and prompts as Sol actions. The codebase has no existing MCP infrastructure — zero matches for "mcp", "tool_call", or "model_context" — so this would be a net-new addition.

MCP is a rapidly growing ecosystem. Every MCP server (filesystem, database, GitHub, browser, Puppeteer, custom) becomes an action in Sol with no WASM authoring required. Sol's instruction pipeline already searches and invokes actions by name; MCP tools slot in naturally.

---

## Complexity vs. Value Assessment

### Value (High)
- Instant access to every MCP-compatible tool without writing WASM
- Sol's planning pipeline can call MCP tools the same way it calls any action
- Connects Sol to hundreds of community MCP servers (filesystem, git, databases, APIs, browser automation)
- Resources map to artifacts; prompts map to instruction artifacts — full MCP surface becomes usable

### Complexity
| Phase | Scope | Effort |
|---|---|---|
| **Phase 1 – stdio tools** | Spawn process, JSON-RPC tools/call | ~2 days |
| **Phase 2 – HTTP/SSE tools** | Connect to running SSE server | +1 day |
| **Phase 3 – resources + prompts** | MCP resources → artifacts, prompts → instructions | +2 days |
| **Full pool/discovery** | Connection reuse, auto-import UI | +1 week |

**Recommendation:** Ship Phase 1 (stdio, tools only). It covers ~80% of the ecosystem value with minimal risk because the MCP protocol for `tools/call` over stdio is simple JSON-RPC 2.0 — no external SDK needed.

---

## Architecture

### Field Convention (reuses existing Action fields)
| Action field | MCP meaning |
|---|---|
| `action_type` | `"Mcp"` |
| `bin_name` | stdio: shell command string (e.g. `npx -y @modelcontextprotocol/server-filesystem /tmp`); HTTP: base URL (e.g. `http://localhost:3000`) |
| `fn_name` | MCP tool name to invoke (e.g. `read_file`) |
| `parameters_in` | Human description (auto-fill from MCP tool schema) |

### Execution (stdio transport, Phase 1)
1. Split `bin_name` into program + args via shell-words parsing
2. Spawn subprocess; capture stdin/stdout
3. Write `initialize` request (JSON-RPC 2.0, `\n`-delimited)
4. Write `notifications/initialized`
5. Write `tools/call` request with `fn_name` + `message` as arguments
6. Read response line, parse `result.content[*].text` or `result.content[*].data`
7. Kill subprocess; return `ActionExecResult`

No `rmcp` crate needed for Phase 1. The protocol is trivially hand-coded (~120 lines).

---

## Files to Modify

### Backend

**`sol-manager/src/mcp_dispatch.rs`** (new file, ~150 lines)
- `async fn dispatch_mcp_stdio(action_name, command, tool_name, params) -> Result<ActionExecResult, String>`
- JSON-RPC 2.0 initialization + `tools/call` over subprocess stdin/stdout
- Parse `CallToolResult` content array → JSON `Value`

**`sol-manager/src/manager.rs`** — `execute_action_once()` at line 1855
```rust
"Mcp" => {
    let command = action.bin_name.as_deref().ok_or("Mcp action requires bin_name (server command or URL)")?;
    let tool = action.fn_name.as_deref().ok_or("Mcp action requires fn_name (tool name)")?;
    return dispatch_mcp_stdio(entity_name, command, tool, message).await;
}
```

Add `mod mcp_dispatch;` and `use mcp_dispatch::dispatch_mcp_stdio;` at top of manager.rs.

**`sol-manager/Cargo.toml`**
- No new dependencies for Phase 1 (uses `tokio::process::Command` already present)
- Optionally add `shell-words = "1"` for robust command splitting (or split naively on whitespace)

### Frontend

**`sol-browser/src/components/ActionEditor.tsx`**

1. Add `"Mcp"` option to the `<select>` at line 516:
   ```tsx
   <option value="Mcp">MCP — call a tool on an MCP server</option>
   ```

2. Update the "Exec Artifact" field condition at line 531 to include `actionType === "Mcp"` — rename label to "MCP Server Command or URL".

3. Update the "Invoke Resource" field condition at line 557 to include `actionType === "Mcp"` — rename label to "MCP Tool Name".

4. Add a hint row when `actionType === "Mcp"`:
   ```
   bin_name hint: "e.g. npx -y @modelcontextprotocol/server-filesystem /path"
   fn_name hint: "e.g. read_file"
   ```

No API or bindings changes needed — `actionExec` already passes through to the same dispatch chain.

---

## JSON-RPC Wire Protocol (Phase 1 reference)

```jsonc
// → stdin
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"sol","version":"0.1.0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"<fn_name>","arguments":<message>}}

// ← stdout
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{...},"serverInfo":{...}}}
{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"..."}]}}
```

Error cases: `error` field present instead of `result`; content type may be `"resource"` (ignore in Phase 1).

---

## Phase 2 additions (future, low priority)

- **HTTP/SSE transport**: detect `bin_name` starts with `http` → POST to `{bin_name}/mcp/v1/tools/call`, parse SSE stream
- **Resources**: `fn_name` prefix `resource://` → `resources/read` method → store response as artifact
- **Prompts**: `fn_name` prefix `prompt://` → `prompts/get` → pass to instruction pipeline
- **Discovery action**: a built-in action `mcp-list-tools` that takes `bin_name` and returns all tool definitions, which can then be auto-imported as Sol actions

---

## Recommended Test Servers

These are well-maintained, installable via `npx` (no local install needed), and cover a useful range:

| Server | Install command | Good tools to test |
|---|---|---|
| **Filesystem** | `npx -y @modelcontextprotocol/server-filesystem /path/to/dir` | `read_file`, `write_file`, `list_directory` |
| **Memory** | `npx -y @modelcontextprotocol/server-memory` | `create_entities`, `search_nodes` |
| **Fetch** | `npx -y @modelcontextprotocol/server-fetch` | `fetch` (HTTP GET → markdown) |
| **Git** | `npx -y @modelcontextprotocol/server-git --repository /path/to/repo` | `git_log`, `git_diff`, `git_status` |
| **SQLite** | `npx -y @modelcontextprotocol/server-sqlite --db-path /path/to/db.sqlite` | `read_query`, `write_query` |
| **Puppeteer** | `npx -y @modelcontextprotocol/server-puppeteer` | `puppeteer_navigate`, `puppeteer_screenshot` |

**Simplest smoke test:** Filesystem server — requires only Node.js, no credentials, tools are predictable, and return values are plain text that's easy to assert on.

---

## Verification

1. Create an action: `name="fs-read"`, `action_type="Mcp"`, `bin_name="npx -y @modelcontextprotocol/server-filesystem /tmp"`, `fn_name="read_file"`
2. In ActionEditor "Run Params", supply `{"path": "/tmp/test.txt"}`
3. Click **Run Action** — should return file contents
4. Verify error case: bad tool name → `editor-error` shows MCP error message
5. Run `sol-cli exec fs-read < params.json` to confirm CLI path works identically
