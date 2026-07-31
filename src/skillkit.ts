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
  memory: Memory;
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
  memory: Memory;
  peers: PeerApi;
}

export interface LedgerItem {
  text: string;
  status: "todo" | "doing" | "done";
}

/** Host-side durable memory, scoped per owner (survives a worker logging out). Backed by SQLite. */
export interface Memory {
  setWaypoint(scope: string, name: string, pos: Pos): void;
  getWaypoint(scope: string, name: string): Pos | undefined;
  listWaypoints(scope: string): [string, Pos][];
  setNote(scope: string, key: string, text: string): void;
  listNotes(scope: string, query?: string): [string, string][];
  ledger(scope: string): LedgerItem[];
  /** Upsert a ledger item's status, returning the scope's full ledger. */
  setLedgerItem(scope: string, text: string, status: LedgerItem["status"]): LedgerItem[];
  summary(scope: string): string;
}

