import type { Config } from "./config.js";

/** One Hyperliquid candle from the public info API (numeric fields arrive as strings). */
interface Candle {
  h: string; // high
  l: string; // low
  c: string; // close
}

export interface MarketContext {
  /** ATR over `atrPeriod` candles, expressed as a percentage of the last close. */
  atrPercent?: number;
}

/** Milliseconds per supported candle interval; defaults to 5m for an unknown label. */
function intervalToMs(interval: string): number {
  const table: Record<string, number> = {
    "1m": 60_000, "3m": 180_000, "5m": 300_000, "15m": 900_000,
    "30m": 1_800_000, "1h": 3_600_000, "4h": 14_400_000, "1d": 86_400_000,
  };
  return table[interval] ?? 300_000;
}

/**
 * Average True Range over the last `period` candles, as a percent of the last close.
 * Returns NaN when there aren't enough candles to measure. Pure — exported for testing.
 *
 * @param candles - Candles in ascending time order.
 * @param period - Number of true-range samples to average (e.g. 14).
 * @returns ATR as a percentage of the last close, or NaN if undeterminable.
 */
export function computeAtrPercent(candles: Candle[], period: number): number {
  if (!Array.isArray(candles) || candles.length < period + 1) return NaN;
  const recent = candles.slice(-(period + 1));
  let trueRangeSum = 0;
  for (let index = 1; index < recent.length; index += 1) {
    const high = Number(recent[index].h);
    const low = Number(recent[index].l);
    const previousClose = Number(recent[index - 1].c);
    if (![high, low, previousClose].every(Number.isFinite)) return NaN;
    trueRangeSum += Math.max(high - low, Math.abs(high - previousClose), Math.abs(low - previousClose));
  }
  const lastClose = Number(recent[recent.length - 1].c);
  if (!Number.isFinite(lastClose) || lastClose === 0) return NaN;
  const averageTrueRange = trueRangeSum / period;
  return (averageTrueRange / lastClose) * 100;
}

/**
 * Best-effort recent-volatility context from Hyperliquid's public info API. Used to give
 * Neurobro a concrete ATR hint so its Stop Loss distance is grounded in real volatility
 * instead of guessed off the chart. Fail-open: any error (network, timeout, bad shape)
 * returns an empty object so a missing hint never blocks a setup. Never throws.
 *
 * @param coin - Perp symbol as Hyperliquid names it (e.g. "BTC", "PENDLE").
 * @param cfg - Scraper config (info URL, ATR interval/period).
 * @param nowMs - Current epoch millis (injectable for testing).
 * @returns Volatility context, possibly empty.
 */
export async function fetchMarketContext(coin: string, cfg: Config, nowMs: number = Date.now()): Promise<MarketContext> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 5_000);
  try {
    const lookbackCandles = cfg.atrPeriod + 5;
    const startTime = nowMs - lookbackCandles * intervalToMs(cfg.atrInterval);
    const response = await fetch(cfg.hyperliquidInfoUrl, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        type: "candleSnapshot",
        req: { coin, interval: cfg.atrInterval, startTime, endTime: nowMs },
      }),
      signal: controller.signal,
    });
    if (!response.ok) return {};
    const candles = (await response.json()) as Candle[];
    const atrPercent = computeAtrPercent(candles, cfg.atrPeriod);
    return Number.isFinite(atrPercent) ? { atrPercent } : {};
  } catch {
    return {};
  } finally {
    clearTimeout(timer);
  }
}
