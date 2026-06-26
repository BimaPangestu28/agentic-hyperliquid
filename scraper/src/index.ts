import { chromium } from "playwright";
import { loadConfig } from "./config.js";
import { BotApi } from "./botApi.js";
import { runForever, runOnce } from "./loop.js";

async function main(): Promise<void> {
  const cfg = loadConfig(process.env);
  const dryRun = process.argv.includes("--dry-run");
  const browser = await chromium.launch({ headless: true });

  // Graceful shutdown: k8s sends SIGTERM on pod stop. Close the browser cleanly
  // so Chromium does not leave lock files or zombie processes behind.
  for (const sig of ["SIGTERM", "SIGINT"] as const) {
    process.on(sig, () => { browser.close().finally(() => process.exit(0)); });
  }

  const context = await browser.newContext({ storageState: cfg.storageStatePath });
  const hlPage = await context.newPage();
  const nbPage = await context.newPage();

  const deps = {
    cfg,
    api: new BotApi(cfg),
    hlPage,
    nbPage,
    cooldownUntil: new Map<string, number>(),
    now: () => Date.now() / 1000,
    dryRun,
  };

  if (dryRun) {
    await runOnce(deps);
    await browser.close();
    return;
  }

  await runForever(deps);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
