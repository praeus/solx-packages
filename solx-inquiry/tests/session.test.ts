/**
 * The read-path correction in `loadInquiry`.
 *
 * Running the package against a real server exposed the need for this: a run
 * abandoned mid-phase (a cancelled MCP call, a stopped `solx exec`) dies
 * inside an `await`, the write-path `catch` never runs, and the document keeps
 * saying `planning` forever. Inquiry has no `pending[]` resume the way the
 * agent does, so a live status read back from the document cannot be true --
 * whatever was running it is gone. The reader settles it, and writes the
 * correction back so the stored document stops lying too.
 */

import { describe, expect, test } from "vitest";
import { loadInquiry, saveInquiry } from "../src/harness/session";
import { fake } from "./fakeHost";
import type { InquirySession } from "../src/harness";

function session(overrides: Partial<InquirySession> = {}): InquirySession {
  return {
    id: "live-test",
    model: "qwen3:4b",
    title: "t",
    status: "idle",
    options: {
      max_docs: 12,
      max_plan_steps: 4,
      doc_budget: 18000,
      include_actions: false,
      replan: true,
      chat_timeout_secs: null,
    },
    turns: [],
    answer: null,
    ...overrides,
  };
}

describe("loadInquiry", () => {
  test("leaves a settled session untouched", async () => {
    const { host, fake: f } = fake();
    await saveInquiry(host, "live-test", session({ status: "idle", answer: "done" }));
    const read = await loadInquiry(host, "live-test");
    expect(read.status).toBe("idle");
    expect(read.answer).toBe("done");
    void f;
  });

  test("closes out a session a killed process left live", async () => {
    const { fake: f, host } = fake();
    const interrupted = session({
      status: "planning",
      turns: [
        {
          question: "q",
          status: "planning",
          plan: [],
          plan_error: null,
          corpus: "",
          steps: [],
          documents: [],
          answer: null,
        },
      ],
    });
    await saveInquiry(host, "live-test", interrupted);

    const read = await loadInquiry(host, "live-test");
    expect(read.status).toBe("error");
    expect(read.turns[0].status).toBe("error");
    expect(read.turns[0].plan_error).toBe("interrupted");

    // The correction was written back, so the *stored* document is honest too.
    const stored = f.inquiry("live-test") as unknown as InquirySession;
    expect(stored.status).toBe("error");
    expect(stored.turns[0].status).toBe("error");
  });

  test("closes each of the three live phases the same way", async () => {
    for (const status of ["planning", "gathering", "answering"] as const) {
      const { host } = fake();
      await saveInquiry(
        host,
        "live-test",
        session({
          status,
          turns: [
            {
              question: "q",
              status,
              plan: [],
              plan_error: null,
              corpus: "",
              steps: [],
              documents: [],
              answer: null,
            },
          ],
        }),
      );
      const read = await loadInquiry(host, "live-test");
      expect(read.status, status).toBe("error");
      expect(read.turns[0].status, status).toBe("error");
    }
  });

  test("does not touch a settled turn list", async () => {
    const { host } = fake();
    await saveInquiry(
      host,
      "live-test",
      session({
        status: "idle",
        turns: [
          {
            question: "q",
            status: "idle",
            plan: [{ verb: "search", q: "x" }],
            plan_error: null,
            corpus: "",
            steps: [],
            documents: [],
            answer: "an answer",
          },
        ],
      }),
    );
    const read = await loadInquiry(host, "live-test");
    expect(read.turns[0].plan_error).toBeNull();
    expect(read.turns[0].answer).toBe("an answer");
  });

  test("leaves a recently written live session alone -- it may be running elsewhere", async () => {
    const { fake: f, host } = fake();
    await saveInquiry(host, "live-test", session({ status: "answering" }));
    const now = Date.parse("2026-10-13T16:30:00Z");
    (f.docs.get("/inquiry/sessions/live-test") as unknown as { updatedAt: string }).updatedAt =
      "2026-10-13T16:26:03.320854700Z";
    const read = await loadInquiry(host, "live-test", now);
    expect(read.status).toBe("answering");

    const later = await loadInquiry(host, "live-test", now + 30 * 60 * 1000);
    expect(later.status).toBe("error");
  });
});
