import { createRequire } from "node:module";
import type { Bot } from "mineflayer";

// One place for the CommonJS Mineflayer stack (createRequire avoids ESM named-export pitfalls).
const require = createRequire(import.meta.url);
export const mineflayer = require("mineflayer");
export const { pathfinder, Movements, goals } = require("mineflayer-pathfinder");
export const collectBlock = require("mineflayer-collectblock").plugin;
export const pvp = require("mineflayer-pvp").plugin;
export const tool = require("mineflayer-tool").plugin;
export const { Vec3 } = require("vec3");
export const mcDataLoader = require("minecraft-data");

export const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));

/**
 * Equip the fastest tool for a block (mineflayer-tool). With requireHarvest, only a
 * tool that actually yields drops is equipped; returns false if the bot carries none.
 */
export async function equipBestTool(bot: any, block: any, requireHarvest = false): Promise<boolean> {
  if (!block || !bot.tool?.equipForBlock) return true; // no plugin: dig with whatever's in hand
  try {
    await bot.tool.equipForBlock(block, { requireHarvest });
    return true;
  } catch {
    return false; // requireHarvest and nothing carried can harvest it
  }
}

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

/**
 * Cap a bot's world copy: periodically drop loaded columns beyond `keepRadius`
 * chunks of the bot, reclaiming chunks the server never told it to unload (#1123).
 * `keepRadius` must be ≥ the server view-distance or blocks near the bot go missing.
 */
export function startChunkPrune(bot: any, keepRadius: number, intervalMs = 45000): void {
  const timer = setInterval(() => {
    try {
      const p = bot.entity?.position;
      const world = bot.world;
      if (!p || !world?.getColumns) return;
      const cx = Math.floor(p.x) >> 4;
      const cz = Math.floor(p.z) >> 4;
      for (const col of world.getColumns()) {
        const x = Number(col.chunkX); // getColumns() yields string coords
        const z = Number(col.chunkZ);
        if (Math.max(Math.abs(x - cx), Math.abs(z - cz)) > keepRadius) world.unloadColumn(x, z);
      }
    } catch {
      /* transient; retry next tick */
    }
  }, intervalMs);
  bot.once("end", () => clearInterval(timer));
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

const AUTH_PROMPT = /not authenticated|please (log ?in|login)|use \/login|\/l to auth|you have to (login|register)|register (first|to)|not logged in/i;
const AUTH_OK = /logged in|login successful|authentication successful|successfully (logged|registered|authenticated)|welcome back|session restored/i;
const AUTH_FAIL = /wrong password|incorrect|invalid password|not registered|must register|password.*(short|long|weak)|too many|max.*accounts/i;

/**
 * Authenticate after spawn: send the login message(s) on spawn AND whenever the
 * server prints an auth prompt, stopping once the server confirms success.
 * `getMessage` is read live; newline-separated lines are sent in order
 * (e.g. "/register <pw> <pw>\n/login <pw>" handles both fresh and existing accounts).
 */
export function installAuth(bot: Bot, getMessage: () => string, note: (m: string) => void): void {
  let sent = 0;
  let done = false;
  const send = (prompted: boolean): void => {
    if (done) return;
    const lines = getMessage().split(/\n|\s+&&\s+/).map((s) => s.trim()).filter(Boolean);
    if (!lines.length) {
      note(prompted ? "server requires login but none is configured — set it in the dashboard" : "no login message configured");
      return;
    }
    sent++;
    lines.forEach((line, i) =>
      setTimeout(() => {
        if (done) return;
        try { bot.chat(line); note(`sent login: ${line}`); }
        catch (e) { note(`login send failed: ${(e as Error).message}`); }
      }, i * 700),
    );
  };
  // Login plugins hold the bot in limbo (no "spawn") until authenticated, so send on
  // "login" (fires on join, pre-spawn) and on the auth prompt — never gated on spawn.
  bot.once("login", () => { setTimeout(() => send(false), 500); });
  (bot as any).on("messagestr", (m: string) => {
    const s = String(m);
    if (!done && AUTH_OK.test(s)) { done = true; note("authenticated ✓"); return; }
    if (AUTH_FAIL.test(s)) { note(`auth rejected: ${s.slice(0, 120)}`); return; }
    if (!done && sent < 6 && AUTH_PROMPT.test(s)) send(true);
  });
}

const KICK_FRIENDLY: Record<string, string> = {
  "multiplayer.disconnect.duplicate_login": "duplicate login (another connection with this name)",
  "multiplayer.disconnect.idling": "kicked for idling (AFK)",
  "multiplayer.disconnect.kicked": "kicked by an operator",
  "multiplayer.disconnect.server_shutdown": "server shut down",
  "multiplayer.disconnect.flying": "flying is not enabled on this server",
  "multiplayer.disconnect.slow_login": "login timed out",
};

/** Unwrap prismarine-nbt's {type,value} envelopes to plain values. */
function unwrapNbt(x: any): any {
  return x && typeof x === "object" && typeof x.type === "string" && "value" in x ? unwrapNbt(x.value) : x;
}

function flattenComponent(c: any): string {
  c = unwrapNbt(c);
  if (c == null) return "";
  if (typeof c === "string") return c;
  let s = String(unwrapNbt(c.text) ?? unwrapNbt(c.translate) ?? "");
  const extra = unwrapNbt(c.extra);
  if (Array.isArray(extra)) s += extra.map(flattenComponent).join("");
  const wth = unwrapNbt(c.with);
  if (Array.isArray(wth)) s += " " + wth.map(flattenComponent).join(" ");
  return s.trim() || JSON.stringify(c);
}

/** Readable text from a kick/disconnect reason (string, JSON string, chat component, or NBT). */
export function kickReason(reason: any): string {
  let r = reason;
  if (typeof r === "string") { try { r = JSON.parse(r); } catch { return r; } }
  const s = flattenComponent(r).trim();
  return KICK_FRIENDLY[s] ?? s;
}

/** Keep an idle bot from being AFK-kicked: nudge its view periodically. Returns a stop fn. */
export function antiAfk(bot: Bot, ms = 15000): () => void {
  const id = setInterval(() => {
    try { void bot.look(((bot.entity?.yaw ?? 0) + 0.1) % (Math.PI * 2), 0, false)?.catch(() => {}); } catch { /* not spawned */ }
  }, ms);
  return () => clearInterval(id);
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
  private pending: ReturnType<typeof setTimeout> | null = null;

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
    this.cancelPending(); // at most one reconnect ever in flight
    this.attempts++;
    if (this.attempts > this.maxAttempts) { this.onGiveUp(this.attempts); return 0; }
    const delay = Math.min(30000, 2000 * 2 ** (this.attempts - 1));
    this.pending = setTimeout(() => { this.pending = null; if (shouldReconnect()) this.onReconnect(); }, delay);
    return delay;
  }

  /** Cancel a scheduled reconnect without touching the backoff count. */
  cancelPending(): void {
    if (this.pending) { clearTimeout(this.pending); this.pending = null; }
  }

  reset(): void {
    this.attempts = 0;
    this.cancelPending();
    if (this.stableTimer) { clearTimeout(this.stableTimer); this.stableTimer = null; }
  }
}
