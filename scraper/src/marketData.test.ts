import { describe, it, expect } from "vitest";
import { computeAtrPercent } from "./marketData.js";

/** Builds `count` candles each with a constant high-low range and close, prevClose=close. */
function flatCandles(count: number, price: number, range: number) {
  return Array.from({ length: count }, () => ({
    h: String(price + range / 2),
    l: String(price - range / 2),
    c: String(price),
  }));
}

describe("computeAtrPercent", () => {
  it("returns the true range as a percent of close for flat candles", () => {
    // Each TR = high-low = 2 (range), close = 100 → ATR% = 2%.
    const atr = computeAtrPercent(flatCandles(20, 100, 2), 14);
    expect(atr).toBeCloseTo(2, 6);
  });

  it("returns NaN when there are too few candles", () => {
    expect(Number.isNaN(computeAtrPercent(flatCandles(5, 100, 2), 14))).toBe(true);
  });

  it("returns NaN on a malformed (non-numeric) candle", () => {
    const candles = flatCandles(20, 100, 2);
    candles[19] = { h: "abc", l: "x", c: "y" };
    expect(Number.isNaN(computeAtrPercent(candles, 14))).toBe(true);
  });
});
