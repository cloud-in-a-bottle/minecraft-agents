import Anthropic from "@anthropic-ai/sdk";
import { Agent } from "./agent.js";
import { Dispatcher } from "./dispatcher.js";
import { type PeerApi, type SkillEnv } from "./skillkit.js";
import type { Store } from "./store.js";
import type { AppConfig, AgentStatus, BatchResult, BotSpec, CreateResult, SpawnResult } from "./types.js";

/** Owns the dispatcher and every worker; handles creation, ownership, and reuse. */
export class BotManager {
  private readonly agents = new Map<string, Agent>();
  private readonly dispatcher: Dispatcher;
  private readonly env: SkillEnv;
  private readonly client: Anthropic; // one client shared by the whole fleet
  private nextNumber: number;
  private maxPerUser: number;

  constructor(private readonly config: AppConfig, private readonly store: Store) {
    this.loadSettings(); // durable settings override the env-var seed
    this.maxPerUser = config.maxPerUser;
    this.client = new Anthropic({ apiKey: config.llm.apiKey });
    const peers: PeerApi = {
      position: (name) => this.agents.get(name)?.position() ?? null,
      online: (name) => !!this.agents.get(name)?.isOnline(),
      send: (to, from, message) => {
        const a = this.agents.get(to);
        if (!a || !a.isOnline()) return false;
        a.inject(`AGENT ${from}: ${message}`);
        return true;
      },
      summon: (count, goal, owner) => this.createNew(count, goal, owner),
    };
    this.env = { memory: store, peers, routines: store };
    this.dispatcher = new Dispatcher(config.dispatcherName, config.mc, config.commandAllowlist, {
      createNew: (count, goal, owner) => this.createNew(count, goal, owner),
      assignExisting: (nums, goal, owner) => this.assignExisting(nums, goal, owner),
      release: (nums, owner) => this.release(nums, owner),
      claim: (nums, owner) => this.claim(nums, owner),
      give: (nums, owner, target) => this.give(nums, owner, target),
    });
    for (const spec of config.bots) this.create(spec, null);
    this.nextNumber = config.bots.length + 1;
    this.restoreOwnership();
  }

  private loadSettings(): void {
    const { config, store } = this;
    const host = store.getSetting("mcHost");
    const port = store.getSetting("mcPort");
    const login = store.getSetting("loginMessage");
    const cap = store.getSetting("maxPerUser");
    if (host) config.mc.host = host;
    if (port) config.mc.port = Number(port);
    if (login != null) config.mc.loginMessage = login;
    if (cap != null) config.maxPerUser = Number(cap);
  }

  /** Recreate persisted owned numbers as offline placeholders so ownership survives restarts. */
  private restoreOwnership(): void {
    for (const { username, owner } of this.store.allAgents()) {
      const existing = this.agents.get(username);
      if (existing) { existing.owner = owner; continue; }
      this.create({ username }, owner).markOffline();
      const n = Number(username.replace(/^agent_/, ""));
      if (Number.isFinite(n)) this.nextNumber = Math.max(this.nextNumber, n + 1);
    }
  }

  private create(spec: BotSpec, owner: string | null): Agent {
    const agent = new Agent(spec, this.config.mc, this.config.llm, owner, this.env, this.client);
    this.agents.set(spec.username, agent);
    return agent;
  }

  /** Set an agent's owner and persist it (ownership is written on any change). */
  private setOwner(agent: Agent, name: string, owner: string | null): void {
    agent.owner = owner;
    this.store.setOwner(name, owner);
  }

  startAll(): void {
    this.dispatcher.start();
    this.dispatcher.enableRecycle(this.config.dispatcherRecycleMs);
    for (const agent of this.agents.values()) agent.start();
  }

  private onlineCount(): number {
    return [...this.agents.values()].filter((a) => a.isOnline()).length;
  }

  private ownedOnlineCount(owner: string): number {
    return [...this.agents.values()].filter((a) => a.owner === owner && a.isOnline()).length;
  }

  /** Per-user cap applies to real players, not the admin HTTP channel. */
  private overUserCap(owner: string | null): boolean {
    return owner !== null && owner !== "api" && this.maxPerUser > 0 && this.ownedOnlineCount(owner) >= this.maxPerUser;
  }

  /** Live settings shown/edited in the dashboard. */
  getSettings(): { maxBots: number; maxPerUser: number; mcHost: string; mcPort: number; loginMessage: string } {
    return {
      maxBots: this.config.maxBots,
      maxPerUser: this.maxPerUser,
      mcHost: this.config.mc.host,
      mcPort: this.config.mc.port,
      loginMessage: this.config.mc.loginMessage,
    };
  }

