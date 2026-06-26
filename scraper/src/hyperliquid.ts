import type { Page } from "playwright";
import type { Config } from "./config.js";

export async function screenshotChart(page: Page, cfg: Config, coin: string): Promise<Buffer> {
  await page.goto(`${cfg.hyperliquidUrl}/trade/${coin}`, { waitUntil: "networkidle" });
  await page.waitForTimeout(4000); // let the chart canvas render
  return await page.screenshot({ fullPage: false }); // viewport screenshot (chart + book + price)
}

export async function readMark(page: Page): Promise<number> {
  // The trade page shows "Mark" with the price beneath it. Read the page text and
  // parse the mark value. Validate this selector live; fall back to page title which
  // contains the price (e.g. "app.hyperliquid.xyz / 6.2845 | AVAX").
  const title = await page.title();
  const m = title.match(/\/\s*([\d.]+)\s*\|/);
  return m ? Number(m[1]) : NaN;
}
