import type { OpenPosition, BotApi } from "./botApi.js";
import type { Page } from "playwright";
import type { Config } from "./config.js";
import { extractSetup } from "./extract.js";
import { screenshotChart, readMark } from "./hyperliquid.js";
import { requestSetup as neurobroRequestSetup, isNeurobroReady } from "./neurobro.js";
import { notifyTelegram } from "./notify.js";
import { dayKey, readQuota, remaining, recordAnalysis } from "./quota.js";

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

  let analysesThisCycle = 0;
  for (const coin of eligibleCoins) {
    // Optional per-cycle cap (0 = unlimited → scan all eligible coins this cycle, e.g. a
    // startup burst). The daily cap below is the hard budget protection regardless.
    if (deps.cfg.maxAnalysesPerCycle > 0 && analysesThisCycle >= deps.cfg.maxAnalysesPerCycle) break;
    if (remaining(readQuota(deps.cfg.quotaStatePath, today), deps.cfg.maxAnalysesPerDay) <= 0) break;
    try {
      const screenshot = await screenshotChart(deps.hlPage, deps.cfg, coin);
      const responseHtml = await neurobroRequestSetup(deps.nbPage, deps.cfg, coin, screenshot);
      recordAnalysis(deps.cfg.quotaStatePath, today); // an analysis was spent the moment Neurobro responded
      analysesThisCycle += 1;
      const setup = extractSetup(responseHtml, coin);

      if (!setup) {
        console.warn(`${coin}: no parseable setup — skip`);
        cooldown(deps, coin);
        continue;
      }
      if (setup.confidence < 7) {
        console.log(`${coin}: conf ${setup.confidence} < 7 — skip`);
        cooldown(deps, coin);
        continue;
      }
      // Re-read the mark fresh: entry came from a chart snapshot up to ~2 min ago, so
      // measure slippage against the live price right before executing.
      await deps.hlPage.bringToFront();
      const markPrice = await readMark(deps.hlPage);
      if (!passesSlippage(markPrice, setup.entry, deps.cfg.maxDeviation)) {
        console.log(`${coin}: slippage mark=${markPrice} entry=${setup.entry} — skip`);
        cooldown(deps, coin);
        continue;
      }
      if (deps.dryRun) {
        console.log(`[dry-run] would execute`, setup);
        continue;
      }
      const result = await deps.api.execute(setup);
      console.log(`${coin}: execute → ${result.status} ok=${result.ok}`);
      if (!result.ok) cooldown(deps, coin);
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
    await new Promise<void>((resolve) => setTimeout(resolve, deps.cfg.pollIntervalSecs * 1000));
  }
}
