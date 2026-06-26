# Neurobro Auto-Scalping — Scraper Subsystem Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A standalone TypeScript/Playwright service (in `scraper/`) that runs 24/7 on the VPS: for each free watchlist coin it screenshots the Hyperliquid chart, asks Neurobro for a one-TP scalp setup, parses the result, and — if confidence ≥ 7 and a slippage check passes — calls the bot's `POST /execute` to auto-open the position.

**Architecture:** Pure, unit-tested core (Neurobro card HTML parser, bot API client, loop/free-slot/cooldown/slippage logic) plus thin live browser modules (Hyperliquid screenshot, Neurobro upload+read, one-time login session) that are validated manually against the live sites. The bot endpoints (`GET /watchlist`, `GET /positions`, `POST /execute`) already exist and are deployed.

**Tech Stack:** Node.js (≥20), TypeScript, Playwright (Chromium), cheerio (HTML parsing), vitest (tests), undici/fetch (HTTP).

## Global Constraints

- New code lives entirely under `scraper/` — it does NOT touch the Rust bot.
- The bot is the only thing that places orders; the scraper only calls `POST /execute` with a structured setup. Request body (verbatim, matches the bot's `ExecuteRequest`): `{ coin, direction: "long"|"short", entry, stop_loss, take_profit, confidence, thesis }`.
- Only POST a setup when `confidence >= 7` AND the slippage check passes (`abs(mark - entry)/entry <= MAX_DEVIATION`, default 0.004 = 0.4%).
- Fail-closed: any parse/extraction doubt → return `null` → skip the coin (never guess numbers).
- Auth: every bot API call sends `Authorization: Bearer <BOT_API_TOKEN>`.
- The scraper reuses a saved Neurobro browser session (`storageState` JSON); it does NOT automate the OTP login in the loop.
- Per-coin cooldown after a skip/failure (`COOLDOWN_SECS`, default 300) to avoid hammering Neurobro.
- Run `npm test` (vitest) after each task; pure logic is unit-tested, browser modules are validated via `npm run login` + `npm run dry-run` (no `/execute` call).

## Confirmed facts (from live DOM captured 2026-06-26)

- Neurobro: `app.neurobro.ai`. Chat input `textarea[name="input"]` (placeholder "Tanyakan apa saja"). Attach control: `button` containing `svg.lucide-plus` to the left of the textarea (`aria-haspopup="menu"`). The file-input revealed after clicking `+` is NOT yet captured — resolve live in Task 5 (prefer `setInputFiles` on the revealed `input[type=file]`).
- Neurobro setup card: container has a header `span.font-medium` with text `Setup Trading`; coin in `h4` "Setup trading untuk {COIN}"; "Arah" card value is `LONG`/`SHORT`; confidence as a `{n}/10` span; the SL/Entry/TP rows are labeled `SL`, `Masuk`, `TP1` each with a `$`-prefixed price span. A real fixture is committed at `scraper/test/fixtures/neurobro-card.html` (SOL, LONG, entry 69.32, SL 68.98, TP1 70.10, conf 7/10).
- Hyperliquid: navigate to `https://app.hyperliquid.xyz/trade/{COIN}` and take a full-viewport screenshot (no per-element selector needed). The mark price is shown on the page and is also read for the slippage check.

## File structure (all under `scraper/`)

- `package.json`, `tsconfig.json`, `vitest.config.ts`, `.env.example`, `Dockerfile`
- `src/config.ts` — env → typed `Config` (pure validation)
- `src/extract.ts` — pure: Neurobro card HTML → `Setup | null`
- `src/botApi.ts` — bot HTTP client (getWatchlist/getPositions/execute)
- `src/loop.ts` — pure: `freeCoins`, `passesSlippage`, cooldown map; plus `runOnce`/`runForever` orchestration
- `src/hyperliquid.ts` — live: `screenshotChart(page, coin) → Buffer` + `readMark(page) → number`
- `src/neurobro.ts` — live: `requestSetup(page, coin, screenshot) → cardHtml`
- `src/login.ts` — one-time headed login → saves `storageState`
- `src/index.ts` — entry point: build config, launch browser w/ saved session, run loop
- `test/extract.test.ts`, `test/loop.test.ts`, `test/botApi.test.ts`, `test/fixtures/neurobro-card.html` (committed)

---

### Task 1: Project scaffold + config

**Files:**
- Create: `scraper/package.json`, `scraper/tsconfig.json`, `scraper/vitest.config.ts`, `scraper/.env.example`, `scraper/src/config.ts`, `scraper/test/config.test.ts`.

**Interfaces:**
- Produces: `export interface Config { botApiUrl: string; botApiToken: string; hyperliquidUrl: string; neurobroUrl: string; storageStatePath: string; pollIntervalSecs: number; cooldownSecs: number; maxDeviation: number }` and `export function loadConfig(env: Record<string,string|undefined>): Config` (throws on missing required keys).

- [ ] **Step 1: Create `package.json`**

```json
{
  "name": "neurobro-scraper",
  "private": true,
  "type": "module",
  "scripts": {
    "test": "vitest run",
    "login": "tsx src/login.ts",
    "dry-run": "tsx src/index.ts --dry-run",
    "start": "tsx src/index.ts"
  },
  "dependencies": {
    "playwright": "^1.48.0",
    "cheerio": "^1.0.0"
  },
  "devDependencies": {
    "typescript": "^5.6.0",
    "tsx": "^4.19.0",
    "vitest": "^2.1.0",
    "@types/node": "^22.0.0"
  }
}
```

- [ ] **Step 2: Create `tsconfig.json` and `vitest.config.ts`**

`scraper/tsconfig.json`:
```json
{
  "compilerOptions": {
    "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler",
    "strict": true, "esModuleInterop": true, "skipLibCheck": true, "types": ["node"]
  },
  "include": ["src", "test"]
}
```
`scraper/vitest.config.ts`:
```ts
import { defineConfig } from "vitest/config";
export default defineConfig({ test: { environment: "node" } });
```

- [ ] **Step 3: Create `.env.example`**

```
BOT_API_URL=http://localhost:8080
BOT_API_TOKEN=changeme
HYPERLIQUID_URL=https://app.hyperliquid.xyz
NEUROBRO_URL=https://app.neurobro.ai
NEUROBRO_STORAGE_STATE=./neurobro-session.json
POLL_INTERVAL_SECS=60
COOLDOWN_SECS=300
MAX_DEVIATION=0.004
```

- [ ] **Step 4: Write the failing config test**

`scraper/test/config.test.ts`:
```ts
import { describe, it, expect } from "vitest";
import { loadConfig } from "../src/config.js";

describe("loadConfig", () => {
  const base = { BOT_API_URL: "http://b", BOT_API_TOKEN: "t" };
  it("applies defaults for optional keys", () => {
    const c = loadConfig(base);
    expect(c.pollIntervalSecs).toBe(60);
    expect(c.maxDeviation).toBeCloseTo(0.004);
    expect(c.neurobroUrl).toContain("neurobro");
  });
  it("throws when a required key is missing", () => {
    expect(() => loadConfig({ BOT_API_URL: "http://b" })).toThrow(/BOT_API_TOKEN/);
  });
});
```

- [ ] **Step 5: Run test to verify it fails**

Run: `cd scraper && npm install && npm test`
Expected: FAIL — cannot find `../src/config.js`.

- [ ] **Step 6: Implement `src/config.ts`**

```ts
export interface Config {
  botApiUrl: string; botApiToken: string;
  hyperliquidUrl: string; neurobroUrl: string; storageStatePath: string;
  pollIntervalSecs: number; cooldownSecs: number; maxDeviation: number;
}

function required(env: Record<string, string | undefined>, key: string): string {
  const v = env[key];
  if (!v) throw new Error(`missing required env: ${key}`);
  return v;
}

export function loadConfig(env: Record<string, string | undefined>): Config {
  return {
    botApiUrl: required(env, "BOT_API_URL"),
    botApiToken: required(env, "BOT_API_TOKEN"),
    hyperliquidUrl: env.HYPERLIQUID_URL ?? "https://app.hyperliquid.xyz",
    neurobroUrl: env.NEUROBRO_URL ?? "https://app.neurobro.ai",
    storageStatePath: env.NEUROBRO_STORAGE_STATE ?? "./neurobro-session.json",
    pollIntervalSecs: Number(env.POLL_INTERVAL_SECS ?? "60"),
    cooldownSecs: Number(env.COOLDOWN_SECS ?? "300"),
    maxDeviation: Number(env.MAX_DEVIATION ?? "0.004"),
  };
}
```

- [ ] **Step 7: Run test + commit**

Run: `cd scraper && npm test` → PASS (2 tests).
```bash
git add scraper/package.json scraper/tsconfig.json scraper/vitest.config.ts scraper/.env.example scraper/src/config.ts scraper/test/config.test.ts
git commit -m "feat(scraper): project scaffold + config loader"
```

---

### Task 2: Neurobro card parser (`extract.ts`) — pure, TDD

**Files:**
- Create: `scraper/src/extract.ts`, `scraper/test/extract.test.ts`. Uses committed fixture `scraper/test/fixtures/neurobro-card.html`.

**Interfaces:**
- Produces: `export interface Setup { coin: string; direction: "long"|"short"; entry: number; stopLoss: number; takeProfit: number; confidence: number; thesis: string }` and `export function extractSetup(html: string): Setup | null` (returns `null` if any required field is missing/unparseable — fail-closed).

- [ ] **Step 1: Write the failing tests**

`scraper/test/extract.test.ts`:
```ts
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { extractSetup } from "../src/extract.js";

const fixture = readFileSync(new URL("./fixtures/neurobro-card.html", import.meta.url), "utf8");

describe("extractSetup", () => {
  it("parses the real SOL card", () => {
    const s = extractSetup(fixture)!;
    expect(s.coin).toBe("SOL");
    expect(s.direction).toBe("long");
    expect(s.entry).toBeCloseTo(69.32);
    expect(s.stopLoss).toBeCloseTo(68.98);
    expect(s.takeProfit).toBeCloseTo(70.10);
    expect(s.confidence).toBe(7);
    expect(s.thesis.length).toBeGreaterThan(10);
  });
  it("returns null when a price is missing (fail-closed)", () => {
    const broken = fixture.replace("$68.98", "n/a");
    expect(extractSetup(broken)).toBeNull();
  });
  it("returns null when there is no setup card", () => {
    expect(extractSetup("<div>hello</div>")).toBeNull();
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd scraper && npm test extract`
Expected: FAIL — cannot find `../src/extract.js`.

- [ ] **Step 3: Implement `src/extract.ts`**

```ts
import * as cheerio from "cheerio";

export interface Setup {
  coin: string; direction: "long" | "short";
  entry: number; stopLoss: number; takeProfit: number;
  confidence: number; thesis: string;
}

/** Parses "$69.32" / "69.32" → 69.32; returns NaN if no number. */
function money(text: string | undefined): number {
  if (!text) return NaN;
  const m = text.replace(/,/g, "").match(/-?\d+(\.\d+)?/);
  return m ? Number(m[0]) : NaN;
}

/** Finds the `$`-price text in the row that contains a label span equal to `label`. */
function priceForLabel($: cheerio.CheerioAPI, label: string): number {
  const labelSpan = $("span").filter((_, el) => $(el).text().trim() === label).first();
  if (!labelSpan.length) return NaN;
  const row = labelSpan.closest("div.flex-shrink-0");
  const price = row.find("span").filter((_, el) => $(el).text().trim().startsWith("$")).first();
  return money(price.text());
}

export function extractSetup(html: string): Setup | null {
  const $ = cheerio.load(html);

  // Must be a setup card.
  const heading = $("h4").filter((_, el) => $(el).text().includes("Setup trading untuk")).first();
  if (!heading.length) return null;
  const coin = heading.text().replace(/.*untuk\s+/i, "").trim().toUpperCase();
  if (!coin) return null;

  // Direction: the "Arah" card's bold value.
  const arahLabel = $("div").filter((_, el) => $(el).text().trim() === "Arah").first();
  const arahValue = arahLabel.closest("[data-slot='card']").find(".font-bold").text().toUpperCase();
  const direction = arahValue.includes("LONG") ? "long" : arahValue.includes("SHORT") ? "short" : null;

  // Confidence: first "{n}/10".
  const confMatch = $.root().text().match(/(\d{1,2})\s*\/\s*10/);
  const confidence = confMatch ? Number(confMatch[1]) : NaN;

  const entry = priceForLabel($, "Masuk");
  const stopLoss = priceForLabel($, "SL");
  const takeProfit = priceForLabel($, "TP1");
  const thesis = $("p").filter((_, el) => $(el).text().includes("$") || $(el).text().length > 20).first().text().trim();

  if (!direction || [entry, stopLoss, takeProfit, confidence].some((n) => !Number.isFinite(n))) {
    return null; // fail-closed
  }
  return { coin, direction, entry, stopLoss, takeProfit, confidence, thesis };
}
```

- [ ] **Step 4: Run tests + commit**

Run: `cd scraper && npm test extract` → PASS (3 tests).
```bash
git add scraper/src/extract.ts scraper/test/extract.test.ts
git commit -m "feat(scraper): Neurobro setup-card parser (fail-closed)"
```

---

### Task 3: Bot API client (`botApi.ts`)

**Files:**
- Create: `scraper/src/botApi.ts`, `scraper/test/botApi.test.ts`.

**Interfaces:**
- Consumes: `Config` (Task 1), `Setup` (Task 2).
- Produces: `export interface OpenPosition { coin: string }`, `export class BotApi { constructor(cfg: Config); getWatchlist(): Promise<{coins:string[]; autoScalpEnabled:boolean; maxOpenPositions:number}>; getPositions(): Promise<OpenPosition[]>; execute(setup: Setup): Promise<{ok:boolean; status:number}> }`. `execute` maps `Setup` → the bot body `{coin, direction, entry, stop_loss, take_profit, confidence, thesis}` and never throws on a 4xx (returns `{ok:false,status}`); it throws only on network failure.

- [ ] **Step 1: Write the failing test (against a local stub server)**

`scraper/test/botApi.test.ts`:
```ts
import { describe, it, expect, afterEach } from "vitest";
import { createServer, Server } from "node:http";
import { BotApi } from "../src/botApi.js";

let server: Server;
afterEach(() => server?.close());

function stub(handler: (url: string, body: string) => { status: number; json: unknown }) {
  return new Promise<string>((resolve) => {
    server = createServer((req, res) => {
      let body = "";
      req.on("data", (c) => (body += c));
      req.on("end", () => {
        const r = handler(req.url ?? "", body);
        res.writeHead(r.status, { "content-type": "application/json" });
        res.end(JSON.stringify(r.json));
      });
    }).listen(0, () => resolve(`http://127.0.0.1:${(server.address() as any).port}`));
  });
}

