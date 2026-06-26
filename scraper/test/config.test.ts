import { describe, it, expect } from "vitest";
import { loadConfig } from "../src/config.js";

describe("loadConfig", () => {
  const base = { BOT_API_URL: "http://b", BOT_API_TOKEN: "t" };
  it("applies defaults for optional keys", () => {
    const c = loadConfig(base);
    expect(c.pollIntervalSecs).toBe(60);
    expect(c.maxDeviation).toBeCloseTo(0.004);
    expect(c.neurobroUrl).toContain("neurobro");
  });
  it("throws when a required key is missing", () => {
    expect(() => loadConfig({ BOT_API_URL: "http://b" })).toThrow(/BOT_API_TOKEN/);
  });
});
