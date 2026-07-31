import { createRequire } from "node:module";
import type { Bot } from "mineflayer";

// One place for the CommonJS Mineflayer stack (createRequire avoids ESM named-export pitfalls).
const require = createRequire(import.meta.url);
export const mineflayer = require("mineflayer");
export const { pathfinder, Movements, goals } = require("mineflayer-pathfinder");
export const collectBlock = require("mineflayer-collectblock").plugin;
export const pvp = require("mineflayer-pvp").plugin;
export const { Vec3 } = require("vec3");
export const mcDataLoader = require("minecraft-data");

export const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));

/** Reject a promise if it doesn't settle in time — every world action is time-boxed. */
export function withTimeout<T>(p: Promise<T>, ms: number, label = "action"): Promise<T> {
  return Promise.race([
    p,
    new Promise<T>((_, reject) => setTimeout(() => reject(new Error(`${label} timed out after ${ms}ms`)), ms)),
  ]);
}

/** Nearest hostile mob (optionally within `range`). Shared by attack/fight/flee/survival. */
export function nearestHostile(bot: Bot, range?: number): any {
  const p = (bot as any).entity?.position;
  return (bot as any).nearestEntity(
    (e: any) => e.type === "mob" && e.kind === "Hostile mobs" && (range == null || (p && e.position.distanceTo(p) <= range)),
  );
}

/** Cumulative bytes on a bot's Minecraft TCP socket (on-wire, i.e. compressed/encrypted). */
export function socketBytes(bot: any): { in: number; out: number } {
  const s = bot?._client?.socket;
  return { in: s?.bytesRead ?? 0, out: s?.bytesWritten ?? 0 };
}

/** Append to a capped ring-buffer log. */
export function logLine(log: string[], msg: string): void {
  log.push(`${new Date().toISOString()} ${msg}`);
  if (log.length > 100) log.shift();
}

/** Disconnect a bot, tolerating a partially-initialized client (quit may not be attached yet). */
export function safeQuit(bot: any): void {
  try {
    if (typeof bot?.quit === "function") bot.quit();
    else if (typeof bot?.end === "function") bot.end();
    else bot?._client?.end?.();
  } catch {
    /* already gone */
  }
}

/**
 * Send the configured login message(s) shortly after spawn (e.g. "/login <pw>").
 * Newline-separated lines are sent in order (e.g. "/register <pw> <pw>\n/login <pw>").
 */
export function sendLogin(bot: Bot, message: string, note?: (m: string) => void): void {
  const lines = message.split("\n").map((s) => s.trim()).filter(Boolean);
  if (!lines.length) { note?.("no login message configured"); return; }
  lines.forEach((line, i) =>
    setTimeout(() => {
      try { bot.chat(line); note?.(`sent login: ${line}`); }
      catch (e) { note?.(`login send failed: ${(e as Error).message}`); }
    }, 1000 + i * 700),
  );
}

/** Mirror server/system chat into a log so auth prompts and rejections are visible. */
export function logServerMessages(bot: Bot, note: (m: string) => void): void {
  (bot as any).on("messagestr", (message: string) => {
    const s = String(message).trim();
    if (s) note(`srv: ${s.slice(0, 180)}`);
  });
}

/**
 * Backed-off reconnect with an attempt cap, shared by the dispatcher and workers.
 * A connection that stays up long enough resets the backoff; too many quick failures give up.
 */
export class Reconnector {
  private attempts = 0;
  private stableTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    private readonly maxAttempts: number,
    private readonly onReconnect: () => void,
    private readonly onGiveUp: (attempts: number) => void,
  ) {}

  /** Call once a connection is live; if it survives `stableMs`, the backoff resets. */
  markConnected(stableMs = 20000): void {
    if (this.stableTimer) clearTimeout(this.stableTimer);
    this.stableTimer = setTimeout(() => (this.attempts = 0), stableMs);
  }

  /** Call from "end". Reconnects after a growing delay (capped 30s) if `shouldReconnect`, else gives up. */
  scheduleReconnect(shouldReconnect: () => boolean): number {
    if (this.stableTimer) { clearTimeout(this.stableTimer); this.stableTimer = null; }
    this.attempts++;
    if (this.attempts > this.maxAttempts) { this.onGiveUp(this.attempts); return 0; }
    const delay = Math.min(30000, 2000 * 2 ** (this.attempts - 1));
    setTimeout(() => { if (shouldReconnect()) this.onReconnect(); }, delay);
    return delay;
  }

  reset(): void {
    this.attempts = 0;
    if (this.stableTimer) { clearTimeout(this.stableTimer); this.stableTimer = null; }
  }
}