const cfg = (url: string) => ({ botApiUrl: url, botApiToken: "t", hyperliquidUrl: "", neurobroUrl: "", storageStatePath: "", pollIntervalSecs: 60, cooldownSecs: 300, maxDeviation: 0.004 });

describe("BotApi", () => {
  it("getWatchlist parses the response", async () => {
    const url = await stub(() => ({ status: 200, json: { coins: ["BTC"], auto_scalp_enabled: true, max_open_positions: 5 } }));
    const api = new BotApi(cfg(url));
    const w = await api.getWatchlist();
    expect(w.coins).toEqual(["BTC"]);
    expect(w.autoScalpEnabled).toBe(true);
    expect(w.maxOpenPositions).toBe(5);
  });
  it("execute returns ok:false on a 4xx instead of throwing", async () => {
    const url = await stub(() => ({ status: 422, json: {} }));
    const api = new BotApi(cfg(url));
    const r = await api.execute({ coin: "SOL", direction: "long", entry: 1, stopLoss: 0.9, takeProfit: 1.1, confidence: 6, thesis: "x" });
    expect(r.ok).toBe(false);
    expect(r.status).toBe(422);
  });
});
```

- [ ] **Step 2: Run test → fail** (`cd scraper && npm test botApi`) — cannot find module.

- [ ] **Step 3: Implement `src/botApi.ts`**

```ts
import type { Config } from "./config.js";
import type { Setup } from "./extract.js";

