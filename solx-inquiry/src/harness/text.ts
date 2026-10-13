/**
 * Small text and number helpers shared by the phases.
 *
 * Kept separate from `plan.ts`/`execute.ts` so both can use them without one
 * importing the other, and so the truncation rules live in exactly one place:
 * how text is cut matters here, because every cut is a document silently
 * contributing less than it holds.
 */

/** Clamp a possibly-absent number into `[lo, hi]`, flooring it. */
export function clamp(n: unknown, lo: number, hi: number): number {
  if (typeof n !== "number" || !isFinite(n)) return lo;
  return Math.max(lo, Math.min(hi, Math.floor(n)));
}

/**
 * Cut a string to `n` characters, saying so when it is cut.
 *
 * The marker is not decoration: a model given a truncated document with no
 * signal will treat the cut as the end of the text and answer as if that were
 * all there was. Saying how many characters were dropped lets it say "the
 * document continues" instead of guessing.
 */
export function truncate(s: unknown, n: number): string {
  const str = String(s ?? "");
  if (str.length <= n) return str;
  return str.slice(0, n) + "\n[truncated, " + (str.length - n) + " more characters]";
}

/** A cheap, stable hash for de-duplication within one turn, never for storage. */
export function hash(s: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h = (h ^ s.charCodeAt(i)) >>> 0;
    h = (h * 0x01000193) >>> 0;
  }
  return ("0000000" + h.toString(16)).slice(-8);
}

/** Does `path` equal or sit under `prefix`? Mirrors the store's prefix match. */
export function underPrefix(path: string, prefix?: string | null): boolean {
  if (!prefix || prefix === "/") return true;
  return path === prefix || path.startsWith(prefix + "/");
}
