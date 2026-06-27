import { describe, it, expect } from "vitest";
import { rangeTabSelectors } from "../src/hyperliquid.js";

describe("rangeTabSelectors", () => {
  it("targets the TradingView date-range tab by data-name and value, case-insensitively", () => {
    const selectors = rangeTabSelectors("1D");
    expect(selectors).toContain('button[data-name="date-range-tab-1D" i]');
    expect(selectors).toContain('button[value="1D" i]');
  });
  it("trims whitespace around the range", () => {
    expect(rangeTabSelectors("  5D ")[0]).toBe('button[data-name="date-range-tab-5D" i]');
  });
});
