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
  return [
    `button[data-name="date-range-tab-${value}"]`,
    `button[value="${value}"]`,
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

export async function screenshotChart(page: Page, cfg: Config, coin: string): Promise<Buffer> {
  // domcontentloaded, not networkidle: HL streams live prices over a websocket that
  // never goes idle, so networkidle would always hit the timeout. The explicit waits
  // below give the chart time to render.
  await page.goto(`${cfg.hyperliquidUrl}/trade/${coin}`, { waitUntil: "domcontentloaded" });
  await page.waitForTimeout(4000); // let the chart canvas render
  await selectChartRange(page, cfg.hlTimeframe); // set the date range (e.g. 1D = 1-min candles, 1 day) before capture
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
