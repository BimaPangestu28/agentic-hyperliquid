import type { Config } from "./config.js";
import type { Setup } from "./extract.js";

export interface OpenPosition { coin: string }

// Hard cap per request so a dead bot API (or a tunnel whose upstream is down) fails fast
// with a clear error instead of hanging the whole loop forever — the SSH forward keeps the
// local port open even when nothing answers behind it, so an un-timed fetch never returns.
const REQUEST_TIMEOUT_MS = 15_000;

export class BotApi {
  constructor(private cfg: Config) {}
  private headers() { return { authorization: `Bearer ${this.cfg.botApiToken}`, "content-type": "application/json" }; }

  // Wraps fetch with an abort-based timeout and a uniform, actionable error message.
  private async request(path: string, init?: RequestInit): Promise<Response> {
    try {
      return await fetch(`${this.cfg.botApiUrl}${path}`, {
        ...init,
        headers: this.headers(),
        signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      });
    } catch (error) {
      if (error instanceof Error && error.name === "TimeoutError") {
        throw new Error(`bot API ${path} no response within ${REQUEST_TIMEOUT_MS}ms (is the tunnel + bot API up?)`);
      }
      throw error;
    }
  }

  async getWatchlist() {
    const res = await this.request("/watchlist");
    if (!res.ok) throw new Error(`watchlist ${res.status}`);
    const j = await res.json() as any;
    return { coins: j.coins as string[], autoScalpEnabled: j.auto_scalp_enabled as boolean, maxOpenPositions: j.max_open_positions as number };
  }

  async getPositions(): Promise<OpenPosition[]> {
    const res = await this.request("/positions");
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
    const res = await this.request("/execute", { method: "POST", body });
    return { ok: res.ok, status: res.status };
  }
}
