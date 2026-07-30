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

/** Send a configured login/chat message shortly after spawn (e.g. "/login <pw>"). */
export function sendLogin(bot: Bot, message: string): void {
  setTimeout(() => bot.chat(message), 1000);
}
