import type { Page } from "playwright";
import type { Config } from "./config.js";

// Sent to Neurobro alongside the chart screenshot. Sharpened to push higher-quality
// setups while staying compatible with extract.ts (table columns Arah/Masuk/Stop Loss/
// Take Profit/Keyakinan + a "Tesis" blockquote) and the streaming-done check (an "x/10"
// confidence). Two pipeline-aware constraints: entry must sit within ~0.3% of the last
// price so it clears the bot's MAX_DEVIATION (0.4%) slippage gate without wasting quota,
// and weak setups are routed to a LOW Keyakinan so the existing confidence>=7 gate filters
// them — a soft "no-trade" that never drops the table the extractor depends on.
const PROMPT = (coin: string) =>
  `Kamu scalper hit-and-run di Hyperliquid perpetual. Analisa chart ${coin} pada timeframe yang ditampilkan dan beri SATU setup scalping terbaik.

Aturan wajib:
- Arah (LONG/SHORT) sesuai bias struktur & momentum di chart. Jangan lawan tren kuat tanpa sinyal pembalikan yang jelas.
- Harga Masuk harus dekat harga sekarang & eksekutabel (maks ~0.3% dari harga terakhir) — entry di pullback/retest level nyata, BUKAN ngejar harga yang sudah jauh.
- Stop Loss di level invalidasi struktural terdekat (di luar swing/likuiditas), ketat sesuai volatilitas — ini batas "salah", bukan angka asal.
- SATU Take Profit saja (TP 100%, JANGAN TP2/TP3) di level struktur/likuiditas realistis berikutnya. Jarak TP minimal 1.5x jarak Stop Loss (risk:reward >= 1.5R).
- Keyakinan (x/10) jujur & terkalibrasi: 8-10 hanya bila banyak konfluensi searah (tren + level + momentum + volume); 7 = layak; <=6 = marjinal/kurang edge. JANGAN gelembungkan — kalau nggak ada edge jelas, kasih Keyakinan rendah.
- Semua harga angka eksplisit.

Tampilkan sebagai kartu "Setup Trading" dengan kolom: Arah, Masuk, Stop Loss, Take Profit, Keyakinan. Lalu satu blockquote "Tesis:" 1-2 kalimat yang menyebut alasan konfluensi DAN level invalidasi.`;

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
 * Dismisses a blocking overlay that intercepts clicks on the composer or new-chat
 * controls. Two kinds occur:
 *   1. shadcn/Radix dialogs — a full-screen `fixed inset-0` backdrop (z-[9999],
 *      backdrop-blur).
 *   2. An open Radix dropdown/popover menu — it mounts a dismissable layer that makes the
 *      WHOLE document intercept pointer events ("<html> intercepts pointer events"), so a
 *      click on anything beneath it silently fails.
 * Both close on Escape. Falls back to a visible close/confirm button. Logs the overlay's
 * text once so an unexpected dialog (announcement, quota notice, etc.) can be identified
 * from the pod logs. Returns true when nothing is blocking afterwards.
 */
