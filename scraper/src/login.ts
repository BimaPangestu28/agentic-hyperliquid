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
