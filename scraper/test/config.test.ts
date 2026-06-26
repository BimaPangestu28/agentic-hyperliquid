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
  it("prefers an explicit TELEGRAM_CHAT_ID", () => {
    const c = loadConfig({ ...base, TELEGRAM_CHAT_ID: "999", TELEGRAM_ALLOWED_USER_IDS: "111,222" });
    expect(c.telegramChatId).toBe("999");
  });
  it("falls back to the first TELEGRAM_ALLOWED_USER_IDS when chat id is unset", () => {
    const c = loadConfig({ ...base, TELEGRAM_CHAT_ID: "", TELEGRAM_ALLOWED_USER_IDS: " 111 , 222 " });
    expect(c.telegramChatId).toBe("111");
  });
  it("leaves chat id empty when neither is set", () => {
    expect(loadConfig(base).telegramChatId).toBe("");
  });
});
