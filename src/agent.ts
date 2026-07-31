import Anthropic from "@anthropic-ai/sdk";
import type { Bot } from "mineflayer";
import type { AgentState, AgentStatus, BotSpec, LlmConfig, McConfig } from "./types.js";
import type { Pos, SkillContext, SkillEnv } from "./skillkit.js";
import { type Planner, plannerFor } from "./llm.js";
import { TOOLS, execute, installAutoBehaviors, observe, summarizeResult } from "./skills.js";
import { Movements, Reconnector, collectBlock, installAuth, kickReason, logLine, logServerMessages, mcDataLoader, mineflayer, nearestHostile, pathfinder, pvp, safeQuit, socketBytes, startChunkPrune } from "./deps.js";

const SYSTEM = `You control a single Minecraft bot through a fixed set of skills (tools).
Pursue the assigned GOAL by calling one skill at a time and reading the result and the CURRENT STATE that follows each result.
Rules:
- Decompose the goal into short, concrete steps. Long-horizon plans fail; act, observe, adjust.
- Never invent coordinates — use find_blocks to locate things before moving or mining.
- If a skill returns an error, try a different concrete approach rather than repeating it.
- You can only talk to your owner and to fellow agents owned by them: use "message" to reach one, "message_team" to reach all your teammates. There is no public chat.
- Owner messages appear as OWNER:, teammate messages as AGENT <name>:, and damage as a "took N damage" note — respond to these.
- For repetitive work (gathering, crafting chains), save a routine once with save_routine, then run_routine to execute it without planning each step; check list_routines first to reuse one.
- To react automatically to conditions (low food/health, etc.), create a setting once with create_setting (e.g. food<14 -> collect and eat food); it runs on its own until you delete it.
- Call task_complete as soon as the goal is met or is clearly impossible.`;

// Cached prefix: tools render before system, so a breakpoint here caches tools + system.
const SYSTEM_BLOCKS: Anthropic.TextBlockParam[] = [
  { type: "text", text: SYSTEM, cache_control: { type: "ephemeral" } },
];

/** A worker bot: connects, pursues one goal, then logs out. Reusable via assign(). */
export class Agent {
  private bot: Bot | null = null;
  private mcData: any = null;
  private state: AgentState = "connecting";
  private goal: string | null;
  private step = 0;
  private convSteps = 0;
  private tokensIn = 0;
  private tokensOut = 0;
  private cacheRead = 0;
  private apiIn = 0;
  private apiOut = 0;
  private mcInBase = 0; // bytes from previous (closed) connections
  private mcOutBase = 0;
  private stopped = false;
  private looping = false;
  private effortOk = true;
  private thinkingOk = true;
  private readonly injected: string[] = [];
  // defend + auto_eat are on by default; the planner can toggle any behavior.
  private readonly behaviors = new Set<string>(["defend", "auto_eat"]);
  private lastHealth = 20;
  private readonly log: string[] = [];
  private readonly reconnector = new Reconnector(
    4,
    () => this.start(),
    (n) => {
      this.state = "error";
      this.note(`gave up reconnecting after ${n} attempts (check host/login)`);
    },
  );

  private planner: Planner;

  constructor(
    private readonly spec: BotSpec,
    private readonly mc: McConfig,
    private readonly llm: LlmConfig,
    public owner: string | null = null,
    private readonly env: SkillEnv,
    private readonly keys: { anthropic: string; openai: string }, // planner keys shared by the fleet
  ) {
    this.goal = spec.goal ?? null;
    this.planner = plannerFor(spec.model ?? llm.model, keys);
  }

  private makeCtx(): SkillContext {
    return {
      bot: this.bot,
      mcData: this.mcData,
      memory: this.env.memory,
      peers: this.env.peers,
      routines: this.env.routines,
      rules: this.env.rules,
      self: { username: this.spec.username, owner: this.owner },
      behaviors: this.behaviors,
    };
  }

  /** Position for the peer API. */
  position(): Pos | null {
    const p = this.bot?.entity?.position;
    return p ? { x: p.x, y: p.y, z: p.z } : null;
  }

