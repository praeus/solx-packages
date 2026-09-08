/** The gate: default-deny, structural denies, exact-name rules, reserved paths. */

import { describe, expect, test } from "vitest";
import {
  globMatches,
  isAtOrUnder,
  normalizeAllow,
  permitted,
  reservedDocWrite,
  validSegment,
} from "../src/harness/gate";

const SKILLS = { skills: { enabled: true, path: "/agent/skills", limit: 10 } };

describe("the grant", () => {
  test("is default-deny: absent and empty are errors, not empty catalogues", () => {
    for (const bad of [undefined, null, []]) {
      expect(() => normalizeAllow(bad as never)).toThrow(/default-deny/);
    }
    expect(() => normalizeAllow([{ path: "" }])).toThrow(/non-empty path/);
  });

  test("a glob reaches a script, command, or webhook action alike", () => {
    // There used to be a carve-out here requiring an exact `actions` name
    // for command/webhook rows, so a wide grant could not accidentally reach
    // a shell or an arbitrary host. Removed: it made a Command action
    // invisible to *discovery* (resolveCatalogue, sys__tool_search) as a
    // side effect of restricting *dispatch*, and every command/webhook row
    // is unconditionally destructive regardless (see isExecutableType and
    // turn.ts's gateCall), so a call still suspends for a human decision
    // with the full resolved ref and arguments shown before it runs.
    const allow = [{ path: "/tools" }];
    const script = { path: "/tools", name: "a", actionType: "script" };
    const command = { path: "/tools", name: "sh", actionType: "command" };
    const webhook = { path: "/tools", name: "hook", actionType: "webhook" };

    expect(permitted(script, allow)).toBe(true);
    expect(permitted(command, allow)).toBe(true);
    expect(permitted(webhook, allow)).toBe(true);
    expect(permitted(command, [{ path: "/tools", actions: ["sh"] }])).toBe(true);
    expect(permitted(command, [{ path: "/tools", actions: ["other"] }])).toBe(false);
  });

  test("a glob reaches /builtin/web too, gated only by hard denies and solx-core's own allowed_base_urls", () => {
    const web = { path: "/builtin/web", name: "http_request", actionType: "internal" };
    expect(permitted(web, [{ path: "/builtin/web" }])).toBe(true);
    expect(permitted(web, [{ path: "/builtin/*" }])).toBe(true);

    // A non-wildcard grant path is still only ever an exact match to a row's
    // own path -- that is ordinary globMatches behavior, unrelated to this
    // change. A `*` is what reaches a nested child like the stream actions.
    const stream = { path: "/builtin/web/stream", name: "start", actionType: "internal" };
    expect(permitted(stream, [{ path: "/builtin/web/stream" }])).toBe(true);
    expect(permitted(stream, [{ path: "/builtin/web/*" }])).toBe(true);
  });

  test("hard denies hold whatever the grant says", () => {
    const wideOpen = [
      {
        path: "*",
        actions: [
          "get_secret",
          "entity_save_action",
          "entity_delete_action",
          "start",
          "stop",
          "poll",
          "cancelled",
          "set_env",
          "agent-widget",
        ],
      },
    ];
    for (const [path, name] of [
      ["/builtin/secrets", "get_secret"],
      // A script or wasm row registered under an already-granted path would
      // run outside this session's grant entirely.
      ["/builtin/action", "entity_save_action"],
      ["/builtin/action", "entity_delete_action"],
      // The widget itself drives these, so the model must never reach them.
      ["/builtin/action", "start"],
      ["/builtin/action", "stop"],
      ["/builtin/action", "poll"],
      ["/builtin/action", "cancelled"],
      ["/builtin/env", "set_env"],
      ["/packages/solx-agent", "agent-widget"],
    ]) {
      expect(permitted({ path, name, actionType: "internal" }, wideOpen), path + "/" + name).toBe(
        false,
      );
    }
  });

  /**
   * The read half of /builtin/action is the point of narrowing the deny from
   * a blanket glob to exact names: the registry *is* the tool catalogue, so
   * this is how a model looks up a tool it was not handed and reads that
   * tool's parameter schema.
   */
  test("the catalogue readers survive the narrowed deny", () => {
    const grant = [
      {
        path: "/builtin/action",
        actions: ["search_actions", "entity_get_action", "entity_list_actions"],
      },
    ];
    for (const name of ["search_actions", "entity_get_action", "entity_list_actions"]) {
      expect(
        permitted({ path: "/builtin/action", name, actionType: "internal" }, grant),
        name,
      ).toBe(true);
    }
    // Same grant, and still refused: these are denied by name, not by absence.
    for (const name of ["entity_save_action", "start"]) {
      expect(
        permitted({ path: "/builtin/action", name, actionType: "internal" }, grant),
        name,
      ).toBe(false);
    }
  });
});

