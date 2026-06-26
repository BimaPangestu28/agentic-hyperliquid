import type { Page } from "playwright";
import type { Config } from "./config.js";

const PROMPT = (coin: string) =>
  `Analisa chart ${coin} ini buat scalping di Hyperliquid perpetual. Kasih SATU setup scalping hit-and-run: tentukan arah (LONG/SHORT), harga Masuk, Stop Loss, dan SATU Take Profit aja (TP1 100%, JANGAN ada TP2/TP3). Wajib sertakan angka harga eksplisit, tingkat Keyakinan (x/10), dan tesis singkat. Tampilkan sebagai kartu "Setup Trading".`;

/**
 * True when Neurobro's chat composer is reachable — i.e. the saved session is still
 * valid. False when a Cloudflare challenge or login wall stands in the way (in both
 * cases the composer textarea never appears). Used to alert + pause before scanning.
 */
export async function isNeurobroReady(page: Page, cfg: Config): Promise<boolean> {
  if (!page.url().startsWith(cfg.neurobroUrl)) {
    await page.goto(cfg.neurobroUrl, { waitUntil: "networkidle" });
  }
  try {
    await page.locator('textarea[name="input"]:visible').first().waitFor({ state: "visible", timeout: 20_000 });
    return true;
  } catch {
    return false;
  }
}

export async function requestSetup(page: Page, cfg: Config, coin: string, screenshot: Buffer): Promise<string> {
  // Navigate only when not already on Neurobro. Re-issuing goto() every cycle reloads
  // the SPA and re-triggers the Cloudflare challenge; staying on the loaded SPA reuses
  // the already-cleared session. New cards are picked via .last() below.
  if (!page.url().startsWith(cfg.neurobroUrl)) {
    await page.goto(cfg.neurobroUrl, { waitUntil: "networkidle" });
  }

  // Attach the screenshot directly to the hidden image file input. Neurobro keeps a
  // <input type="file" accept="image/*" class="hidden"> in the DOM; Playwright sets
  // files on hidden inputs, so we skip the "+" menu button (it renders twice — one
  // hidden — and is ambiguous). accept*="image" disambiguates from the docs input.
  const imageInput = page.locator('input[type="file"][accept*="image"]').first();
  await imageInput.setInputFiles({ name: `${coin}.png`, mimeType: "image/png", buffer: screenshot });
  await page.waitForTimeout(1500); // let the image preview/upload register

  // Type the prompt and submit. The composer renders twice (one hidden); pick the
  // visible one — same composer whose image input we set above (both are index 0).
  const input = page.locator('textarea[name="input"]:visible').first();
  await input.fill(PROMPT(coin));
  await input.press("Enter");

  // The setup arrives as an expandable accordion (toggle button carries aria-expanded,
  // unlike the collapsed grid-preview card with the same label). Expand it if collapsed
  // so its table renders visibly.
  const accordion = page.locator('button[aria-expanded]:has(span:text-is("Setup Trading"))').last();
  await accordion.waitFor({ state: "visible", timeout: 120_000 });
  if ((await accordion.getAttribute("aria-expanded")) !== "true") {
    await accordion.click();
  }

  // The setup renders as a markdown table headed by "Arah". Target it directly (radix
  // panel ids regenerate per render, so don't rely on aria-controls), then wait for a
  // "$" price cell to confirm the values finished loading.
  const setupTable = page.locator('table:has(th:has-text("Arah"))').last();
  await setupTable.waitFor({ state: "visible", timeout: 120_000 });
  await setupTable.locator('td:has-text("$")').first().waitFor({ state: "visible", timeout: 60_000 });

  // Return a container that holds both the table and the "Tesis singkat" blockquote so
  // the parser gets the thesis too; climb from the table until a blockquote is in scope.
  const html = await setupTable.evaluate((tableEl) => {
    let node: HTMLElement = tableEl as HTMLElement;
    for (let i = 0; i < 6 && node.parentElement; i++) {
      node = node.parentElement;
      if (node.querySelector("blockquote")) break;
    }
    return node.outerHTML;
  });
  if (process.env.DUMP_CARD) { const fs = await import("node:fs"); fs.writeFileSync(process.env.DUMP_CARD, html); }
  return html;
}