  /** Deliver an external message (from another agent) into the planning loop. */
  inject(message: string): void {
    this.injected.push(message);
    this.note(`inbox: ${message}`);
  }

  /** Notify the planner when the bot loses health, naming a nearby hostile if any. */
  private onHealthChange(bot: Bot): void {
    const h = bot.health ?? this.lastHealth;
    if (h < this.lastHealth) {
      const dmg = Math.round(this.lastHealth - h);
      const threat = nearestHostile(bot, 8);
      const from = threat ? ` (hostile nearby: ${threat.name})` : "";
      this.inject(`took ${dmg} damage, health now ${Math.round(h)}/20${from}`);
    }
    this.lastHealth = h;
  }

  /** Perception + durable memory summary, fed to the planner each step. */
  private stateText(): string {
    const scope = this.owner ?? this.spec.username;
    const mem = this.env.memory.summary(scope);
    return mem ? `${observe(this.bot!)}\n${mem}` : observe(this.bot!);
  }

  private note(msg: string): void {
    logLine(this.log, msg);
  }

  start(): void {
    this.stopped = false;
    this.state = "connecting";
    this.reconnector.cancelPending();
    const old = this.bot;
    const prev = socketBytes(old); // fold the closing connection's bytes into the running total
    this.mcInBase += prev.in;
    this.mcOutBase += prev.out;
    const bot: Bot = mineflayer.createBot({
      host: this.mc.host,
      port: this.mc.port,
      username: this.spec.username,
      version: this.mc.version,
      auth: this.mc.auth,
      viewDistance: this.mc.viewDistance,
    });
    bot.loadPlugin(pathfinder);
    bot.loadPlugin(collectBlock);
    bot.loadPlugin(pvp);
    this.bot = bot;

    bot.once("spawn", () => {
      this.mcData = mcDataLoader(bot.version);
      bot.pathfinder.setMovements(new Movements(bot));
      // A* is synchronous on the shared event loop; keep each bot's slice small so
      // concurrent bots don't starve each other, and bound runaway searches.
      bot.pathfinder.tickTimeout = 10; // ms/tick spent pathfinding (default 40)
      bot.pathfinder.thinkTimeout = 3000; // give up a single search sooner (default 5000)
      (bot.pathfinder as any).searchRadius = 128; // don't A* the whole world for far/blocked goals
      this.behaviors.add("defend"); // eat-when-hungry + defend-when-attacked are always on by default
      this.behaviors.add("auto_eat");
      (bot as any)._behaviors = this.behaviors;
      installAutoBehaviors(bot, () => this.mcData, (name) => name === this.owner || /^agent_\d+$/i.test(name), () => this.makeCtx());
      startChunkPrune(bot, this.mc.chunkKeepRadius);
      this.state = "idle";
      this.reconnector.markConnected();
      this.note(`spawned as ${this.spec.username}`);
      // Authenticate first, then act — starting the loop before login lands gets the bot kicked.
      if (this.goal) setTimeout(() => void this.runLoop(), this.mc.loginMessage ? 3000 : 0);
    });
    logServerMessages(bot, (m) => this.note(m));
    installAuth(bot, () => this.mc.loginMessage, (m) => this.note(m));
    bot.on("chat", (username, message) => this.onOwnerChat(username, message));
    bot.on("whisper", (username, message) => this.onWhisper(username, message));
    this.lastHealth = bot.health ?? 20;
    bot.on("health", () => this.onHealthChange(bot));
    bot.on("kicked", (reason) => this.note(`kicked: ${kickReason(reason)}`));
    bot.on("error", (err) => this.note(`error: ${err.message}`));
    bot.on("end", () => {
      if (this.bot !== bot) return; // superseded by a newer connection; ignore
      if (this.stopped) return;
      this.state = "connecting";
      const delay = this.reconnector.scheduleReconnect(() => !this.stopped && this.bot === bot);
      if (delay) this.note(`disconnected; reconnecting in ${delay / 1000}s`);
    });
    if (old) safeQuit(old); // its "end" is ignored (this.bot now points to the new one)
  }

