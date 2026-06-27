export interface Config {
  botApiUrl: string; botApiToken: string;
  hyperliquidUrl: string; hyperliquidInfoUrl: string; neurobroUrl: string; storageStatePath: string;
  hlTimeframe: string; hlTimeframeHtf: string;
  atrInterval: string; atrPeriod: number;
  userDataDir: string; headless: boolean; browserChannel: string;
  pollIntervalSecs: number; cooldownSecs: number; maxDeviation: number;
  minRiskReward: number; maxStopLossPct: number;
  telegramBotToken: string; telegramChatId: string;
  maxAnalysesPerDay: number; maxAnalysesPerCycle: number; quotaStatePath: string;
}

function required(env: Record<string, string | undefined>, key: string): string {
  const v = env[key];
  if (!v) throw new Error(`missing required env: ${key}`);
  return v;
}

/**
 * Resolves the Telegram chat to notify. Prefers an explicit TELEGRAM_CHAT_ID; if
 * unset, falls back to the first id in TELEGRAM_ALLOWED_USER_IDS (the bot's
 * comma-separated allow-list) so notifications work from the shared secret without
 * extra config — in a private chat, chat id == user id.
 */
function resolveChatId(env: Record<string, string | undefined>): string {
  const explicit = (env.TELEGRAM_CHAT_ID ?? "").trim();
  if (explicit) return explicit;
  const firstAllowed = (env.TELEGRAM_ALLOWED_USER_IDS ?? "")
    .split(",")
    .map((id) => id.trim())
    .find((id) => id.length > 0);
  return firstAllowed ?? "";
}

export function loadConfig(env: Record<string, string | undefined>): Config {
  return {
    botApiUrl: required(env, "BOT_API_URL"),
    botApiToken: required(env, "BOT_API_TOKEN"),
    hyperliquidUrl: env.HYPERLIQUID_URL ?? "https://app.hyperliquid.xyz",
    // Public, auth-free info endpoint used to fetch recent candles for a volatility (ATR)
    // hint injected into the Neurobro prompt. Best-effort: a failure just omits the hint.
    hyperliquidInfoUrl: env.HYPERLIQUID_INFO_URL ?? "https://api.hyperliquid.xyz/info",
    // TradingView date-range tab the scraper clicks before screenshotting for Neurobro.
    // The button's value/data-name="date-range-tab-<VALUE>" is UPPERCASE (1D, 5D, 1M, 3M,
    // 6M, 12M, 60M) even though the visible label is lowercase. "1D" = 1 day of 1-minute
    // candles — the lower timeframe for entry timing. Matching is case-insensitive anyway.
    hlTimeframe: env.HL_TIMEFRAME ?? "1D",
    // Higher-timeframe range captured as a SECOND screenshot for trend-bias context. "5D"
    // = 5 days of 5-minute candles, one step up from the 1-minute LTF. Both images go to
    // Neurobro (HTF first, LTF second) so it aligns entries with the larger trend. Bump to
    // "1M" (30-min candles) for a stronger trend view; set empty ("") to disable.
    hlTimeframeHtf: env.HL_TIMEFRAME_HTF ?? "5D",
    // Candle interval + period for the ATR volatility hint (via the public info API).
    atrInterval: env.ATR_INTERVAL ?? "5m",
    atrPeriod: Number(env.ATR_PERIOD ?? "14"),
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
    // Deterministic guardrails applied to every parsed setup before execution, independent
    // of the model's own claims: reject anything below this risk:reward, or whose stop
    // distance exceeds this fraction of entry (a too-wide SL = tiny size / wrong read).
    minRiskReward: Number(env.MIN_RISK_REWARD ?? "1.5"),
    maxStopLossPct: Number(env.MAX_STOP_LOSS_PCT ?? "0.03"),
    // Optional: alert the operator when the Neurobro session dies (Cloudflare/login wall).
    // Reuse the bot's Telegram bot token + your user/chat id. Empty → alerts just logged.
    telegramBotToken: env.TELEGRAM_BOT_TOKEN ?? "",
    telegramChatId: resolveChatId(env),
    // Neurobro quota guard. Each chart analysis = 1 "light" chat; the plan grants ~100
    // light/day. Hard daily cap (persisted, resets daily) so the loop never overspends;
    // per-cycle cap spreads usage instead of burning the budget in the first minutes.
    // Lower maxPerDay if you also use Neurobro manually (dry-run/once count too).
    maxAnalysesPerDay: Number(env.MAX_ANALYSES_PER_DAY ?? "100"),
    // 0 = unlimited per cycle (scan all eligible — burst at startup, recycle on close).
    // The daily cap is the real budget guard; cooldown paces per-coin re-checks.
    maxAnalysesPerCycle: Number(env.MAX_ANALYSES_PER_CYCLE ?? "0"),
    quotaStatePath: env.NEUROBRO_QUOTA_STATE ?? "./neurobro-quota.json",
  };
}
