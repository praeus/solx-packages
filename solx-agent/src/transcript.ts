import type { CallRecord, Message, Session, ToolCall } from "./harness";

/**
 * Turns a session's flat `messages[]` into the turn blocks the UI renders.
 *
 * The harness stores a transcript, not a conversation tree: one array of
 * system/user/assistant/tool turns, plus a parallel `calls[]` audit log. The
 * shape a reader wants is nested -- a user message, the iterations it
 * caused, the calls each iteration made, and how each one turned out -- so
 * the nesting is reconstructed here rather than rendered flat.
 *
 * Two invariants from `completeTurn` make this reliable: every pending call
 * gets exactly one `role:"tool"` turn, and `messages` and `calls` are
 * appended in lockstep in the same order. So results pair to calls by
 * position, and one monotonic cursor into `calls[]` stays aligned.
 */

export interface RenderedCall {
  name: string;
  ref: string | null;
  arguments: Record<string, unknown>;
  /** The tool turn's content: a JSON result, or an error/refusal string. */
  result: string | null;
  outcome: CallRecord["outcome"] | null;
  approvedBy: string | null;
}

export interface RenderedIteration {
  /** 1-based, within the turn -- what the user counts, not the session total. */
  index: number;
  /** Anything the model said alongside its calls. Often empty. */
  narration: string;
  calls: RenderedCall[];
}

export interface RenderedTurn {
  key: string;
  /** Absent for a transcript that somehow starts without a user turn. */
  user: string | null;
  /** Seed material and any mid-thread context additions, kept out of the flow. */
  systemNotes: string[];
  iterations: RenderedIteration[];
  /** The model's closing text for this turn: it answered, or it asked. */
  answer: string | null;
}

function toRenderedCall(call: ToolCall): RenderedCall {
  return {
    name: call.function?.name ?? "(unnamed)",
    ref: null,
    arguments: call.function?.arguments ?? {},
    result: null,
    outcome: null,
    approvedBy: null,
  };
}

export function buildTurns(messages: Message[], calls: CallRecord[]): RenderedTurn[] {
  const turns: RenderedTurn[] = [];
  let pendingSystem: string[] = [];
  let callCursor = 0;

  const currentTurn = (): RenderedTurn => {
    if (turns.length === 0) {
      // A transcript with no user turn yet still has to render its seed.
      turns.push({ key: "turn-0", user: null, systemNotes: pendingSystem, iterations: [], answer: null });
      pendingSystem = [];
    }
    return turns[turns.length - 1];
  };

  for (const message of messages) {
    if (message.role === "system") {
      pendingSystem.push(message.content);
      continue;
    }

    if (message.role === "user") {
      turns.push({
        key: `turn-${turns.length}`,
        user: message.content,
        systemNotes: pendingSystem,
        iterations: [],
        answer: null,
      });
      pendingSystem = [];
      continue;
    }

    if (message.role === "assistant") {
      const turn = currentTurn();
      const toolCalls = message.tool_calls ?? [];
      if (toolCalls.length === 0) {
        // No calls is how the loop ends a turn -- whether that is an answer
        // or a question is not something to guess at, so it is neither
        // labelled nor treated differently.
        turn.answer = message.content ?? "";
        continue;
      }
      turn.iterations.push({
        index: turn.iterations.length + 1,
        narration: message.content ?? "",
        calls: toolCalls.map(toRenderedCall),
      });
      continue;
    }

    // A tool result closes the next call still waiting for one.
    const turn = currentTurn();
    const iteration = turn.iterations[turn.iterations.length - 1];
    const target = iteration?.calls.find((c) => c.result === null);
    const record = calls[callCursor];
    callCursor += 1;
    if (!target) continue;
    target.result = message.content;
    if (record) {
      target.ref = record.ref;
      target.outcome = record.outcome;
      target.approvedBy = record.approved_by;
    }
  }

  // Trailing system turns (context added by `say` before the next message is
  // sent) would otherwise be dropped.
  if (pendingSystem.length > 0) {
    turns.push({
      key: `turn-${turns.length}`,
      user: null,
      systemNotes: pendingSystem,
      iterations: [],
      answer: null,
    });
  }

  return turns;
}

export function buildTurnsFrom(session: Session): RenderedTurn[] {
  return buildTurns(session.messages ?? [], session.calls ?? []);
}

/** One-line recap for a collapsed turn. */
export function summariseTurn(turn: RenderedTurn): string {
  const callCount = turn.iterations.reduce((n, it) => n + it.calls.length, 0);
  const approved = turn.iterations.reduce(
    (n, it) => n + it.calls.filter((c) => c.approvedBy).length,
    0,
  );
  if (callCount === 0) return "no tools used";
  const iterations = turn.iterations.length;
  const parts = [
    `${callCount} ${callCount === 1 ? "call" : "calls"}`,
    `${iterations} ${iterations === 1 ? "iteration" : "iterations"}`,
  ];
  if (approved > 0) parts.push(`${approved} approved`);
  return parts.join(" · ");
}