export interface OpenPosition { coin: string }

export class BotApi {
  constructor(private cfg: Config) {}
  private headers() { return { authorization: `Bearer ${this.cfg.botApiToken}`, "content-type": "application/json" }; }

  async getWatchlist() {
    const res = await fetch(`${this.cfg.botApiUrl}/watchlist`, { headers: this.headers() });
    if (!res.ok) throw new Error(`watchlist ${res.status}`);
    const j = await res.json() as any;
    return { coins: j.coins as string[], autoScalpEnabled: j.auto_scalp_enabled as boolean, maxOpenPositions: j.max_open_positions as number };
  }

  async getPositions(): Promise<OpenPosition[]> {
    const res = await fetch(`${this.cfg.botApiUrl}/positions`, { headers: this.headers() });
    if (!res.ok) throw new Error(`positions ${res.status}`);
    const j = await res.json() as any;
    const arr = Array.isArray(j) ? j : (j.positions ?? []);
    return arr.map((p: any) => ({ coin: String(p.coin).toUpperCase() }));
  }

  async execute(setup: Setup): Promise<{ ok: boolean; status: number }> {
    const body = JSON.stringify({
      coin: setup.coin, direction: setup.direction, entry: setup.entry,
      stop_loss: setup.stopLoss, take_profit: setup.takeProfit,
      confidence: setup.confidence, thesis: setup.thesis,
    });
    const res = await fetch(`${this.cfg.botApiUrl}/execute`, { method: "POST", headers: this.headers(), body });
    return { ok: res.ok, status: res.status };
  }
}
```

> Verify the `GET /positions` JSON shape against `src/api/mod.rs` (it returns position objects with a `coin` field); adjust the `arr` mapping if the wrapper differs.

- [ ] **Step 4: Run test + commit**

Run: `cd scraper && npm test botApi` → PASS.
```bash
git add scraper/src/botApi.ts scraper/test/botApi.test.ts
git commit -m "feat(scraper): bot API client (watchlist/positions/execute)"
```

---

### Task 4: Loop logic — free coins, slippage, cooldown (pure)

**Files:**
- Create: `scraper/src/loop.ts` (pure helpers first), `scraper/test/loop.test.ts`.

**Interfaces:**
- Consumes: `Setup` (Task 2), `OpenPosition` (Task 3).
- Produces:
  - `export function freeCoins(watchlist: string[], open: OpenPosition[], cooldownUntil: Map<string, number>, now: number, maxOpen: number): string[]` — coins in the watchlist that have no open position, are not in cooldown, capped so `open.length + result.length <= maxOpen`.
  - `export function passesSlippage(mark: number, entry: number, maxDeviation: number): boolean` — `Math.abs(mark-entry)/entry <= maxDeviation`.

- [ ] **Step 1: Write the failing tests**

`scraper/test/loop.test.ts`:
```ts
import { describe, it, expect } from "vitest";
import { freeCoins, passesSlippage } from "../src/loop.js";

