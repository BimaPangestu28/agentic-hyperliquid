import type { OpenPosition, BotApi, ExecuteError } from "./botApi.js";
import type { Page } from "playwright";
import type { Config } from "./config.js";
import { extractSetup, validateSetup } from "./extract.js";
import { screenshotCharts, readMark } from "./hyperliquid.js";
import { fetchMarketContext } from "./marketData.js";
import { requestSetup as neurobroRequestSetup, isNeurobroReady } from "./neurobro.js";
import { notifyTelegram } from "./notify.js";
import { dayKey, readQuota, remaining, recordAnalysis } from "./quota.js";
import type { Wake } from "./trigger.js";

/**
 * Coins eligible to scan this cycle: in the watchlist, with no open position,
 * not in cooldown, capped so open.length + result.length <= maxOpen.
 * Coins are compared upper-cased; `cooldownUntil` keys MUST be stored upper-cased
 * by the caller (Task 6's cooldown() helper does this).
 */
export function freeCoins(
  watchlist: string[], open: OpenPosition[], cooldownUntil: Map<string, number>,
  now: number, maxOpen: number,
): string[] {
  const held = new Set(open.map((p) => p.coin.toUpperCase()));
  const slots = Math.max(0, maxOpen - open.length);
  const eligible = watchlist
    .map((c) => c.toUpperCase())
    .filter((c) => !held.has(c))
    .filter((c) => (cooldownUntil.get(c) ?? 0) <= now);
  return eligible.slice(0, slots);
}

/**
 * Renders a human-readable Indonesian explanation for an `/execute` rejection,
 * so the Telegram alert names the gate that fired (kill-switch, cap, margin, …)
 * instead of a bare HTTP status the operator has to guess at.
 *
 * @param status - The HTTP status returned by the bot API.
 * @param error - The parsed `{ reason, ... }` body, when present.
 * @returns A short phrase describing why the trade was rejected.
 */
export function describeExecuteError(status: number, error?: ExecuteError): string {
  switch (error?.reason) {
    case "kill_switch": return "auto-scalp OFF (kill-switch)";
    case "position_cap": return `posisi penuh (${error.open}/${error.max})`;
    case "insufficient_margin":
      return `margin kurang (butuh $${error.required_margin?.toFixed(2)}, free $${error.free_collateral?.toFixed(2)})`;
    case "low_confidence": return "confidence < 7";
    case "bad_direction": return "arah trade invalid";
    default: return error?.reason ?? `HTTP ${status}`;
  }
}

export function passesSlippage(mark: number, entry: number, maxDeviation: number): boolean {
  if (!Number.isFinite(mark) || !Number.isFinite(entry) || entry === 0) return false;
  return Math.abs(mark - entry) / entry <= maxDeviation;
}

export interface RunDeps {
  cfg: Config;
  api: BotApi;
  hlPage: Page;
  nbPage: Page;
  cooldownUntil: Map<string, number>;
  now: () => number;
  dryRun: boolean;
  // Mutable holder (persists across cycles) so the "session expired" alert fires once
  // per outage, not every poll.
  sessionAlertSent: { value: boolean };
  // Day (YYYY-MM-DD) for which the "quota exhausted" alert was already sent, so it fires
  // at most once per day.
  quotaAlertedDay: { value: string };
  // Optional manual-trigger primitive: when present, runForever waits on it between cycles
  // so an external signal (SIGUSR2) can cut the poll wait short and force an immediate scan.
  wake?: Wake;
}

/**
 * Sets a cooldown for the given coin, preventing it from being scanned
 * again until `cfg.cooldownSecs` seconds have elapsed from now.
 */
function cooldown(deps: RunDeps, coin: string): void {
  deps.cooldownUntil.set(coin.toUpperCase(), deps.now() + deps.cfg.cooldownSecs);
}

/**
 * Runs a single scan cycle: fetches the watchlist and open positions,
 * determines eligible coins, then for each coin: takes a chart screenshot,
 * reads the mark price, requests a Neurobro setup, and — if all gates pass —
 * executes the trade (or logs it in dry-run mode).
 */
