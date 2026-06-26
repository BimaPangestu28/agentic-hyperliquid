import { describe, it, expect } from "vitest";
import { freeCoins, passesSlippage } from "../src/loop.js";

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
});