describe("freeCoins", () => {
  it("excludes coins with positions, cooldown, and respects the cap", () => {
    const wl = ["BTC", "ETH", "SOL", "AVAX"];
    const open = [{ coin: "BTC" }];
    const cd = new Map([["ETH", 2000]]);
    // maxOpen 3, 1 already open → at most 2 new; ETH cooled down; BTC held → [SOL, AVAX]
    expect(freeCoins(wl, open, cd, 1000, 3)).toEqual(["SOL", "AVAX"]);
  });
  it("cap leaves zero slots when already full", () => {
    expect(freeCoins(["BTC","ETH"], [{coin:"X"},{coin:"Y"}], new Map(), 0, 2)).toEqual([]);
  });
});

describe("passesSlippage", () => {
  it("passes within deviation and fails beyond", () => {
    expect(passesSlippage(100.3, 100, 0.004)).toBe(true);   // 0.3%
    expect(passesSlippage(100.5, 100, 0.004)).toBe(false);  // 0.5%
  });
});
```

- [ ] **Step 2: Run test → fail** (`cd scraper && npm test loop`).

- [ ] **Step 3: Implement the pure helpers in `src/loop.ts`**

```ts
import type { OpenPosition } from "./botApi.js";

export function freeCoins(
  watchlist: string[], open: OpenPosition[], cooldownUntil: Map<string, number>,
  now: number, maxOpen: number,
): string[] {
  const held = new Set(open.map((p) => p.coin.toUpperCase()));
  const slots = Math.max(0, maxOpen - open.length);
  const eligible = watchlist
    .map((c) => c.toUpperCase())
    .filter((c) => !held.has(c))
    .filter((c) => (cooldownUntil.get(c) ?? 0) <= now);
  return eligible.slice(0, slots);
}