async function dismissOverlay(page: Page): Promise<boolean> {
  const overlay = page
    .locator('div.fixed.inset-0, [data-radix-popper-content-wrapper], [role="menu"][data-state="open"]')
    .first();
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

/**
 * Best-effort "new chat" so each cycle starts fresh instead of piling into one
 * ever-growing thread (which slows the SPA and makes card lookups flaky). Done in-SPA (no
 * reload) so the Cloudflare session is preserved.
 *
 * The control is the sidebar "Chat" entry carrying a plus badge — a button with both a
 * `lucide-message-square` and a `lucide-plus` svg, and no `aria-haspopup`. (The
 * `lucide-square-pen` button is a DIFFERENT sidebar menu trigger that only opens a
 * dropdown; clicking it mounts a dismissable layer that makes the whole document intercept
 * pointer events, which is why targeting it always timed out.) Never throws: if the button
 * isn't reachable we reuse the current thread, which is far better than aborting every coin.
 */
async function startNewChat(page: Page): Promise<void> {
  const newChat = page.locator('button:has(svg.lucide-message-square):has(svg.lucide-plus)').first();
  if (!(await newChat.isVisible().catch(() => false))) return;
  for (let attempt = 0; attempt < 3; attempt++) {
    // An open Radix popper/menu makes the document intercept pointer events; Escape closes
    // it. Clear any leftover overlay before each attempt.
    await page.keyboard.press("Escape").catch(() => {});
    await dismissOverlay(page);
    try {
      await newChat.click({ timeout: 8_000 });
      await page.waitForTimeout(800);
      return;
    } catch (error) {
      if (attempt === 2) {
        await dumpDebugShot(page, "new-chat-blocked");
        const msg = (error as Error).message?.split("\n")[0] ?? String(error);
        console.warn(`Neurobro new-chat skipped (${msg}) — reusing current thread`);
      }
    }
  }
}

export async function requestSetup(page: Page, cfg: Config, coin: string, screenshot: Buffer): Promise<string> {
  // Navigate only when not already on Neurobro. Re-issuing goto() every cycle reloads
  // the SPA and re-triggers the Cloudflare challenge; staying on the loaded SPA reuses
  // the already-cleared session.
  if (!page.url().startsWith(cfg.neurobroUrl)) {
    await page.goto(cfg.neurobroUrl, { waitUntil: "domcontentloaded" });
  }

  // A modal/dialog overlay (announcement, quota notice, etc.) or a leftover open dropdown
  // can cover the composer and intercept clicks — clear it before interacting.
  await dismissOverlay(page);

  // Start a fresh conversation each cycle if a real compose button is present. Best-effort:
  // if it can't be clicked we fall back to reusing the current thread rather than aborting
  // the whole coin — the composer wait below is the actual requirement.
  await startNewChat(page);

  await page.locator('textarea[name="input"]:visible').first().waitFor({ state: "visible", timeout: 30_000 });

  // Attach the screenshot directly to the hidden image file input. Neurobro keeps a
  // <input type="file" accept="image/*" class="hidden"> in the DOM; Playwright sets
  // files on hidden inputs, so we skip the "+" menu button (it renders twice — one
  // hidden — and is ambiguous). accept*="image" disambiguates from the docs input.
  const imageInput = page.locator('input[type="file"][accept*="image"]').first();
  await imageInput.setInputFiles({ name: `${coin}.png`, mimeType: "image/png", buffer: screenshot });
  await page.waitForTimeout(1500); // let the image preview/upload register

  // Count the response tables already on screen BEFORE submitting. When the new-chat reset
  // is skipped (square-pen not clickable), prior coins' setup tables stay in the same
  // thread — so we must wait for THIS coin's table to appear *beyond* those. Otherwise the
  // wait returns instantly on a stale, already-finished table and we'd read the previous
  // coin's entry (e.g. AVAX inheriting AAVE's entry=95.15).
  const tablesBefore = await page.evaluate(
    () => [...document.querySelectorAll("table")].filter((t) => (t as HTMLElement).offsetParent).length,
  );

  // Type the prompt and submit. The composer renders twice (one hidden); pick the
  // visible one — same composer whose image input we set above (both are index 0).
  const input = page.locator('textarea[name="input"]:visible').first();
  await input.fill(PROMPT(coin));
  await input.press("Enter");

  // Wait for THIS coin's setup table to FINISH streaming. The AI types the response token
  // by token, so an early read catches a half-filled row (missing columns, literal "**"
  // markdown). Require: a NEW table (count grew past tablesBefore) whose latest row has all
  // columns, no streaming "**", and a rendered "n/10" confidence. Independent of "$" and
  // header language.
  try {
    await page.waitForFunction((prevCount) => {
      const tables = [...document.querySelectorAll("table")].filter((t) => (t as HTMLElement).offsetParent);
      if (tables.length <= prevCount) return false;          // our response table not added yet
      const t = tables[tables.length - 1];                   // newest table = this coin's response
      const headerCount = t.querySelectorAll("thead th").length;
      const row = t.querySelector("tbody tr");
      if (!row || headerCount < 4) return false;
      const cells = [...row.querySelectorAll("td")].map((td) => td.textContent || "");
      if (cells.length < headerCount) return false;          // every column present
      const text = cells.join(" ");
      if (text.includes("**")) return false;                 // markdown still streaming
      return /\d\s*\/\s*10/.test(text);                      // confidence finished rendering
    }, tablesBefore, { timeout: 150_000 });
  } catch (error) {
    if (process.env.NEUROBRO_DEBUG_SHOT) {
      const shot = process.env.NEUROBRO_DEBUG_SHOT;
      const fs = await import("node:fs");
      await page.screenshot({ path: shot, fullPage: true }).catch(() => {});
      await fs.promises.writeFile(shot.replace(/\.png$/, "") + ".html", await page.content()).catch(() => {});
      console.warn(`table wait failed for ${coin} — dumped ${shot} + .html`);
    }
    throw error;
  }
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