describe("reserved document paths", () => {
  const SAVE = "/builtin/document/entity_save_document";
  const AT_PATH = "/builtin/document/set_field_at_path";

  test("are refused by whichever param names them", () => {
    expect(reservedDocWrite(SKILLS, SAVE, { path: "/agent/sessions" })).toMatch(/agent-owned/);
    expect(reservedDocWrite(SKILLS, SAVE, { path: "/agent/memories/x" })).toMatch(/agent-owned/);
    expect(reservedDocWrite(SKILLS, SAVE, { path: "/elsewhere" })).toBeNull();

    // The trap: set_field_at_path names the entity as `doc_path`; its `path`
    // is a JSON pointer into contents. Reading the wrong key would disable
    // the check for exactly the call that rewrites one field of a session.
    expect(
      reservedDocWrite(SKILLS, AT_PATH, { doc_path: "/agent/sessions", path: "/status" }),
    ).toMatch(/agent-owned/);
    expect(reservedDocWrite(SKILLS, AT_PATH, { path: "/agent/sessions" })).toBeNull();
  });

  test("the check is case-insensitive and survives a trailing slash", () => {
    expect(reservedDocWrite(SKILLS, SAVE, { path: "/AGENT/sessions" })).toMatch(/agent-owned/);
    expect(reservedDocWrite(SKILLS, SAVE, { path: "/agent/sessions/" })).toMatch(/agent-owned/);
    expect(reservedDocWrite(SKILLS, SAVE, { path: "agent/sessions" })).toMatch(/agent-owned/);
  });

  test("the default skills path stays reserved even when skills point elsewhere", () => {
    const elsewhere = { skills: { enabled: true, path: "/house/skills", limit: 10 } };
    expect(reservedDocWrite(elsewhere, SAVE, { path: "/agent/skills" })).toMatch(/agent-owned/);
    expect(reservedDocWrite(elsewhere, SAVE, { path: "/house/skills" })).toMatch(/agent-owned/);
  });

  test("a non-writer action is not gated by this check at all", () => {
    expect(reservedDocWrite(SKILLS, "/builtin/document/search_documents", { path: "/agent" })).toBeNull();
  });
});

describe("path helpers", () => {
  test("glob matches across slashes, and ? matches one character", () => {
    expect(globMatches("/a/*", "/a/b/c")).toBe(true);
    expect(globMatches("/a/?", "/a/b")).toBe(true);
    expect(globMatches("/a/?", "/a/bc")).toBe(false);
    expect(globMatches("/a", "/ab")).toBe(false);
  });

  test("isAtOrUnder does not match a sibling with a shared prefix", () => {
    expect(isAtOrUnder("/agent", "/agent")).toBe(true);
    expect(isAtOrUnder("/agent/sessions", "/agent")).toBe(true);
    expect(isAtOrUnder("/agentic", "/agent")).toBe(false);
  });

  test("a memory scope must be one path segment", () => {
    // Exactly solx_surface::path::validate_segment: empty, . and .., the
    // three separators, and control characters. A space is legal there, so
    // it is legal here.
    const nul = String.fromCharCode(0);
    for (const bad of ["", ".", "..", "a/b", "a\\b", "c:", "a" + nul + "b"]) {
      expect(validSegment(bad), JSON.stringify(bad)).toBe(false);
    }
    for (const good of ["proj", "my-project", "release_2026", "a b"]) {
      expect(validSegment(good), good).toBe(true);
    }
  });
});
