import { chromium } from "playwright";
import { loadConfig } from "./config.js";
import { BotApi } from "./botApi.js";
import { runForever, runOnce } from "./loop.js";

async function main(): Promise<void> {
  const cfg = loadConfig(process.env);
  const dryRun = process.argv.includes("--dry-run");

  // Reuse the persistent Chrome profile established by `npm run login` so the
  // Cloudflare cf_clearance cookie + Neurobro auth carry over. Real Chrome channel
  // (not Playwright's bundled Chromium) and default-headful both help pass Turnstile;
  // set HEADLESS=true only behind a virtual display (xvfb) on a server.
  const context = await chromium.launchPersistentContext(cfg.userDataDir, {
    headless: cfg.headless,
    channel: cfg.browserChannel,
    viewport: null,
    // Stop announcing automation (drops navigator.webdriver + the automation banner).
    args: ["--disable-blink-features=AutomationControlled"],
    ignoreDefaultArgs: ["--enable-automation"],
  });

  // Graceful shutdown: k8s sends SIGTERM on pod stop. Close the context cleanly so
  // Chromium does not leave profile lock files or zombie processes behind.
  for (const sig of ["SIGTERM", "SIGINT"] as const) {
    process.on(sig, () => { context.close().finally(() => process.exit(0)); });
  }

  const hlPage = await context.newPage();
  const nbPage = context.pages()[0] ?? (await context.newPage());

  const deps = {
    cfg,
    api: new BotApi(cfg),
    hlPage,
    nbPage,
    cooldownUntil: new Map<string, number>(),
    now: () => Date.now() / 1000,
    dryRun,
    sessionAlertSent: { value: false },
  };

  if (dryRun) {
    await runOnce(deps);
    await context.close();
    return;
  }

  await runForever(deps);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
