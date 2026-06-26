import { describe, it, expect, afterEach } from "vitest";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { dayKey, readQuota, remaining, recordAnalysis } from "../src/quota.js";

let dir: string;
const tmpPath = () => { dir = mkdtempSync(join(tmpdir(), "quota-")); return join(dir, "q.json"); };
afterEach(() => { if (dir) rmSync(dir, { recursive: true, force: true }); });

describe("dayKey", () => {
  it("is the UTC date for the given epoch seconds", () => {
    expect(dayKey(Date.UTC(2026, 5, 27, 10, 30) / 1000)).toBe("2026-06-27");
  });
});

describe("readQuota", () => {
  it("returns zero for a missing file", () => {
    expect(readQuota(tmpPath(), "2026-06-27")).toEqual({ date: "2026-06-27", count: 0 });
  });
  it("resets when the stored day is not today", () => {
    const p = tmpPath();
    writeFileSync(p, JSON.stringify({ date: "2026-06-26", count: 25 }));
    expect(readQuota(p, "2026-06-27")).toEqual({ date: "2026-06-27", count: 0 });
  });
  it("keeps the count for the same day", () => {
    const p = tmpPath();
    writeFileSync(p, JSON.stringify({ date: "2026-06-27", count: 7 }));
    expect(readQuota(p, "2026-06-27").count).toBe(7);
  });
});

describe("remaining", () => {
  it("never goes negative", () => {
    expect(remaining({ date: "d", count: 30 }, 25)).toBe(0);
    expect(remaining({ date: "d", count: 10 }, 25)).toBe(15);
  });
});

describe("recordAnalysis", () => {
  it("increments and persists across reads", () => {
    const p = tmpPath();
    recordAnalysis(p, "2026-06-27");
    recordAnalysis(p, "2026-06-27");
    expect(readQuota(p, "2026-06-27").count).toBe(2);
  });
});
