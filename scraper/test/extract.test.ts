import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { extractSetup } from "../src/extract.js";

const fixture = readFileSync(new URL("./fixtures/neurobro-card.html", import.meta.url), "utf8");

describe("extractSetup", () => {
  it("parses the real SOL card", () => {
    const s = extractSetup(fixture)!;
    expect(s.coin).toBe("SOL");
    expect(s.direction).toBe("long");
    expect(s.entry).toBeCloseTo(69.32);
    expect(s.stopLoss).toBeCloseTo(68.98);
    expect(s.takeProfit).toBeCloseTo(70.10);
    expect(s.confidence).toBe(7);
    expect(s.thesis.length).toBeGreaterThan(10);
  });
  it("returns null when a price is missing (fail-closed)", () => {
    const broken = fixture.replace("$68.98", "n/a");
    expect(extractSetup(broken)).toBeNull();
  });
  it("returns null when there is no setup card", () => {
    expect(extractSetup("<div>hello</div>")).toBeNull();
  });
  it("ignores a decoy n/10 outside the Keyakinan card", () => {
    const decoy = fixture.replace('<h4', '<span>3/10</span><h4');
    expect(extractSetup(decoy)!.confidence).toBe(7);
  });
});
