import { describe, it, expect } from "vitest";
import { validateSetup, type Setup } from "./extract.js";

const OPTIONS = { minRiskReward: 1.5, maxStopLossPct: 0.03 };

/** A valid LONG with RR=2 and a 1% stop, as a base to mutate per case. */
function longSetup(overrides: Partial<Setup> = {}): Setup {
  return {
    coin: "BTC", direction: "long",
    entry: 100, stopLoss: 99, takeProfit: 102,
    confidence: 8, thesis: "", ...overrides,
  };
}

describe("validateSetup", () => {
  it("accepts a well-ordered LONG meeting RR and stop bounds", () => {
    expect(validateSetup(longSetup(), OPTIONS)).toEqual({ ok: true });
  });

  it("accepts a well-ordered SHORT", () => {
    const setup = longSetup({ direction: "short", entry: 100, stopLoss: 101, takeProfit: 98 });
    expect(validateSetup(setup, OPTIONS)).toEqual({ ok: true });
  });

  it("rejects a LONG whose TP is below entry (wrong ordering)", () => {
    const result = validateSetup(longSetup({ takeProfit: 99.5 }), OPTIONS);
    expect(result.ok).toBe(false);
  });

  it("rejects a SHORT whose SL is below entry (wrong ordering)", () => {
    const setup = longSetup({ direction: "short", entry: 100, stopLoss: 99, takeProfit: 98 });
    expect(validateSetup(setup, OPTIONS).ok).toBe(false);
  });

  it("rejects reward:risk below the minimum", () => {
    // risk = 1 (100→99), reward = 1 (100→101) → RR 1.0 < 1.5
    const result = validateSetup(longSetup({ takeProfit: 101 }), OPTIONS);
    expect(result).toEqual({ ok: false, reason: "RR 1.00 < 1.5" });
  });

  it("rejects a stop distance wider than the cap", () => {
    // stop = 95 → 5% > 3%; keep RR valid (TP far enough)
    const result = validateSetup(longSetup({ stopLoss: 95, takeProfit: 110 }), OPTIONS);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toContain("SL");
  });

  it("rejects when stop loss equals entry", () => {
    expect(validateSetup(longSetup({ stopLoss: 100 }), OPTIONS).ok).toBe(false);
  });

  it("rejects non-finite prices", () => {
    expect(validateSetup(longSetup({ entry: NaN }), OPTIONS).ok).toBe(false);
  });
});
