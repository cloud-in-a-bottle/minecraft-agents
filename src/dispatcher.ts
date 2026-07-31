import type { Bot } from "mineflayer";
import type { BatchResult, CreateResult, McConfig } from "./types.js";
import { Reconnector, antiAfk, installAuth, kickReason, logLine, logServerMessages, mineflayer, safeQuit, socketBytes, startChunkPrune } from "./deps.js";

export interface DispatchHandlers {
  createNew: (count: number, goal: string, owner: string) => CreateResult;
  assignExisting: (numbers: number[], goal: string, owner: string) => BatchResult;
  release: (numbers: number[], owner: string) => BatchResult;
  claim: (numbers: number[], owner: string) => BatchResult;
  give: (numbers: number[], owner: string, target: string) => BatchResult;
}

/** Parse "1 2 agent_3" (or a leading run of them) into numbers + trailing text; commas tolerated. */
function parseNumbers(str: string): number[] {
  return str
    .split(/[\s,]+/)
    .map((t) => t.replace(/^agent_/i, ""))
    .filter((t) => /^\d+$/.test(t))
    .map(Number);
}

function parseLeadingNumbers(str: string): { numbers: number[]; rest: string } {
  const tokens = str.split(/\s+/);
  const numbers: number[] = [];
  let i = 0;
  for (; i < tokens.length; i++) {
    const t = tokens[i].replace(/,$/, "").replace(/^agent_/i, "");
    if (/^\d+$/.test(t)) numbers.push(Number(t));
    else break;
  }
  return { numbers, rest: tokens.slice(i).join(" ") };
}

/** Always-on, non-interactable (spectator) player. Players tag it to manage workers. */
export class Dispatcher {
  private bot: Bot | null = null;
  private stopped = false;
  private stopAfk: (() => void) | null = null;
  private recycleTimer: ReturnType<typeof setInterval> | null = null;
  private mcInBase = 0;
  private mcOutBase = 0;
  private readonly log: string[] = [];
  private readonly reconnector = new Reconnector(
    8,
    () => this.start(),
    (n) => this.note(`gave up reconnecting after ${n} attempts (check host/login)`),
  );

  constructor(
    private readonly username: string,
    private readonly mc: McConfig,
    private readonly allowlist: string[],
    private readonly handlers: DispatchHandlers,
  ) {}

  private note(msg: string): void {
    logLine(this.log, msg);
  }

  start(): void {
    this.stopped = false;
    this.reconnector.cancelPending();
    const old = this.bot;
    const prev = socketBytes(old);
    this.mcInBase += prev.in;
    this.mcOutBase += prev.out;
    const bot: Bot = mineflayer.createBot({
      host: this.mc.host,
      port: this.mc.port,
      username: this.username,
      version: this.mc.version,
      auth: this.mc.auth,
      viewDistance: "tiny", // spectator needs only chat, not the world
      physicsEnabled: false, // never moves; skip the physics tick
    });
    this.bot = bot;
    bot.once("spawn", () => {
      this.reconnector.markConnected();
      this.note("dispatcher online");
      this.stopAfk = antiAfk(bot); // idle spectator would otherwise be AFK-kicked
      startChunkPrune(bot, this.mc.chunkKeepRadius);
    });
    logServerMessages(bot, (m) => this.note(m));
    installAuth(bot, () => this.mc.loginMessage, (m) => this.note(m));
    bot.on("chat", (username, message) => this.onChat(username, message));
    bot.on("kicked", (reason) => this.note(`kicked: ${kickReason(reason)}`));
    bot.on("error", (err) => this.note(`error: ${err.message}`));
    bot.on("end", () => {
      if (this.bot !== bot) return; // superseded by a newer connection; ignore
      this.stopAfk?.();
      this.stopAfk = null;
      if (this.stopped) return;
      const delay = this.reconnector.scheduleReconnect(() => !this.stopped && this.bot === bot);
      if (delay) this.note(`disconnected; reconnecting in ${delay / 1000}s`);
    });
    if (old) safeQuit(old); // its "end" is ignored (this.bot now points to the new one)
  }

  private reply(username: string, msg: string): void {
    this.bot?.chat(`${username} ${msg}`);
    this.note(`${username}: ${msg}`);
  }

  private skippedNote(r: BatchResult): string {
    return r.skipped.length ? ` (skipped ${r.skipped.map((s) => `${s.name}: ${s.reason}`).join(", ")})` : "";
  }