export function passesSlippage(mark: number, entry: number, maxDeviation: number): boolean {
  if (!Number.isFinite(mark) || !Number.isFinite(entry) || entry === 0) return false;
  return Math.abs(mark - entry) / entry <= maxDeviation;
}
```

- [ ] **Step 4: Run test + commit**

Run: `cd scraper && npm test loop` → PASS.
```bash
git add scraper/src/loop.ts scraper/test/loop.test.ts
git commit -m "feat(scraper): pure loop logic (free coins, slippage, cooldown)"
```

---

### Task 5: Live browser modules — Hyperliquid screenshot, Neurobro, login

**Files:**
- Create: `scraper/src/hyperliquid.ts`, `scraper/src/neurobro.ts`, `scraper/src/login.ts`.

**Interfaces:**
- Consumes: `Config` (Task 1), Playwright `Page`/`BrowserContext`.
- Produces:
  - `export async function screenshotChart(page: Page, cfg: Config, coin: string): Promise<Buffer>`
  - `export async function readMark(page: Page): Promise<number>` (current mark price from the HL trade page)
  - `export async function requestSetup(page: Page, cfg: Config, coin: string, screenshot: Buffer): Promise<string>` (returns the setup card's outerHTML)
  - `export async function login(): Promise<void>` (headed; saves storageState)

> These are LIVE browser flows; selectors must be validated against the real sites. They are NOT unit-tested. Verification is `npm run login` (one-time) then `npm run dry-run` (Task 6) printing a parsed setup. Keep each function small and log every step.

- [ ] **Step 1: Implement `src/hyperliquid.ts`**

```ts
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
```

> The page `<title>` is `app.hyperliquid.xyz / <price> | <COIN>` (confirmed in the captured screenshot), which is the most stable mark-price source — prefer it over a DOM selector. Confirm live.

- [ ] **Step 2: Implement `src/neurobro.ts`**

```ts
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
  const input = page.locator('textarea[name="input"]');
  await input.fill(PROMPT(coin));
  await input.press("Enter");

  // Wait for the "Setup Trading" card to render, then return its outerHTML.
  const card = page.locator('div:has(> button span:text-is("Setup Trading"))').last();
  await card.waitFor({ state: "visible", timeout: 120_000 });
  await page.waitForTimeout(2000); // allow the card values to finish animating in
  return await card.evaluate((el) => el.outerHTML);
}
```

> The card locator and the upload step are the two selectors most likely to need live tweaking. The prompt text is fixed (matches the spec).

- [ ] **Step 3: Implement `src/login.ts`**

```ts
import { chromium } from "playwright";
import { loadConfig } from "./config.js";

