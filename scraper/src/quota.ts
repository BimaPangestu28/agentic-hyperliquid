import { readFileSync, writeFileSync } from "node:fs";

/** Persisted daily counter of Neurobro analyses spent (each chart analysis = 1 heavy). */
export interface QuotaState {
  date: string; // YYYY-MM-DD this count belongs to
  count: number;
}

/** UTC date key for the given epoch seconds. */
export function dayKey(epochSeconds: number): string {
  return new Date(epochSeconds * 1000).toISOString().slice(0, 10);
}

/** Reads the persisted counter; resets to 0 if it belongs to an earlier day or is missing/corrupt. */
export function readQuota(path: string, today: string): QuotaState {
  try {
    const parsed = JSON.parse(readFileSync(path, "utf8")) as Partial<QuotaState>;
    if (parsed.date === today && typeof parsed.count === "number") {
      return { date: today, count: parsed.count };
    }
  } catch {
    // missing or corrupt → start fresh for today
  }
  return { date: today, count: 0 };
}

/** Analyses still allowed today (never negative). */
export function remaining(state: QuotaState, maxPerDay: number): number {
  return Math.max(0, maxPerDay - state.count);
}

/** Records one spent analysis for today and persists it; returns the updated state. */
export function recordAnalysis(path: string, today: string): QuotaState {
  const current = readQuota(path, today);
  const next: QuotaState = { date: today, count: current.count + 1 };
  try {
    writeFileSync(path, JSON.stringify(next));
  } catch {
    // best-effort persistence; an unwritable path must not crash the loop
  }
  return next;
}