  /** Owner-only in-game prompt: `@agent_N <msg>` while it's online. */
  private onOwnerChat(username: string, message: string): void {
    if (this.owner === null || username !== this.owner) return;
    const escaped = this.spec.username.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const match = message.trim().match(new RegExp(`^@${escaped}\\b[\\s:,-]*(.*)$`, "i"));
    if (!match) return;
    const msg = match[1].trim();
    if (msg) this.steer(msg);
  }

  /** Owner-only whisper (`/msg agent_N <msg>`). */
  private onWhisper(username: string, message: string): void {
    if (this.owner === null || username !== this.owner) return;
    const msg = message.trim();
    if (msg) this.steer(msg);
  }

  /** Steer a running task (inject a prompt) or, if idle, start it as a new goal. */
  private steer(message: string): void {
    if (this.looping) {
      this.injected.push(`OWNER: ${message}`);
      this.note(`owner prompt queued: ${message}`);
    } else {
      this.assign(message);
    }
  }

  /** Reserve this identity without connecting (used by claim of an unspawned number). */
  markOffline(): void {
    this.stopped = true;
    this.state = "stopped";
    this.reconnector.reset();
  }

  /** (Re)assign a goal. Reconnects if logged out. Rejected (false) while busy. */
  assign(goal: string): boolean {
    if (this.looping) {
      this.note(`assign rejected (busy): ${goal}`);
      return false;
    }
    this.goal = goal;
    this.note(`goal: ${goal}`);
    if (this.state === "idle") void this.runLoop();
    else if (this.state === "stopped" || this.state === "error") {
      this.stopped = false;
      this.reconnector.reset();
      this.start();
    }
    return true;
  }

  /** Thinking is always off. Self-heals if a model rejects the thinking/effort params (e.g. Haiku effort). */
  private planParams(model: string, messages: Anthropic.MessageParam[]): Anthropic.MessageCreateParamsNonStreaming {
    return {
      model,
      max_tokens: 1024,
      system: SYSTEM_BLOCKS,
      tools: TOOLS,
      messages,
      ...(this.thinkingOk ? { thinking: { type: "disabled" as const } } : {}),
      ...(this.effortOk ? { output_config: { effort: this.llm.effort } } : {}),
    };
  }

  /** Send one request, accumulating approximate on-wire API bytes (uncompressed JSON). */
  private async send(params: Anthropic.MessageCreateParamsNonStreaming): Promise<Anthropic.Message> {
    this.apiOut += JSON.stringify(params).length;
    const res = await this.planner.create(params);
    this.apiIn += JSON.stringify(res).length;
    return res;
  }

  private async plan(model: string, messages: Anthropic.MessageParam[]): Promise<Anthropic.Message> {
    try {
      return await this.send(this.planParams(model, messages));
    } catch (err) {
      if (err instanceof Anthropic.BadRequestError) {
        let changed = false;
        if (this.effortOk && /effort|output_config/i.test(err.message)) { this.effortOk = false; changed = true; }
        if (this.thinkingOk && /thinking/i.test(err.message)) { this.thinkingOk = false; changed = true; }
        if (changed) {
          this.note("dropping rejected params; retrying");
          return await this.send(this.planParams(model, messages));
        }
      }
      throw err;
    }
  }