// One-time, HEADED: log in to Neurobro by hand (OTP "send code"), then this saves
// the session so the loop can run headless. Run: `npm run login`.
async function main() {
  const cfg = loadConfig(process.env);
  const browser = await chromium.launch({ headless: false });
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.goto(cfg.neurobroUrl);
  console.log("Log in to Neurobro in the opened window, then press Enter here to save the session…");
  await new Promise<void>((resolve) => process.stdin.once("data", () => resolve()));
  await context.storageState({ path: cfg.storageStatePath });
  console.log(`Saved session to ${cfg.storageStatePath}`);
  await browser.close();
}
main();
```

- [ ] **Step 4: Validate live (manual)**

Run: `cd scraper && cp .env.example .env` (fill `BOT_API_*`), then `npm run login` and log in once. Confirm `neurobro-session.json` is written. Full validation happens in Task 6's dry-run.

- [ ] **Step 5: Commit**

```bash
git add scraper/src/hyperliquid.ts scraper/src/neurobro.ts scraper/src/login.ts
git commit -m "feat(scraper): live browser modules (HL screenshot, Neurobro, login)"
```

---

### Task 6: Orchestration + entry point + dry-run

**Files:**
- Modify: `scraper/src/loop.ts` (add `runOnce`/`runForever`).
- Create: `scraper/src/index.ts`.

**Interfaces:**
- Consumes: everything above.
- Produces: `export async function runOnce(deps): Promise<void>` and `export async function runForever(deps): Promise<void>`; `index.ts` wires real deps and honors a `--dry-run` flag (skips `execute`, logs the parsed setup + slippage decision).

- [ ] **Step 1: Add `runOnce`/`runForever` to `src/loop.ts`**

```ts
import type { BotApi } from "./botApi.js";
import type { Page } from "playwright";
import type { Config } from "./config.js";
import { extractSetup } from "./extract.js";
import { screenshotChart, readMark, requestSetup } from "./hyperliquid.js"; // readMark from hyperliquid
import { requestSetup as neurobroRequestSetup } from "./neurobro.js";

