import { chromium } from "playwright";
import { loadConfig } from "./config.js";

// One-time, HEADED: solve the Cloudflare check + log in to Neurobro by hand (OTP
// "send code"). Uses the REAL installed Chrome and a PERSISTENT profile dir so the
// Cloudflare cf_clearance cookie and Neurobro auth survive into the loop's runs —
// Playwright's bundled "Chrome for Testing" is flagged by Cloudflare Turnstile and
// cannot pass the human check. Run: `npm run login`.
async function main() {
  const cfg = loadConfig(process.env);
  const context = await chromium.launchPersistentContext(cfg.userDataDir, {
    headless: false,
    channel: cfg.browserChannel,
    viewport: null,
    // Stop announcing automation (drops navigator.webdriver + the automation banner)
    // so a human-solved Cloudflare check can actually pass. Not fingerprint spoofing.
    args: ["--disable-blink-features=AutomationControlled"],
    ignoreDefaultArgs: ["--enable-automation"],
  });
  try {
    const page = context.pages()[0] ?? (await context.newPage());
    await page.goto(cfg.neurobroUrl);
    console.log(
      `Solve the Cloudflare "Verify you are human" check and log in to Neurobro in the\n` +
      `opened window, then press Enter here to persist the session…`,
    );
    await new Promise<void>((resolve) => process.stdin.once("data", () => resolve()));
    // Also export a storageState snapshot for tooling that wants it; the profile dir
    // is the source of truth the loop reuses.
    await context.storageState({ path: cfg.storageStatePath });
    console.log(`Session persisted to profile dir: ${cfg.userDataDir}`);
    console.log(`storageState snapshot written to: ${cfg.storageStatePath}`);
  } finally {
    await context.close();
  }
}
main();
