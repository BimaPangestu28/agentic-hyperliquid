import type { Config } from "./config.js";
import type { Setup } from "./extract.js";

export interface OpenPosition { coin: string }

/**
 * Body returned by `/execute` when a request is rejected. `reason` identifies
 * which gate fired; the remaining fields carry context for specific reasons
 * (open/max for `position_cap`, required/free for `insufficient_margin`).
 */
export interface ExecuteError {
  ok: false;
  reason?: string;
  open?: number;
  max?: number;
  required_margin?: number;
  free_collateral?: number;
}

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

  /**
   * Sends a setup to the bot's `/execute` endpoint.
   *
   * On rejection the endpoint returns a JSON body `{ ok: false, reason, ... }`
   * identifying which gate fired (kill_switch, position_cap, insufficient_margin,
   * low_confidence, bad_direction). The body is surfaced via `error` so callers
   * can report the actual cause instead of a bare status code.
   *
   * @param setup - The trade setup parsed from a Neurobro analysis.
   * @returns Whether it succeeded, the HTTP status, and the parsed error body on failure.
   */
  async execute(setup: Setup): Promise<{ ok: boolean; status: number; error?: ExecuteError }> {
    const body = JSON.stringify({
      coin: setup.coin, direction: setup.direction, entry: setup.entry,
      stop_loss: setup.stopLoss, take_profit: setup.takeProfit,
      confidence: setup.confidence, thesis: setup.thesis,
    });
    const res = await this.request("/execute", { method: "POST", body });
    if (res.ok) return { ok: true, status: res.status };
    let error: ExecuteError | undefined;
    try { error = await res.json() as ExecuteError; } catch { /* non-JSON body — leave undefined */ }
    return { ok: false, status: res.status, error };
  }
}
