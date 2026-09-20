/**
 * The transcript renderer: human-readable tool labels and the recap summary.
 *
 * The harness stores the wire tool name (`act__…`) for dispatch, and a parallel
 * `tool_labels` map for display. These tests pin the contract that the label
 * reaches the renderer, and that the recap counts calls rather than narrating
 * the internal identifier.
 */

import { describe, expect, test } from "vitest";
import { buildTurns, summariseTurn } from "../src/transcript";
import type { Message, CallRecord } from "../src/harness";

describe("tool labels", () => {
  test("a call renders its caption, not the wire name", () => {
    const labels = { act__builtin__document__search_documents: "Search documents" };
    const messages: Message[] = [
      { role: "user", content: "find things" },
      {
        role: "assistant",
        content: "",
        tool_calls: [
          { function: { name: "act__builtin__document__search_documents", arguments: { q: "x" } } },
        ],
      },
      { role: "tool", tool_name: "act__builtin__document__search_documents", content: "{}" },
    ];
    const calls: CallRecord[] = [
      {
        iteration: 1,
        name: "act__builtin__document__search_documents",
        ref: "/builtin/document/search-documents",
        outcome: "ok",
        approved_by: null,
      },
    ];

    const turns = buildTurns(messages, calls, labels);
    const call = turns[0].iterations[0].calls[0];
    expect(call.label).toBe("Search documents");
    expect(call.name).toBe("act__builtin__document__search_documents");
  });

  test("a call with no caption falls back to the wire name", () => {
    const turns = buildTurns(
      [
        { role: "user", content: "go" },
        {
          role: "assistant",
          content: "",
          tool_calls: [{ function: { name: "act__x__y", arguments: {} } }],
        },
        { role: "tool", tool_name: "act__x__y", content: "{}" },
      ],
      [{ iteration: 1, name: "act__x__y", ref: "/x/y", outcome: "ok", approved_by: null }],
      {},
    );
    const call = turns[0].iterations[0].calls[0];
    expect(call.label).toBe("act__x__y");
  });
});

describe("summariseTurn", () => {
  test("counts calls and iterations, never naming an identifier", () => {
    const messages: Message[] = [
      { role: "user", content: "do it" },
      {
        role: "assistant",
        content: "",
        tool_calls: [
          { function: { name: "act__a__b", arguments: {} } },
          { function: { name: "act__a__c", arguments: {} } },
        ],
      },
      { role: "tool", tool_name: "act__a__b", content: "{}" },
      { role: "tool", tool_name: "act__a__c", content: "{}" },
    ];
    const calls: CallRecord[] = [
      { iteration: 1, name: "act__a__b", ref: "/a/b", outcome: "ok", approved_by: null },
      { iteration: 1, name: "act__a__c", ref: "/a/c", outcome: "ok", approved_by: null },
    ];
    const turns = buildTurns(messages, calls, {});
    const summary = summariseTurn(turns[0]);
    expect(summary).toContain("2 calls");
    expect(summary).toContain("1 iteration");
  });
});
