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
    await page.goto(cfg.neurobroUrl, { waitUntil: "domcontentloaded" });
  }
  // Neurobro shows a full-screen "Initializing / Setting up your secure session" gate
  // (a Cloudflare-style handshake) before the app loads. Give it time to resolve on its
  // own — pressing keys does nothing to it. Only once it clears is the composer real.
  try {
    await page.waitForFunction(() => {
      const overlay = document.querySelector("div.fixed.inset-0");
      if (!overlay || (overlay as HTMLElement).offsetParent === null) return true; // gone
      return !/secure session|initializing/i.test(overlay.textContent || "");
    }, { timeout: 60_000 });
  } catch {
    await dumpDebugShot(page, "secure-session-gate");
    return false; // still gated after 60s → walled (likely Cloudflare on the datacenter IP)
  }
  try {
    await page.locator('textarea[name="input"]:visible').first().waitFor({ state: "visible", timeout: 20_000 });
    return true;
  } catch {
    await dumpDebugShot(page, "no-composer");
    return false;
  }
}

/**
 * Best-effort full-page screenshot to NEUROBRO_DEBUG_SHOT (default /data/neurobro-debug.png)
 * so the operator can see exactly what blocked the session (Cloudflare challenge, login,
 * etc.). Never throws.
 */
async function dumpDebugShot(page: Page, label: string): Promise<void> {
  try {
    const path = process.env.NEUROBRO_DEBUG_SHOT ?? "/data/neurobro-debug.png";
    await page.screenshot({ path, fullPage: false });
    console.warn(`Neurobro blocked (${label}) — wrote debug screenshot to ${path}`);
  } catch {
    // ignore — diagnostics are optional
  }
}

/**
 * Dismisses a blocking modal/dialog overlay that intercepts clicks on the composer or
 * new-chat controls. Neurobro renders shadcn/Radix dialogs as a full-screen
 * `fixed inset-0` backdrop (z-[9999], backdrop-blur); these close on Escape. Falls back
 * to a visible close/confirm button. Logs the overlay's text once so an unexpected
 * dialog (announcement, quota notice, etc.) can be identified from the pod logs.
 * Returns true when nothing is blocking afterwards.
 */
async function dismissOverlay(page: Page): Promise<boolean> {
  const overlay = page.locator("div.fixed.inset-0").first();
  for (let attempt = 0; attempt < 3; attempt++) {
    if (!(await overlay.isVisible().catch(() => false))) return true;
    const text = ((await overlay.textContent().catch(() => "")) || "").replace(/\s+/g, " ").trim().slice(0, 200);
    console.warn(`Neurobro overlay blocking (attempt ${attempt + 1}/3): "${text}"`);
    await page.keyboard.press("Escape");
    await page.waitForTimeout(500);
    if (!(await overlay.isVisible().catch(() => false))) return true;
    const closeButton = page.locator(
      '[role="dialog"] button[aria-label*="close" i], button[aria-label*="close" i], ' +
      '[role="dialog"] button:has-text("Tutup"), [role="dialog"] button:has-text("Close"), ' +
      '[role="dialog"] button:has-text("OK"), [role="dialog"] button:has-text("Got it"), ' +
      '[role="dialog"] button:has-text("Mengerti"), [role="dialog"] button:has-text("Lanjut")',
    ).first();
    if (await closeButton.isVisible().catch(() => false)) {
      await closeButton.click().catch(() => {});
      await page.waitForTimeout(500);
    }
  }
  return !(await overlay.isVisible().catch(() => false));
}

export async function requestSetup(page: Page, cfg: Config, coin: string, screenshot: Buffer): Promise<string> {
  // Navigate only when not already on Neurobro. Re-issuing goto() every cycle reloads
  // the SPA and re-triggers the Cloudflare challenge; staying on the loaded SPA reuses
  // the already-cleared session.
  if (!page.url().startsWith(cfg.neurobroUrl)) {
    await page.goto(cfg.neurobroUrl, { waitUntil: "domcontentloaded" });
  }

  // A modal/dialog overlay (announcement, quota notice, etc.) can cover the composer and
  // intercept clicks — dismiss it before interacting with the new-chat control.
  await dismissOverlay(page);

  // Start a fresh conversation each cycle (the "new chat" square-pen button). Without
  // this, every cycle piles into one ever-growing thread, which slows the SPA and makes
  // card lookups flaky. Done in-SPA (no reload) so the Cloudflare session is preserved.
  const newChat = page.locator('button:has(svg.lucide-square-pen)').first();
  if (await newChat.isVisible().catch(() => false)) {
    try {
      await newChat.click({ timeout: 10_000 });
    } catch {
      // An overlay likely re-appeared and intercepted the click — dismiss and retry once.
      await dismissOverlay(page);
      await newChat.click({ timeout: 10_000 });
    }
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
