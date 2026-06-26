import { chromium } from "playwright";
import { loadConfig } from "./config.js";
import { BotApi } from "./botApi.js";
import { runForever, runOnce } from "./loop.js";

async function main(): Promise<void> {
  const cfg = loadConfig(process.env);
  const dryRun = process.argv.includes("--dry-run");
  const browser = await chromium.launch({ headless: true });
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
