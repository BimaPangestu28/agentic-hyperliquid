import { describe, it, expect } from "vitest";
import { timeframeLabels } from "../src/hyperliquid.js";

describe("timeframeLabels", () => {
  it("maps minute timeframes to their own label", () => {
    expect(timeframeLabels("15m")).toEqual(["15m"]);
    expect(timeframeLabels("5m")).toEqual(["5m"]);
  });
  it("is case-insensitive and trims", () => {
    expect(timeframeLabels("  15M ")).toEqual(["15m"]);
  });
  it("offers alternate labels for hourly", () => {
    expect(timeframeLabels("1h")).toContain("1h");
    expect(timeframeLabels("1h")).toContain("60m");
  });
  it("maps daily to the chart's D label", () => {
    expect(timeframeLabels("1d")).toContain("D");
  });
  it("falls back to the raw value for unknown keys", () => {
    expect(timeframeLabels("7m")).toEqual(["7m"]);
  });
});