  private onChat(username: string, message: string): void {
    if (username === this.username) return;
    const escaped = this.username.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const match = message.trim().match(new RegExp(`^@${escaped}\\b[\\s:,-]*(.*)$`, "i"));
    if (!match) return;
    if (this.allowlist.length && !this.allowlist.includes(username)) {
      this.reply(username, "not allowed to command bots");
      return;
    }
    const rest = match[1].trim();
    if (!rest) return this.reply(username, `usage: @${this.username} new [n] <task> | <n> [n…] <task> | release/claim/reserve <n> [n…] | give <n> [n…] <player>`);

    const cmd = rest.match(/^(new|release|claim|reserve|give)\b\s*(.*)$/i);
    const keyword = cmd?.[1].toLowerCase();
    const args = cmd?.[2].trim() ?? "";

    if (keyword === "new") {
      const m = args.match(/^(?:(\d+)\s+)?(.+)$/);
      if (!m) return this.reply(username, "usage: @" + this.username + " new [n] <task>");
      const count = m[1] ? Number(m[1]) : 1;
      const goal = m[2].trim();
      const r = this.handlers.createNew(count, goal, username);
      const limit = r.reason === "user_limit" ? "your agent limit reached" : "agent limit reached";
      return this.reply(
        username,
        r.created.length
          ? `created ${r.created.join(", ")} on: ${goal}` + (r.rejected ? ` — ${r.rejected} not summoned (${limit})` : "")
          : `cannot summon — ${limit}`,
      );
    }

    // reserve = claim: log the caller as owner of those numbers without connecting them yet.
    if (keyword === "release" || keyword === "claim" || keyword === "reserve") {
      const numbers = parseNumbers(args);
      if (!numbers.length) return this.reply(username, `usage: @${this.username} ${keyword} <n> [n…]`);
      if (keyword === "release") {
        const r = this.handlers.release(numbers, username);
        return this.reply(username, `released ${r.done.join(", ") || "nothing"}${this.skippedNote(r)}`);
      }
      const r = this.handlers.claim(numbers, username);
      const verb = keyword === "reserve" ? "reserved" : "claimed";
      return this.reply(username, `${verb} ${r.done.join(", ") || "nothing"}${this.skippedNote(r)}`);
    }

    if (keyword === "give") {
      const m = args.match(/^(.+?)\s+(\S+)$/);
      if (!m) return this.reply(username, `usage: @${this.username} give <n> [n…] <player>`);
      const numbers = parseNumbers(m[1]);
      const target = m[2];
      if (!numbers.length) return this.reply(username, `usage: @${this.username} give <n> [n…] <player>`);
      const r = this.handlers.give(numbers, username, target);
      return this.reply(username, `gave ${r.done.join(", ") || "nothing"} to ${target}${this.skippedNote(r)}`);
    }

    // existing-agents task: "<n> [n…] <task>"
    const { numbers, rest: goal } = parseLeadingNumbers(rest);
    if (!numbers.length) return this.reply(username, `unknown command "${rest}"`);
    if (!goal) return this.reply(username, "add a goal after the agent numbers");
    const r = this.handlers.assignExisting(numbers, goal, username);
    return this.reply(username, `${r.done.join(", ") || "nothing"} on: ${goal}${this.skippedNote(r)}`);
  }

  stop(): void {
    this.stopped = true;
    this.reconnector.reset();
    if (this.recycleTimer) { clearInterval(this.recycleTimer); this.recycleTimer = null; }
    safeQuit(this.bot);
  }

  /** Reconnect to pick up a new host/login. start() tears down any existing connection. */
  reconnect(): void {
    this.reconnector.reset();
    this.start();
  }

  /** Periodically reconnect to reset the always-on bot's accumulated chunk memory (0 disables). */
  enableRecycle(intervalMs: number): void {
    if (intervalMs <= 0 || this.recycleTimer) return;
    this.recycleTimer = setInterval(() => {
      if (this.stopped || !this.bot?.entity) return;
      this.note("scheduled recycle (reset chunk memory)");
      this.reconnect();
    }, intervalMs);
  }

  status(): { username: string; online: boolean; netIn: number; netOut: number; log: string[] } {
    const s = socketBytes(this.bot);
    return {
      username: this.username,
      online: !!this.bot?.entity,
      netIn: this.mcInBase + s.in,
      netOut: this.mcOutBase + s.out,
      log: this.log.slice(-100),
    };
  }
}
