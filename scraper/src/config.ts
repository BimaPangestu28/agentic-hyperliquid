export interface Config {
  botApiUrl: string; botApiToken: string;
  hyperliquidUrl: string; neurobroUrl: string; storageStatePath: string;
  userDataDir: string; headless: boolean; browserChannel: string;
  pollIntervalSecs: number; cooldownSecs: number; maxDeviation: number;
  telegramBotToken: string; telegramChatId: string;
  maxAnalysesPerDay: number; maxAnalysesPerCycle: number; quotaStatePath: string;
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
    // Persistent Chrome profile dir: cf_clearance (Cloudflare) + Neurobro auth live
    // here so the loop reuses what `npm run login` established, without re-challenging.
    userDataDir: env.NEUROBRO_USER_DATA_DIR ?? "./neurobro-profile",
    // Default headful: Cloudflare Turnstile flags headless. On a VPS run under xvfb.
    headless: (env.HEADLESS ?? "false") === "true",
    // Real installed Chrome beats Playwright's "Chrome for Testing" against anti-bot.
    browserChannel: env.BROWSER_CHANNEL ?? "chrome",
    pollIntervalSecs: Number(env.POLL_INTERVAL_SECS ?? "60"),
    cooldownSecs: Number(env.COOLDOWN_SECS ?? "300"),
    maxDeviation: Number(env.MAX_DEVIATION ?? "0.004"),
    // Optional: alert the operator when the Neurobro session dies (Cloudflare/login wall).
    // Reuse the bot's Telegram bot token + your user/chat id. Empty → alerts just logged.
    telegramBotToken: env.TELEGRAM_BOT_TOKEN ?? "",
    telegramChatId: env.TELEGRAM_CHAT_ID ?? "",
    // Neurobro quota guard. Each chart analysis = 1 "light" chat; the plan grants ~100
    // light/day. Hard daily cap (persisted, resets daily) so the loop never overspends;
    // per-cycle cap spreads usage instead of burning the budget in the first minutes.
    // Lower maxPerDay if you also use Neurobro manually (dry-run/once count too).
    maxAnalysesPerDay: Number(env.MAX_ANALYSES_PER_DAY ?? "100"),
    maxAnalysesPerCycle: Number(env.MAX_ANALYSES_PER_CYCLE ?? "1"),
    quotaStatePath: env.NEUROBRO_QUOTA_STATE ?? "./neurobro-quota.json",
  };
}
