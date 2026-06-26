import type { Config } from "./config.js";

/**
 * Sends an operator alert to Telegram (e.g. "Neurobro session expired"). No-ops with a
 * log line when TELEGRAM_BOT_TOKEN / TELEGRAM_CHAT_ID are unset, and never throws — a
 * failed notification must not crash the loop.
 */
export async function notifyTelegram(cfg: Config, text: string): Promise<void> {
  if (!cfg.telegramBotToken || !cfg.telegramChatId) {
    console.log(`[notify skipped — no Telegram config] ${text}`);
    return;
  }
  try {
    const res = await fetch(`https://api.telegram.org/bot${cfg.telegramBotToken}/sendMessage`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ chat_id: cfg.telegramChatId, text }),
    });
    if (!res.ok) console.error(`telegram notify failed: ${res.status}`);
  } catch (error) {
    console.error("telegram notify error", error);
  }
}
