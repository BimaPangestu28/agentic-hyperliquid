import type { Page } from "playwright";
import type { Config } from "./config.js";

/**
 * Candidate selectors for the chart's date-range tab. Hyperliquid embeds TradingView,
 * whose date-range tabs render as `<button data-name="date-range-tab-1D" value="1D">`
 * (e.g. "1D" = one day of 1-minute candles). The hashed class names change between
 * builds, so we target the stable data-name / value attributes.
 */
export function rangeTabSelectors(range: string): string[] {
  const value = range.trim();
  // CSS attribute selectors are case-sensitive by default. HL renders the tabs lowercase
  // (1d, 5d, 1m, …) while the underlying data-name/value casing can differ between builds,
  // so the `i` flag matches regardless of case — "1d" and "1D" both resolve. Without it a
  // case mismatch silently falls back to the chart's current range.
  return [
    `button[data-name="date-range-tab-${value}" i]`,
    `button[value="${value}" i]`,
  ];
}

/**
 * Best-effort click of the chart date-range tab (e.g. "1D"). TradingView may live in the
 * main document or an embedded frame, so we try both. A miss must not kill the scan —
 * it only logs a warning and the chart's current range is used.
 */
async function selectChartRange(page: Page, range: string): Promise<boolean> {
  for (const selector of rangeTabSelectors(range)) {
    const locators = [page.locator(selector), ...page.frames().map((frame) => frame.locator(selector))];
    for (const locator of locators) {
      try {
        const element = locator.first();
        if (await element.isVisible({ timeout: 1000 })) {
          await element.click({ timeout: 2000 });
          await page.waitForTimeout(1500); // let the candles redraw for the new range
          return true;
        }
      } catch {
        // try the next locator / frame
      }
    }
  }
  console.warn(`HL chart range "${range}" tab not found — using chart default`);
  return false;
}

/**
 * Captures one viewport screenshot per requested date-range, in order. Used to send
 * Neurobro a higher-timeframe (trend bias) and lower-timeframe (entry timing) view of the
 * same coin. Navigates once, then switches range + recaptures for each entry, so the
 * returned buffers line up 1:1 with `ranges`.
 *
 * @param page - The Hyperliquid browser page.
 * @param cfg - Scraper config (base URL).
 * @param coin - Perp symbol to open (e.g. "BTC").
 * @param ranges - TradingView date-range tab values, in capture order (e.g. ["5D","1D"]).
 * @returns One screenshot buffer per range, in the same order.
 */
export async function screenshotCharts(page: Page, cfg: Config, coin: string, ranges: string[]): Promise<Buffer[]> {
  // domcontentloaded, not networkidle: HL streams live prices over a websocket that
  // never goes idle, so networkidle would always hit the timeout. The explicit waits
  // below give the chart time to render.
  await page.goto(`${cfg.hyperliquidUrl}/trade/${coin}`, { waitUntil: "domcontentloaded" });
  await page.waitForTimeout(4000); // let the chart canvas render
  const shots: Buffer[] = [];
  for (const range of ranges) {
    await selectChartRange(page, range); // set the date range (e.g. 1D = 1-min candles, 1 day) before capture
    await page.waitForTimeout(800); // let the candles settle after the range switch
    shots.push(await page.screenshot({ fullPage: false })); // viewport screenshot (chart + book + price)
  }
  return shots;
}

/** Single-range convenience wrapper around {@link screenshotCharts}. */
export async function screenshotChart(page: Page, cfg: Config, coin: string): Promise<Buffer> {
  const [shot] = await screenshotCharts(page, cfg, coin, [cfg.hlTimeframe]);
  return shot;
}

export async function readMark(page: Page): Promise<number> {
  // The HL trade page sets its document title to "<price> | <COIN> | Hyperliquid"
  // (live-updating), which is the most stable mark-price source. Parse the leading
  // number; returns NaN if absent so the slippage gate fails closed.
  const title = await page.title();
  const m = title.match(/^\s*([\d,]+(?:\.\d+)?)\s*\|/);
  return m ? Number(m[1].replace(/,/g, "")) : NaN;
}