  private async runLoop(): Promise<void> {
    if (this.looping || !this.bot || !this.goal) return;
    this.looping = true;
    this.state = "working";
    this.step = 0;
    this.convSteps = 0;
    const model = this.spec.model ?? this.llm.model;
    this.planner = plannerFor(model, this.keys); // re-pick so a live model change applies next task
    const goal = this.goal;
    // `content` holds the full result while recent, then is replaced in place by its summary
    // once the step leaves the KEEP_FULL window — the full string is dropped, not retained.
    type Step = { assistant: Anthropic.ContentBlock[]; results: { id: string; name: string; content: string }[]; collapsed: boolean };
    const history: Step[] = [];
    const KEEP_FULL = 4; // recent steps keep full results; older collapse to summaries

    // Rebuild the message thread each step: stable goal, compacted old results, fresh perception last.
    const build = (): Anthropic.MessageParam[] => {
      const msgs: Anthropic.MessageParam[] = [];
      if (history.length === 0) {
        msgs.push({ role: "user", content: `GOAL: ${goal}\n\nCURRENT STATE:\n${this.stateText()}` });
      } else {
        msgs.push({ role: "user", content: `GOAL: ${goal}` });
        history.forEach((rec, i) => {
          msgs.push({ role: "assistant", content: rec.assistant });
          const isLast = i === history.length - 1;
          const results: Anthropic.ToolResultBlockParam[] = rec.results.map((r) => ({
            type: "tool_result",
            tool_use_id: r.id,
            content: r.content,
          }));
          msgs.push({ role: "user", content: isLast ? [...results, { type: "text", text: `CURRENT STATE:\n${this.stateText()}` }] : results });
        });
      }
      if (this.injected.length) msgs.push({ role: "user", content: this.injected.splice(0).join("\n") });
      return msgs;
    };

    try {
      while (!this.stopped && this.step < this.llm.maxSteps) {
        this.step++;
        const res = await this.plan(model, build());
        this.tokensIn += res.usage.input_tokens ?? 0;
        this.tokensOut += res.usage.output_tokens ?? 0;
        this.cacheRead += res.usage.cache_read_input_tokens ?? 0;

        for (const b of res.content) if (b.type === "text" && b.text.trim()) this.note(`thinks: ${b.text.trim()}`);
        const calls = res.content.filter((b): b is Anthropic.ToolUseBlock => b.type === "tool_use");
        if (calls.length === 0) break;

        const done = calls.find((c) => c.name === "task_complete");
        if (done) {
          this.note(`done: ${(done.input as any).summary ?? ""}`);
          break;
        }

        const results: Step["results"] = [];
        for (const call of calls) {
          const out = await execute(this.bot, this.mcData, call.name, call.input as Record<string, any>, this.makeCtx());
          this.note(`${call.name} -> ${out}`);
          results.push({ id: call.id, name: call.name, content: out });
        }
        history.push({ assistant: res.content, results, collapsed: false });
        // Collapse steps that fell out of the full-keep window: replace full output with its summary, drop the rest.
        for (let i = 0; i < history.length - KEEP_FULL; i++) {
          const rec = history[i];
          if (rec.collapsed) continue;
          for (const r of rec.results) r.content = summarizeResult(r.name, r.content);
          rec.collapsed = true;
        }
        this.convSteps = history.length;
      }
      if (this.step >= this.llm.maxSteps) this.note("stopped: step budget exhausted");
    } catch (err) {
      this.note(`loop error: ${(err as Error).message}`);
    } finally {
      this.looping = false;
      this.logout();
    }
  }

  /** Disconnect on task completion/failure; the identity stays for owner reuse. */
  private logout(): void {
    this.stopped = true;
    this.state = "stopped";
    this.reconnector.reset();
    safeQuit(this.bot);
    this.note("task finished; logged out");
  }

  chat(message: string): void {
    this.bot?.chat(message);
  }

  stop(): void {
    this.stopped = true;
    this.state = "stopped";
    this.reconnector.reset();
    safeQuit(this.bot);
    this.note("stopped");
  }

  isOnline(): boolean {
    return this.state !== "stopped";
  }

  /** Reconnect an idle bot (to pick up a new host/login). start() tears down any existing connection. Skips busy/stopped ones. */
  reconnect(): void {
    if (this.state === "stopped" || this.looping) return;
    this.reconnector.reset();
    this.start();
  }

  status(): AgentStatus {
    const p = this.bot?.entity?.position ?? null;
    return {
      username: this.spec.username,
      owner: this.owner,
      state: this.state,
      goal: this.goal,
      step: this.step,
      convSteps: this.convSteps,
      tokensIn: this.tokensIn,
      tokensOut: this.tokensOut,
      cacheReadTokens: this.cacheRead,
      netIn: this.mcInBase + socketBytes(this.bot).in + this.apiIn,
      netOut: this.mcOutBase + socketBytes(this.bot).out + this.apiOut,
      health: this.bot?.health ?? null,
      food: this.bot?.food ?? null,
      position: p ? { x: Math.round(p.x), y: Math.round(p.y), z: Math.round(p.z) } : null,
      log: this.log.slice(-100),
    };
  }
}
