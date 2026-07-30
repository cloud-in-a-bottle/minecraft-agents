import { readFileSync } from "node:fs";
import type { AppConfig, BotSpec, LlmConfig, McConfig } from "./types.js";

function env(name: string, fallback?: string): string {
  const v = process.env[name];
  if (v === undefined || v === "") {
    if (fallback !== undefined) return fallback;
    throw new Error(`missing required env var ${name}`);
  }
  return v;
}

/** Only Haiku 4.5 and Sonnet 5 are allowed (aliases accepted). */
const MODEL_ALIASES: Record<string, string> = {
  haiku: "claude-haiku-4-5",
  "claude-haiku-4-5": "claude-haiku-4-5",
  sonnet: "claude-sonnet-5",
  "claude-sonnet-5": "claude-sonnet-5",
};

function normalizeModel(m: string): string {
  const v = MODEL_ALIASES[m.trim().toLowerCase()];
  if (!v) throw new Error(`model "${m}" not allowed; use claude-haiku-4-5 (haiku) or claude-sonnet-5 (sonnet)`);
  return v;
}

/** Resolves the roster. Every agent is numbered agent_1..agent_N — the only naming scheme. */
function resolveBots(): BotSpec[] {
  const path = process.env.BOTS_CONFIG;
  if (path) {
    const specs = JSON.parse(readFileSync(path, "utf8")) as Array<{ goal?: string; model?: string }>;
    if (!Array.isArray(specs) || specs.length === 0)
      throw new Error("BOTS_CONFIG must be a non-empty JSON array of { goal?, model? }");
    return specs.map((s, i) => ({ username: `agent_${i + 1}`, goal: s.goal, model: s.model ? normalizeModel(s.model) : undefined }));
  }
  const count = Number(env("BOT_COUNT", "0"));
  return Array.from({ length: count }, (_, i) => ({ username: `agent_${i + 1}` }));
}

export function loadConfig(): AppConfig {
  const mc: McConfig = {
    host: env("MC_HOST", "localhost"),
    port: Number(env("MC_PORT", "25565")),
    version: process.env.MC_VERSION || undefined,
    auth: env("MC_AUTH", "offline") === "microsoft" ? "microsoft" : "offline",
    loginMessage: process.env.LOGIN_MESSAGE ?? "",
  };
  const llm: LlmConfig = {
    // Filled by resolveApiKey() at boot (env var for local dev, else OpenHost secrets).
    apiKey: process.env.ANTHROPIC_API_KEY ?? "",
    model: normalizeModel(env("LLM_MODEL", "claude-haiku-4-5")),
    effort: env("LLM_EFFORT", "low") as LlmConfig["effort"],
    maxSteps: Number(env("LLM_MAX_STEPS", "40")),
  };
  const commandAllowlist = (process.env.COMMAND_ALLOWLIST ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  return {
    port: Number(env("PORT", "8080")),
    mc,
    llm,
    bots: resolveBots(),
    dispatcherName: env("DISPATCHER_NAME", "agents"),
    commandAllowlist,
    maxBots: Number(env("MAX_BOTS", "20")),
    maxPerUser: Number(env("MAX_PER_USER", "5")),
  };
}
