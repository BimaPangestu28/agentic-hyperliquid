import { describe, it, expect } from "vitest";
import { freeCoins, passesSlippage, describeExecuteError } from "../src/loop.js";

describe("freeCoins", () => {
  it("excludes coins with positions, cooldown, and respects the cap", () => {
    const wl = ["BTC", "ETH", "SOL", "AVAX"];
    const open = [{ coin: "BTC" }];
    const cd = new Map([["ETH", 2000]]);
    // maxOpen 3, 1 already open → at most 2 new; ETH cooled down; BTC held → [SOL, AVAX]
    expect(freeCoins(wl, open, cd, 1000, 3)).toEqual(["SOL", "AVAX"]);
  });
  it("cap leaves zero slots when already full", () => {
    expect(freeCoins(["BTC","ETH"], [{coin:"X"},{coin:"Y"}], new Map(), 0, 2)).toEqual([]);
  });
});

describe("passesSlippage", () => {
  it("passes within deviation and fails beyond", () => {
    expect(passesSlippage(100.3, 100, 0.004)).toBe(true);   // 0.3%
    expect(passesSlippage(100.5, 100, 0.004)).toBe(false);  // 0.5%
  });
  it("fails closed on non-finite inputs and zero entry", () => {
    expect(passesSlippage(NaN, 100, 0.01)).toBe(false);
    expect(passesSlippage(100, NaN, 0.01)).toBe(false);
    expect(passesSlippage(Infinity, 100, 0.01)).toBe(false);
    expect(passesSlippage(100, 0, 0.01)).toBe(false);
  });
});

describe("describeExecuteError", () => {
  it("names each gate and includes context for cap and margin", () => {
    expect(describeExecuteError(409, { ok: false, reason: "kill_switch" }))
      .toBe("auto-scalp OFF (kill-switch)");
    expect(describeExecuteError(409, { ok: false, reason: "position_cap", open: 10, max: 10 }))
      .toBe("posisi penuh (10/10)");
    expect(describeExecuteError(409, { ok: false, reason: "insufficient_margin", required_margin: 30.5, free_collateral: 1.2 }))
      .toBe("margin kurang (butuh $30.50, free $1.20)");
    expect(describeExecuteError(422, { ok: false, reason: "low_confidence" })).toBe("confidence < 7");
  });
  it("falls back to the raw status when no parseable body is present", () => {
    expect(describeExecuteError(502, undefined)).toBe("HTTP 502");
  });
});
