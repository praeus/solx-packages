/**
 * Live progress for a `multi_inquire` turn, read off the console it already
 * writes.
 *
 * There is no progress channel in solx — there is a console, and `solx-inquiry`
 * narrates each milestone to it with a machine-readable `data.ev` envelope
 * beside the human-readable message (see that package's `src/console.rs`).
 * So this directory is a parser (`events.ts`), a fold of those events into
 * renderable state (`reducer.ts`), and the words that state renders as
 * (`labels.ts`) — all pure, all testable without a browser. The tail loop that
 * feeds it is `../console/useConsoleFeed`.
 */
export { parseProgressEvent, legacyEventFromMessage } from "./events";
export type { ProgressEvent, Scope, IntentMode, FanoutMode } from "./events";
export { emptyProgress, foldProgress, reduceProgress, isSettled } from "./reducer";
export type { InquiryPhase, InquiryProgress, ProgressState, Stage } from "./reducer";
export {
  OUTCOME_LABEL,
  OUTCOME_CHIP,
  stageLabel,
  stageChipClass,
  isLive,
  inquiryChipLabel,
  inquiryChipClass,
  inquiryTooltip,
  phaseWord,
  degradedLabel,
} from "./labels";
export type { ChipClass, TurnOutcome } from "./labels";
