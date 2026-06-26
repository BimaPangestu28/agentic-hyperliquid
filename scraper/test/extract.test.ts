import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { extractSetup } from "../src/extract.js";

const fixture = readFileSync(new URL("./fixtures/neurobro-card.html", import.meta.url), "utf8");

describe("extractSetup", () => {
  it("parses the real AVAX setup table", () => {
    const s = extractSetup(fixture, "avax")!;
    expect(s.coin).toBe("AVAX"); // supplied by caller, upper-cased
    expect(s.direction).toBe("long");
    expect(s.entry).toBeCloseTo(6.38);
    expect(s.stopLoss).toBeCloseTo(6.24);
    expect(s.takeProfit).toBeCloseTo(6.56);
    expect(s.confidence).toBe(7);
    expect(s.thesis.length).toBeGreaterThan(10);
    expect(s.thesis.toLowerCase().startsWith("tesis")).toBe(false); // label stripped
  });
  it("returns null when a price is missing (fail-closed)", () => {
    const broken = fixture.replace("$6.24", "n/a");
    expect(extractSetup(broken, "AVAX")).toBeNull();
  });
  it("returns null when confidence is unparseable (fail-closed)", () => {
    const broken = fixture.replace("7/10", "soon");
    expect(extractSetup(broken, "AVAX")).toBeNull();
  });
  it("returns null when there is no setup table", () => {
    expect(extractSetup("<div>hello</div>", "AVAX")).toBeNull();
  });
  it("returns null when coin is empty", () => {
    expect(extractSetup(fixture, "  ")).toBeNull();
  });
  it("tolerates alternate headers (Masuk / SL)", () => {
    const alt = fixture.replace(">Entry<", ">Masuk<").replace(">Stop Loss<", ">SL<");
    const s = extractSetup(alt, "AVAX")!;
    expect(s.entry).toBeCloseTo(6.38);
    expect(s.stopLoss).toBeCloseTo(6.24);
  });
});
