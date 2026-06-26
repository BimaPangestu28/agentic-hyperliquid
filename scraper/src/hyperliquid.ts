import type { Page } from "playwright";
import type { Config } from "./config.js";

export async function screenshotChart(page: Page, cfg: Config, coin: string): Promise<Buffer> {
  await page.goto(`${cfg.hyperliquidUrl}/trade/${coin}`, { waitUntil: "networkidle" });
  await page.waitForTimeout(4000); // let the chart canvas render
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
