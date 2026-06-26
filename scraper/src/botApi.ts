import type { Config } from "./config.js";
import type { Setup } from "./extract.js";

export interface OpenPosition { coin: string }

export class BotApi {
  constructor(private cfg: Config) {}
  private headers() { return { authorization: `Bearer ${this.cfg.botApiToken}`, "content-type": "application/json" }; }

  async getWatchlist() {
    const res = await fetch(`${this.cfg.botApiUrl}/watchlist`, { headers: this.headers() });
    if (!res.ok) throw new Error(`watchlist ${res.status}`);
    const j = await res.json() as any;
    return { coins: j.coins as string[], autoScalpEnabled: j.auto_scalp_enabled as boolean, maxOpenPositions: j.max_open_positions as number };
  }

  async getPositions(): Promise<OpenPosition[]> {
    const res = await fetch(`${this.cfg.botApiUrl}/positions`, { headers: this.headers() });
    if (!res.ok) throw new Error(`positions ${res.status}`);
    const j = await res.json() as any;
    const arr = Array.isArray(j) ? j : (j.positions ?? []);
    return arr.map((p: any) => ({ coin: String(p.coin).toUpperCase() }));
  }

  async execute(setup: Setup): Promise<{ ok: boolean; status: number }> {
    const body = JSON.stringify({
      coin: setup.coin, direction: setup.direction, entry: setup.entry,
      stop_loss: setup.stopLoss, take_profit: setup.takeProfit,
      confidence: setup.confidence, thesis: setup.thesis,
    });
    const res = await fetch(`${this.cfg.botApiUrl}/execute`, { method: "POST", headers: this.headers(), body });
    return { ok: res.ok, status: res.status };
  }
}