export interface RunDeps { cfg: Config; api: BotApi; hlPage: Page; nbPage: Page; cooldownUntil: Map<string, number>; now: () => number; dryRun: boolean; }

export async function runOnce(d: RunDeps): Promise<void> {
  const wl = await d.api.getWatchlist();
  if (!wl.autoScalpEnabled) { console.log("auto_scalp disabled — idle"); return; }
  const open = await d.api.getPositions();
  const coins = freeCoins(wl.coins, open, d.cooldownUntil, d.now(), wl.maxOpenPositions);
  for (const coin of coins) {
    try {
      const shot = await screenshotChart(d.hlPage, d.cfg, coin);
      const mark = await readMark(d.hlPage);
      const html = await neurobroRequestSetup(d.nbPage, d.cfg, coin, shot);
      const setup = extractSetup(html);
      if (!setup) { console.warn(`${coin}: no parseable setup — skip`); cooldown(d, coin); continue; }
      if (setup.confidence < 7) { console.log(`${coin}: conf ${setup.confidence} < 7 — skip`); cooldown(d, coin); continue; }
      if (!passesSlippage(mark, setup.entry, d.cfg.maxDeviation)) {
        console.log(`${coin}: slippage mark=${mark} entry=${setup.entry} — skip`); cooldown(d, coin); continue;
      }
      if (d.dryRun) { console.log(`[dry-run] would execute`, setup); continue; }
      const r = await d.api.execute(setup);
      console.log(`${coin}: execute → ${r.status} ok=${r.ok}`);
      if (!r.ok) cooldown(d, coin);
    } catch (e) { console.error(`${coin}: error`, e); cooldown(d, coin); }
  }
}

function cooldown(d: RunDeps, coin: string) { d.cooldownUntil.set(coin.toUpperCase(), d.now() + d.cfg.cooldownSecs); }

export async function runForever(d: RunDeps): Promise<void> {
  for (;;) {
    try { await runOnce(d); } catch (e) { console.error("runOnce failed", e); }
    await new Promise((r) => setTimeout(r, d.cfg.pollIntervalSecs * 1000));
  }
}
```

> Remove the unused `requestSetup`/`readMark` import collision — import `readMark`+`screenshotChart` from `./hyperliquid.js` and the setup fn from `./neurobro.js` (aliased `neurobroRequestSetup`). Fix imports so the file compiles (`npx tsc --noEmit`).

- [ ] **Step 2: Create `src/index.ts`**

```ts
import { chromium } from "playwright";
import { loadConfig } from "./config.js";
import { BotApi } from "./botApi.js";
import { runForever, runOnce } from "./loop.js";

