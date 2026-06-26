import type { Page } from "playwright";
import type { Config } from "./config.js";

const PROMPT = (coin: string) =>
  `Analisa chart ${coin} ini buat scalping di Hyperliquid perpetual. Kasih SATU setup scalping hit-and-run: tentukan arah (LONG/SHORT), harga Masuk, Stop Loss, dan SATU Take Profit aja (TP1 100%, JANGAN ada TP2/TP3). Wajib sertakan angka harga eksplisit, tingkat Keyakinan (x/10), dan tesis singkat. Tampilkan sebagai kartu "Setup Trading".`;

export async function requestSetup(page: Page, cfg: Config, coin: string, screenshot: Buffer): Promise<string> {
  await page.goto(cfg.neurobroUrl, { waitUntil: "networkidle" });

  // Attach the screenshot. LIVE-RESOLVE: click the "+" button (button containing
  // svg.lucide-plus) to reveal the upload menu, then setInputFiles on the revealed
  // input[type=file]. If a hidden input[type=file] is already in the DOM, set it
  // directly. Capture the exact menu/file-input during implementation.
  await page.locator('button:has(svg.lucide-plus)').click();
  const fileInput = page.locator('input[type=file]');
  await fileInput.setInputFiles({ name: `${coin}.png`, mimeType: "image/png", buffer: screenshot });

  // Type the prompt and submit.
  // LIVE-RESOLVE: confirm the input is a textarea[name="input"] and not a rich-text div on the live Neurobro DOM.
  const input = page.locator('textarea[name="input"]');
  await input.fill(PROMPT(coin));
  await input.press("Enter");

  // Wait for the "Setup Trading" card to render, then return its outerHTML.
  const card = page.locator('div:has(> button span:text-is("Setup Trading"))').last();
  await card.waitFor({ state: "visible", timeout: 120_000 });
  await page.waitForTimeout(2000); // allow the card values to finish animating in
  return await card.evaluate((el) => el.outerHTML);
}
