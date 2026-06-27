import { describe, it, expect, afterEach } from "vitest";
import { createServer, Server } from "node:http";
import { notifyTelegram } from "../src/notify.js";

let server: Server;
afterEach(() => server?.close());

function baseCfg() {
  return {
    botApiUrl: "", botApiToken: "", hyperliquidUrl: "", hyperliquidInfoUrl: "", neurobroUrl: "", storageStatePath: "",
    hlTimeframe: "15m", hlTimeframeHtf: "", atrInterval: "5m", atrPeriod: 14,
    userDataDir: "", headless: true, browserChannel: "chrome",
    pollIntervalSecs: 60, cooldownSecs: 300, maxDeviation: 0.004, minRiskReward: 1.5, maxStopLossPct: 0.03,
    telegramBotToken: "", telegramChatId: "",
    maxAnalysesPerDay: 100, maxAnalysesPerCycle: 1, quotaStatePath: "",
  };
}

describe("notifyTelegram", () => {
  it("no-ops (does not throw) when Telegram is not configured", async () => {
    await expect(notifyTelegram(baseCfg(), "hello")).resolves.toBeUndefined();
  });

  it("POSTs chat_id + text when configured", async () => {
    let captured = "";
    const url = await new Promise<string>((resolve) => {
      server = createServer((req, res) => {
        let body = "";
        req.on("data", (c) => (body += c));
        req.on("end", () => { captured = body; res.writeHead(200, { "content-type": "application/json" }); res.end(JSON.stringify({ ok: true })); });
      }).listen(0, () => resolve(`http://127.0.0.1:${(server.address() as any).port}`));
    });
    // Point the Telegram base at our stub by overriding fetch's target via a token that
    // produces our URL is not possible; instead assert the no-config path above and that
    // a configured call resolves without throwing against a reachable endpoint.
    const cfg = { ...baseCfg(), telegramBotToken: "x", telegramChatId: "123" };
    // Monkeypatch global fetch to hit our stub regardless of the api.telegram.org URL.
    const realFetch = globalThis.fetch;
    (globalThis as any).fetch = (_u: string, init: any) => realFetch(url, init);
    try {
      await notifyTelegram(cfg, "session expired");
    } finally {
      (globalThis as any).fetch = realFetch;
    }
    expect(JSON.parse(captured)).toEqual({ chat_id: "123", text: "session expired" });
  });
});
