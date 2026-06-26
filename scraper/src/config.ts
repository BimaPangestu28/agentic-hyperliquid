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
