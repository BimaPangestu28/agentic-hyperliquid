import { describe, it, expect, afterEach } from "vitest";
import { createServer, Server } from "node:http";
import { BotApi } from "../src/botApi.js";

let server: Server;
afterEach(() => server?.close());

function stub(handler: (url: string, body: string) => { status: number; json: unknown }) {
  return new Promise<string>((resolve) => {
    server = createServer((req, res) => {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        const r = handler(req.url ?? "", body);
        res.writeHead(r.status, { "content-type": "application/json" });
        res.end(JSON.stringify(r.json));
      });
    }).listen(0, () => resolve(`http://127.0.0.1:${(server.address() as any).port}`));
  });
}

const cfg = (url: string) => ({ botApiUrl: url, botApiToken: "t", hyperliquidUrl: "", neurobroUrl: "", storageStatePath: "", hlTimeframe: "15m", hlTimeframeHtf: "", hyperliquidInfoUrl: "", atrInterval: "5m", atrPeriod: 14, userDataDir: "", headless: true, browserChannel: "chrome", pollIntervalSecs: 60, cooldownSecs: 300, maxDeviation: 0.004, minRiskReward: 1.5, maxStopLossPct: 0.03, telegramBotToken: "", telegramChatId: "", maxAnalysesPerDay: 100, maxAnalysesPerCycle: 1, quotaStatePath: "" });

describe("BotApi", () => {
  it("getWatchlist parses the response", async () => {
    const url = await stub(() => ({ status: 200, json: { coins: ["BTC"], auto_scalp_enabled: true, max_open_positions: 5 } }));
    const api = new BotApi(cfg(url));
    const w = await api.getWatchlist();
    expect(w.coins).toEqual(["BTC"]);
    expect(w.autoScalpEnabled).toBe(true);
    expect(w.maxOpenPositions).toBe(5);
  });
  it("execute returns ok:false on a 4xx instead of throwing", async () => {
    const url = await stub(() => ({ status: 422, json: {} }));
    const api = new BotApi(cfg(url));
    const r = await api.execute({ coin: "SOL", direction: "long", entry: 1, stopLoss: 0.9, takeProfit: 1.1, confidence: 6, thesis: "x" });
    expect(r.ok).toBe(false);
    expect(r.status).toBe(422);
  });
});
