/**
 * Driving a session to a quiescent state.
 *
 * It steps one iteration at a time and reports after each, rather than
 * looping internally and returning once. That gives a render per iteration,
 * lets Stop land between iterations, and keeps the caller in control -- the
 * step is the primitive, this is the convenience wrapper over it.
 *
 * Deliberately free of React so the loop can be tested without a DOM.
 */

import { loadSession } from "./session";
import { step } from "./turn";
import type { Host } from "./host";
import type { Session, StepResult } from "./types";

export interface DriveHandlers {
  /** After every step, with the session as it now stands. */
  onProgress(result: StepResult, session: Session): void;
}

export interface DriveOptions {
  /** Checked between iterations; stops the loop without cancelling anything. */
  isAbandoned?: () => boolean;
}

/**
 * Step until quiescent. The loop stops on anything that needs a human: an
 * approval, an answer, a question, a cap, a failure streak, or abandonment.
 */
export async function driveSession(
  host: Host,
  session: Session,
  seed: StepResult,
  handlers: DriveHandlers,
  opts: DriveOptions = {},
): Promise<StepResult> {
  let result = seed;
  let current = session;
  handlers.onProgress(result, current);

  while (result.status === "running") {
    if (opts.isAbandoned?.()) return result;
    result = await step(host, current, undefined);
    handlers.onProgress(result, current);
  }
  return result;
}

/**
 * Resume a suspended turn. Anything not named is denied -- an empty list
 * denies everything, which is the safe direction to fail and is exactly what
 * "Deny all" means.
 */
export async function approveAndContinue(
  host: Host,
  session: Session,
  approve: string[],
  handlers: DriveHandlers,
  opts: DriveOptions = {},
): Promise<StepResult> {
  const resumed = await step(host, session, approve);
  return driveSession(host, session, resumed, handlers, opts);
}

/**
 * Re-read a session from the document store.
 *
 * Used when opening a thread from history, and after another client (or a
 * crashed tab) may have advanced it -- the document is the read model, not
 * anything held in this page.
 */
export async function readSession(host: Host, id: string): Promise<Session> {
  return loadSession(host, id);
}
