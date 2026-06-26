import type { Page } from "playwright";
import type { Config } from "./config.js";

/**
 * Maps a timeframe key (e.g. "15m", "1d") to the candidate button labels
 * Hyperliquid's chart may render it as. Daily is shown as "D"; hourly sometimes
 * as "60m". Returns the key itself as a fallback for unmapped values.
 */
export function timeframeLabels(timeframe: string): string[] {
  const key = timeframe.trim().toLowerCase();
  const map: Record<string, string[]> = {
    "1m": ["1m"], "3m": ["3m"], "5m": ["5m"], "15m": ["15m"], "30m": ["30m"],
    "1h": ["1h", "60m"], "2h": ["2h", "120m"], "4h": ["4h", "240m"],
    "8h": ["8h"], "12h": ["12h"],
    "1d": ["D", "1D", "1d"], "d": ["D", "1D"],
    "1w": ["W", "1W"], "w": ["W", "1W"],
  };
  return map[key] ?? [timeframe];
}

/**
 * Best-effort click of the chart timeframe selector. A label change must not kill
 * the scan, so a miss only logs a warning and the chart's default timeframe is used.
 */
async function selectTimeframe(page: Page, timeframe: string): Promise<boolean> {
  for (const label of timeframeLabels(timeframe)) {
    for (const locator of [
      page.getByRole("button", { name: label, exact: true }),
      page.getByText(label, { exact: true }),
    ]) {
      try {
        const element = locator.first();
        if (await element.isVisible({ timeout: 1000 })) {
          await element.click();
          await page.waitForTimeout(1500); // let the candles redraw at the new interval
          return true;
        }
      } catch {
        // try the next locator/label
      }
    }
  }
  console.warn(`HL timeframe "${timeframe}" button not found — using chart default`);
  return false;
}

export async function screenshotChart(page: Page, cfg: Config, coin: string): Promise<Buffer> {
  await page.goto(`${cfg.hyperliquidUrl}/trade/${coin}`, { waitUntil: "networkidle" });
  await page.waitForTimeout(4000); // let the chart canvas render
  await selectTimeframe(page, cfg.hlTimeframe); // set the analysis timeframe (e.g. 15m) before capture
  return await page.screenshot({ fullPage: false }); // viewport screenshot (chart + book + price)
}

export async function readMark(page: Page): Promise<number> {
  // The HL trade page sets its document title to "<price> | <COIN> | Hyperliquid"
  // (live-updating), which is the most stable mark-price source. Parse the leading
  // number; returns NaN if absent so the slippage gate fails closed.
  const title = await page.title();
  const m = title.match(/^\s*([\d,]+(?:\.\d+)?)\s*\|/);
  return m ? Number(m[1].replace(/,/g, "")) : NaN;
}