async function main() {
  const cfg = loadConfig(process.env);
  const dryRun = process.argv.includes("--dry-run");
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ storageState: cfg.storageStatePath });
  const hlPage = await context.newPage();
  const nbPage = await context.newPage();
  const deps = { cfg, api: new BotApi(cfg), hlPage, nbPage, cooldownUntil: new Map<string, number>(), now: () => Date.now() / 1000, dryRun };
  if (dryRun) { await runOnce(deps); await browser.close(); return; }
  await runForever(deps);
}
main().catch((e) => { console.error(e); process.exit(1); });
```

- [ ] **Step 3: Type-check + dry-run (live validation)**

Run: `cd scraper && npx tsc --noEmit` → no errors.
Run (after `npm run login`): `npm run dry-run` with at least one coin in the bot watchlist and `auto_scalp_enabled` ON. Expected: logs a parsed setup (or a skip reason) for each free coin and `[dry-run] would execute` WITHOUT calling `/execute`. Tune the Neurobro upload/card selectors here until the parse succeeds.

- [ ] **Step 4: Commit**

```bash
git add scraper/src/loop.ts scraper/src/index.ts
git commit -m "feat(scraper): orchestration loop + entry point with --dry-run"
```

---

### Task 7: Dockerfile + deploy

**Files:**
- Create: `scraper/Dockerfile`. Modify: k8s manifests (`k8s/`) to add the scraper container with the storageState mounted from the existing PVC.

**Interfaces:**
- Consumes: the built scraper. Produces: a deployable container that runs `npm start`.

- [ ] **Step 1: Create `scraper/Dockerfile`**

```dockerfile
FROM mcr.microsoft.com/playwright:v1.48.0-jammy
WORKDIR /app
COPY scraper/package.json scraper/package-lock.json* ./
RUN npm install --omit=dev && npm install tsx typescript
COPY scraper/ ./
CMD ["npx", "tsx", "src/index.ts"]
```

- [ ] **Step 2: Add a k8s Deployment for the scraper**

Add `k8s/scraper-deployment.yaml` modeled on the existing `k8s/deployment.yaml`: one replica, env from the same secret (add `BOT_API_URL` pointing at the bot service, `BOT_API_TOKEN` = the portfolio API token), and mount the existing PVC at a path holding `neurobro-session.json` (set `NEUROBRO_STORAGE_STATE` to that path). Copy the structure from `k8s/deployment.yaml` and `k8s/pvc.yaml`; reuse the namespace.

> Bootstrap the session once by running `npm run login` locally and copying `neurobro-session.json` onto the PVC (e.g. via `kubectl cp`), since OTP login can't run headless. Document this in `scraper/README.md`.

- [ ] **Step 3: Create `scraper/README.md`**

Document: env vars, `npm run login` (one-time, copy session to the PVC), `npm run dry-run`, how the loop works, and that `auto_scalp_enabled` must be turned ON via the bot's `/set auto_scalp_enabled on` for the loop to act.

- [ ] **Step 4: Commit**

```bash
git add scraper/Dockerfile k8s/scraper-deployment.yaml scraper/README.md
git commit -m "feat(scraper): Dockerfile + k8s deployment + README"
```

---

## Self-Review Notes

- **Spec coverage:** screenshot HL chart (Task 5), upload+prompt Neurobro (Task 5, prompt fixed), parse card incl. confidence (Task 2), confidence≥7 + slippage gates (Task 4+6), per-coin cooldown + free-slot/cap (Task 4+6), POST /execute via bot (Task 3), saved-session login (Task 5), 24/7 loop (Task 6), VPS deploy (Task 7). The two follow-ups from the bot final review — slippage guard (Task 4 `passesSlippage`, enforced Task 6) and per-coin dedupe + cap (Task 4 `freeCoins`, plus the in-loop re-check via `getPositions` each cycle) — are implemented.
- **Live-resolve items (cannot be unit-tested):** the Neurobro upload control (`+` menu → file input) and the setup-card locator — both validated in Task 6's dry-run. Flagged explicitly so the implementer expects iteration, not a clean first run.
- **Fail-closed:** `extractSetup` returns null on any missing field; the loop skips + cooldowns on null/low-conf/slippage/errors. Nothing executes on doubt.
- **Type consistency:** `Setup` (camelCase) used across extract/loop/botApi; `botApi.execute` maps to the bot's snake_case body; `OpenPosition.coin` consistent; `freeCoins`/`passesSlippage` signatures match their call in `runOnce`.
- **YAGNI:** no retry queue, no close-event subscription (the loop re-derives free slots from `GET /positions` each cycle), no multi-TP — single TP1 100% per the spec.
- **Open dependency (resolve live, Task 5/6):** the exact Neurobro image-upload flow after the `+` button — whether it reveals a menu item or a direct `input[type=file]`. The plan assumes `setInputFiles` on a revealed file input; capture the real DOM during the dry-run and adjust `neurobro.ts` only.
