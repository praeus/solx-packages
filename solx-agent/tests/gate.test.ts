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

  test("a glob reaches a script action but never a command", () => {
    const allow = [{ path: "/tools" }];
    const script = { path: "/tools", name: "a", actionType: "script" };
    const command = { path: "/tools", name: "sh", actionType: "command" };

    expect(permitted(script, allow)).toBe(true);
    expect(permitted(command, allow)).toBe(false);
    expect(permitted(command, [{ path: "/tools", actions: ["sh"] }])).toBe(true);
    expect(permitted(command, [{ path: "/tools", actions: ["other"] }])).toBe(false);
  });

  /**
   * The newer half of the exact-name rule. solx-core now gates *where* an
   * outbound request may go (`allowed_base_urls`); this gates whether the
   * model may make one at all, which is a different question -- an operator
   * allowlist may hold hosts the agent should not reach on its own.
   */
  test("a glob never reaches /builtin/web either", () => {
    const web = { path: "/builtin/web", name: "http_request", actionType: "internal" };
    expect(permitted(web, [{ path: "/builtin/web" }])).toBe(false);
    expect(permitted(web, [{ path: "/builtin/*" }])).toBe(false);
    expect(permitted(web, [{ path: "/builtin/web", actions: ["http_request"] }])).toBe(true);

    const stream = { path: "/builtin/web/stream", name: "start", actionType: "internal" };
    expect(permitted(stream, [{ path: "/builtin/web" }])).toBe(false);
    expect(permitted(stream, [{ path: "/builtin/web/stream", actions: ["start"] }])).toBe(true);
  });

  test("hard denies hold whatever the grant says", () => {
    const wideOpen = [
      { path: "*", actions: ["get_secret", "entity_save_action", "start", "set_env"] },
    ];
    for (const [path, name] of [
      ["/builtin/secrets", "get_secret"],
      ["/builtin/action", "entity_save_action"],
      // The widget itself drives these, so the model must never reach them.
      ["/builtin/action", "start"],
      ["/builtin/env", "set_env"],
      ["/packages/solx-agent", "agent-widget"],
    ]) {
      expect(permitted({ path, name, actionType: "internal" }, wideOpen), path + "/" + name).toBe(
        false,
      );
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
