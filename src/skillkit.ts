import type Anthropic from "@anthropic-ai/sdk";
import type { CreateResult } from "./types.js";

export interface Pos {
  x: number;
  y: number;
  z: number;
}

/** Strict Anthropic tool input schema (all listed props required, no extras). */
export const obj = (properties: Record<string, unknown>, required: string[]): Anthropic.Tool["input_schema"] => ({
  type: "object",
  properties,
  required,
  additionalProperties: false,
});

/** Memory scope for a context: the owner if any, else the bot's own name. */
export const scopeOf = (ctx: SkillContext): string => ctx.self.owner ?? ctx.self.username;

/** Everything a skill/behavior needs. bot/mcData are mineflayer (typed as any). */
export interface SkillContext {
  bot: any;
  mcData: any;
  memory: MemoryStore;
  peers: PeerApi;
  self: { username: string; owner: string | null };
  behaviors: Set<string>;
}

/** A pluggable tool: an Anthropic tool definition plus its executor. */
export interface Skill {
  tool: Anthropic.Tool;
  run(ctx: SkillContext, input: any): Promise<string>;
}

/** A background auto-behavior, toggled by set_behavior and run on health/tick. */
export interface BehaviorHandler {
  name: string;
  onHealth?(ctx: SkillContext): void;
  onTick?(ctx: SkillContext): void;
}

/** Cross-agent access, backed by the manager. */
export interface PeerApi {
  position(name: string): Pos | null;
  online(name: string): boolean;
  send(to: string, from: string, message: string): boolean;
  /** Summon helper workers charged to `owner`'s per-user cap (null = uncapped). */
  summon(count: number, goal: string, owner: string | null): CreateResult;
}

export interface SkillEnv {
  memory: MemoryStore;
  peers: PeerApi;
}

export interface LedgerItem {
  text: string;
  status: "todo" | "doing" | "done";
}

/** Host-side durable memory, scoped per owner (survives a worker logging out). */
export class MemoryStore {
  private readonly waypoints = new Map<string, Map<string, Pos>>();
  private readonly notes = new Map<string, Map<string, string>>();
  private readonly ledgers = new Map<string, LedgerItem[]>();

  private ns<T>(m: Map<string, Map<string, T>>, scope: string): Map<string, T> {
    let inner = m.get(scope);
    if (!inner) m.set(scope, (inner = new Map()));
    return inner;
  }

  setWaypoint(scope: string, name: string, pos: Pos): void {
    this.ns(this.waypoints, scope).set(name, pos);
  }
  getWaypoint(scope: string, name: string): Pos | undefined {
    return this.ns(this.waypoints, scope).get(name);
  }
  listWaypoints(scope: string): [string, Pos][] {
    return [...this.ns(this.waypoints, scope).entries()];
  }

  setNote(scope: string, key: string, text: string): void {
    this.ns(this.notes, scope).set(key, text);
  }
  listNotes(scope: string, query?: string): [string, string][] {
    const all = [...this.ns(this.notes, scope).entries()];
    return query ? all.filter(([k, v]) => k.includes(query) || v.includes(query)) : all;
  }

  ledger(scope: string): LedgerItem[] {
    let l = this.ledgers.get(scope);
    if (!l) this.ledgers.set(scope, (l = []));
    return l;
  }

  /** Short text injected into perception each step so plans survive the step budget. */
  summary(scope: string): string {
    const l = this.ledger(scope).filter((i) => i.status !== "done");
    const wp = this.listWaypoints(scope);
    const parts: string[] = [];
    if (l.length) parts.push(`ledger: ${l.map((i) => `[${i.status}] ${i.text}`).join("; ")}`);
    if (wp.length) parts.push(`waypoints: ${wp.map(([n]) => n).join(", ")}`);
    return parts.join("\n");
  }
}

