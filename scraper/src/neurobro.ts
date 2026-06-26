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
  // the already-cleared session.
  if (!page.url().startsWith(cfg.neurobroUrl)) {
    await page.goto(cfg.neurobroUrl, { waitUntil: "networkidle" });
  }

  // Start a fresh conversation each cycle (the "new chat" square-pen button). Without
  // this, every cycle piles into one ever-growing thread, which slows the SPA and makes
  // card lookups flaky. Done in-SPA (no reload) so the Cloudflare session is preserved.
  const newChat = page.locator('button:has(svg.lucide-square-pen)').first();
  if (await newChat.isVisible().catch(() => false)) {
    await newChat.click();
    await page.waitForTimeout(800);
  }
  await page.locator('textarea[name="input"]:visible').first().waitFor({ state: "visible", timeout: 30_000 });

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

  // Wait for the setup table to FINISH streaming. The AI types the response token by
  // token, so an early read catches a half-filled row (missing columns, literal "**"
  // markdown). Require: a visible table whose data row has all columns, no streaming
  // "**", and a rendered "n/10" confidence. Independent of "$" and header language.
  await page.waitForFunction(() => {
    const tables = [...document.querySelectorAll("table")].filter((t) => (t as HTMLElement).offsetParent);
    return tables.some((t) => {
      const headerCount = t.querySelectorAll("thead th").length;
      const row = t.querySelector("tbody tr");
      if (!row || headerCount < 4) return false;
      const cells = [...row.querySelectorAll("td")].map((td) => td.textContent || "");
      if (cells.length < headerCount) return false;        // every column present
      const text = cells.join(" ");
      if (text.includes("**")) return false;               // markdown still streaming
      return /\d\s*\/\s*10/.test(text);                    // confidence finished rendering
    });
  }, { timeout: 150_000 });
  await page.waitForTimeout(800); // settle

  // Return the smallest container holding the finished table (+ thesis when nearby).
  // extractSetup picks the setup table by its headers, so a tight scope is enough.
  const setupTable = page.locator("table:visible").last();
  const html = await setupTable.evaluate((tableEl) => {
    let node: HTMLElement = tableEl as HTMLElement;
    for (let i = 0; i < 4 && node.parentElement; i++) {
      const parent = node.parentElement;
      node = parent;
      if (parent.querySelector("blockquote") || /tesis/i.test(parent.textContent || "")) break;
    }
    return node.outerHTML;
  });
  if (process.env.DUMP_CARD) { const fs = await import("node:fs"); fs.writeFileSync(process.env.DUMP_CARD, html); }
  return html;
}