export async function runOnce(deps: RunDeps): Promise<void> {
  const watchlist = await deps.api.getWatchlist();
  if (!watchlist.autoScalpEnabled) { console.log("auto_scalp disabled — idle"); return; }

  // Verify the Neurobro session is alive before scanning. A Cloudflare/login wall makes
  // every coin fail; detect it once, alert the operator, and pause this cycle instead of
  // hammering the wall coin-by-coin.
  if (!(await isNeurobroReady(deps.nbPage, deps.cfg))) {
    if (!deps.sessionAlertSent.value) {
      await notifyTelegram(deps.cfg, "⚠️ Neurobro session expired / Cloudflare wall — auto-scalp paused. Re-run `npm run login` to restore it.");
      deps.sessionAlertSent.value = true;
    }
    console.warn("Neurobro not ready (Cloudflare/login wall) — skipping cycle");
    return;
  }
  if (deps.sessionAlertSent.value) {
    await notifyTelegram(deps.cfg, "✅ Neurobro session restored — auto-scalp resumed.");
    deps.sessionAlertSent.value = false;
  }

  // Neurobro quota guard. Each analysis spends 1 "light" chat; the daily cap (persisted)
  // hard-stops the loop before it overspends. Bail the whole cycle if nothing is left.
  const today = dayKey(deps.now());
  if (remaining(readQuota(deps.cfg.quotaStatePath, today), deps.cfg.maxAnalysesPerDay) <= 0) {
    if (deps.quotaAlertedDay.value !== today) {
      await notifyTelegram(deps.cfg, `🪫 Neurobro daily quota (${deps.cfg.maxAnalysesPerDay}) reached — auto-scalp paused until reset.`);
      deps.quotaAlertedDay.value = today;
    }
    console.warn("Neurobro daily quota exhausted — skipping cycle");
    return;
  }

  const openPositions = await deps.api.getPositions();
  const eligibleCoins = freeCoins(watchlist.coins, openPositions, deps.cooldownUntil, deps.now(), watchlist.maxOpenPositions);

  // Visibility: tell the operator a scan is starting and which coins it will analyse.
  // Skipped (silent) when nothing is eligible so quiet cycles don't spam Telegram.
  if (eligibleCoins.length > 0) {
    await notifyTelegram(deps.cfg, `🔄 Scan ${eligibleCoins.length} coin: ${eligibleCoins.join(", ")}`);
  }

  const cooldownMins = Math.round(deps.cfg.cooldownSecs / 60);
  let analysesThisCycle = 0;
  for (const coin of eligibleCoins) {
    // Optional per-cycle cap (0 = unlimited → scan all eligible coins this cycle, e.g. a
    // startup burst). The daily cap below is the hard budget protection regardless.
    if (deps.cfg.maxAnalysesPerCycle > 0 && analysesThisCycle >= deps.cfg.maxAnalysesPerCycle) break;
    if (remaining(readQuota(deps.cfg.quotaStatePath, today), deps.cfg.maxAnalysesPerDay) <= 0) break;
    try {
      // Capture HTF (trend bias) + LTF (entry timing) when a higher timeframe is configured;
      // otherwise a single LTF image. Buffers are ordered HTF-first for the prompt.
      const ranges = deps.cfg.hlTimeframeHtf
        ? [deps.cfg.hlTimeframeHtf, deps.cfg.hlTimeframe]
        : [deps.cfg.hlTimeframe];
      const screenshots = await screenshotCharts(deps.hlPage, deps.cfg, coin, ranges);
      // Ground the prompt in real numbers: live mark (off the page) + recent ATR (public API).
      const preMark = await readMark(deps.hlPage);
      const market = await fetchMarketContext(coin, deps.cfg, deps.now());
      const responseHtml = await neurobroRequestSetup(deps.nbPage, deps.cfg, coin, screenshots, {
        markPrice: preMark,
        atrPercent: market.atrPercent,
        multiTimeframe: ranges.length > 1,
      });
      recordAnalysis(deps.cfg.quotaStatePath, today); // an analysis was spent the moment Neurobro responded
      analysesThisCycle += 1;
      const setup = extractSetup(responseHtml, coin);

      if (!setup) {
        console.warn(`${coin}: no parseable setup — skip`);
        await notifyTelegram(deps.cfg, `🔎 ${coin}: Neurobro nggak kasih setup yang kebaca — skip (cooldown ${cooldownMins}m)`);
        cooldown(deps, coin);
        continue;
      }
      // Deterministic guardrail: reject setups whose own numbers are inconsistent (wrong
      // SL/TP ordering, RR below floor, stop too wide) before spending any execution call.
      const validation = validateSetup(setup, {
        minRiskReward: deps.cfg.minRiskReward,
        maxStopLossPct: deps.cfg.maxStopLossPct,
      });
      if (!validation.ok) {
        console.log(`${coin}: setup ditolak — ${validation.reason}`);
        await notifyTelegram(deps.cfg, `🔎 ${coin}: setup ditolak (${validation.reason}) — skip (cooldown ${cooldownMins}m)`);
        cooldown(deps, coin);
        continue;
      }
      if (setup.confidence < 7) {
        console.log(`${coin}: conf ${setup.confidence} < 7 — skip`);
        await notifyTelegram(deps.cfg, `🔎 ${coin}: confidence ${setup.confidence}/10 < 7 — skip (cooldown ${cooldownMins}m)`);
        cooldown(deps, coin);
        continue;
      }
      // Re-read the mark fresh: entry came from a chart snapshot up to ~2 min ago, so
      // measure slippage against the live price right before executing.
      await deps.hlPage.bringToFront();
      const markPrice = await readMark(deps.hlPage);
      if (!passesSlippage(markPrice, setup.entry, deps.cfg.maxDeviation)) {
        console.log(`${coin}: slippage mark=${markPrice} entry=${setup.entry} — skip`);
        await notifyTelegram(deps.cfg, `🔎 ${coin}: harga geser jauh dari entry (mark ${markPrice}, entry ${setup.entry}) — skip (cooldown ${cooldownMins}m)`);
        cooldown(deps, coin);
        continue;
      }
      if (deps.dryRun) {
        console.log(`[dry-run] would execute`, setup);
        continue;
      }
      // Log the exact setup sent so per-coin entry/SL/TP mismatches (e.g. a stale card
      // read from another coin's thread) are visible in the run output, not just on skip.
      console.log(`${coin}: sending ${setup.direction} entry=${setup.entry} SL=${setup.stopLoss} TP=${setup.takeProfit} conf=${setup.confidence}`);
      const result = await deps.api.execute(setup);
      console.log(`${coin}: execute → ${result.status} ok=${result.ok}`);
      const direction = setup.direction.toUpperCase();
      if (result.ok) {
        // Cooldown on success too: otherwise the moment this position closes (SL/TP) the
        // coin is eligible again and gets re-entered next scan — churning the same coin
        // and stacking overlapping brackets, which is what makes the bot's close-fill
        // labels (SL/TP) misattribute against the newest bracket.
        cooldown(deps, coin);
        await notifyTelegram(deps.cfg, `✅ ${coin} ${direction} dieksekusi — entry ${setup.entry}, SL ${setup.stopLoss}, TP ${setup.takeProfit} (conf ${setup.confidence}/10) · cooldown ${cooldownMins}m`);
      } else {
        const why = describeExecuteError(result.status, result.error);
        await notifyTelegram(deps.cfg, `❌ ${coin} ${direction}: eksekusi gagal (${result.status}: ${why}) — cooldown ${cooldownMins}m`);
        cooldown(deps, coin);
      }
    } catch (error) {
      console.error(`${coin}: error`, error);
      cooldown(deps, coin);
    }
  }
}

/**
 * Runs the orchestration loop indefinitely, polling every `cfg.pollIntervalSecs`
 * seconds. A failed `runOnce` cycle is logged but never kills the loop.
 */
export async function runForever(deps: RunDeps): Promise<void> {
  for (;;) {
    try {
      await runOnce(deps);
    } catch (error) {
      console.error("runOnce failed", error);
    }
    // Wait out the poll interval, but let a manual trigger (deps.wake.fire via SIGUSR2)
    // cut it short and start the next cycle immediately.
    const intervalMs = deps.cfg.pollIntervalSecs * 1000;
    await (deps.wake ? deps.wake.wait(intervalMs) : new Promise<void>((resolve) => setTimeout(resolve, intervalMs)));
  }
}