  /** Apply a live settings patch, persisting each change. Host/port/login changes reconnect the fleet. */
  updateSettings(patch: { maxPerUser?: number; mcHost?: string; mcPort?: number; loginMessage?: string }): void {
    if (patch.maxPerUser != null) {
      this.maxPerUser = Math.max(0, Math.floor(patch.maxPerUser));
      this.store.setSetting("maxPerUser", String(this.maxPerUser));
    }
    let reconnect = false;
    if (patch.mcHost != null && patch.mcHost !== this.config.mc.host) { this.config.mc.host = patch.mcHost; this.store.setSetting("mcHost", patch.mcHost); reconnect = true; }
    if (patch.mcPort != null && patch.mcPort !== this.config.mc.port) { this.config.mc.port = patch.mcPort; this.store.setSetting("mcPort", String(patch.mcPort)); reconnect = true; }
    if (patch.loginMessage != null && patch.loginMessage !== this.config.mc.loginMessage) { this.config.mc.loginMessage = patch.loginMessage; this.store.setSetting("loginMessage", patch.loginMessage); reconnect = true; }
    if (reconnect) {
      this.dispatcher.reconnect();
      for (const a of this.agents.values()) a.reconnect();
    }
  }

  private spawn(goal: string | undefined, owner: string | null): SpawnResult {
    if (this.overUserCap(owner)) return { ok: false, reason: "user_limit" };
    if (this.onlineCount() >= this.config.maxBots) return { ok: false, reason: "at_capacity" };
    let username = `agent_${this.nextNumber}`;
    while (this.agents.has(username)) username = `agent_${++this.nextNumber}`;
    this.create({ username, goal }, owner).start();
    this.store.setOwner(username, owner);
    this.nextNumber++;
    return { ok: true, username };
  }

  /** `new [n] <task>` — n fresh workers on one goal, owned by the caller. */
  createNew(count: number, goal: string, owner: string | null): CreateResult {
    const created: string[] = [];
    let rejected = 0;
    let reason: CreateResult["reason"];
    for (let i = 0; i < count; i++) {
      const r = this.spawn(goal, owner);
      if (r.ok && r.username) created.push(r.username);
      else {
        rejected++;
        reason = r.reason;
      }
    }
    return { created, rejected, reason };
  }

  /** `x[, y] <task>` — retask existing workers the caller owns. */
  assignExisting(numbers: number[], goal: string, owner: string): BatchResult {
    const done: string[] = [];
    const skipped: BatchResult["skipped"] = [];
    for (const name of numbers.map((n) => `agent_${n}`)) {
      const a = this.agents.get(name);
      if (!a) skipped.push({ name, reason: "unknown" });
      else if (a.owner !== owner) skipped.push({ name, reason: "not_owner" });
      else if (!a.isOnline() && this.overUserCap(owner)) skipped.push({ name, reason: "user_limit" });
      else if (!a.isOnline() && this.onlineCount() >= this.config.maxBots) skipped.push({ name, reason: "at_capacity" });
      else if (!a.assign(goal)) skipped.push({ name, reason: "busy" });
      else done.push(name);
    }
    return { done, skipped };
  }

  /** `release x[, y]` — the owner relinquishes ownership (becomes claimable). */
  release(numbers: number[], owner: string): BatchResult {
    const done: string[] = [];
    const skipped: BatchResult["skipped"] = [];
    for (const name of numbers.map((n) => `agent_${n}`)) {
      const a = this.agents.get(name);
      if (!a) skipped.push({ name, reason: "unknown" });
      else if (a.owner !== owner) skipped.push({ name, reason: "not_owner" });
      else {
        this.setOwner(a, name, null);
        done.push(name);
      }
    }
    return { done, skipped };
  }

  /** `claim x[, y]` — take an unowned number (creating it offline if new). */
  claim(numbers: number[], owner: string): BatchResult {
    const done: string[] = [];
    const skipped: BatchResult["skipped"] = [];
    for (const n of numbers) {
      const name = `agent_${n}`;
      const a = this.agents.get(name);
      if (!a) {
        this.create({ username: name }, owner).markOffline();
        this.store.setOwner(name, owner);
        this.nextNumber = Math.max(this.nextNumber, n + 1);
        done.push(name);
      } else if (a.owner === null || a.owner === owner) {
        this.setOwner(a, name, owner);
        done.push(name);
      } else skipped.push({ name, reason: "owned_by_other" });
    }
    return { done, skipped };
  }

  /** `give x[, y] <player>` — transfer ownership of your workers to another player. */
  give(numbers: number[], owner: string, target: string): BatchResult {
    const done: string[] = [];
    const skipped: BatchResult["skipped"] = [];
    for (const name of numbers.map((n) => `agent_${n}`)) {
      const a = this.agents.get(name);
      if (!a) skipped.push({ name, reason: "unknown" });
      else if (a.owner !== owner) skipped.push({ name, reason: "not_owner" });
      else {
        this.setOwner(a, name, target);
        done.push(name);
      }
    }
    return { done, skipped };
  }

  get(username: string): Agent | undefined {
    return this.agents.get(username);
  }

  list(): AgentStatus[] {
    return [...this.agents.values()].map((a) => a.status());
  }

  dispatcherStatus() {
    return this.dispatcher.status();
  }
}
