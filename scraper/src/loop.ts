import type { OpenPosition } from "./botApi.js";

/**
 * Coins eligible to scan this cycle: in the watchlist, with no open position,
 * not in cooldown, capped so open.length + result.length <= maxOpen.
 * Coins are compared upper-cased; `cooldownUntil` keys MUST be stored upper-cased
 * by the caller (Task 6's cooldown() helper does this).
 */
export function freeCoins(
  watchlist: string[], open: OpenPosition[], cooldownUntil: Map<string, number>,
  now: number, maxOpen: number,
): string[] {
  const held = new Set(open.map((p) => p.coin.toUpperCase()));
  const slots = Math.max(0, maxOpen - open.length);
  const eligible = watchlist
    .map((c) => c.toUpperCase())
    .filter((c) => !held.has(c))
    .filter((c) => (cooldownUntil.get(c) ?? 0) <= now);
  return eligible.slice(0, slots);
}

export function passesSlippage(mark: number, entry: number, maxDeviation: number): boolean {
  if (!Number.isFinite(mark) || !Number.isFinite(entry) || entry === 0) return false;
  return Math.abs(mark - entry) / entry <= maxDeviation;
}
