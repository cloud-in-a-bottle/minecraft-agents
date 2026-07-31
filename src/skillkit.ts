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

/** Scope for the routine + settings library: one shared collection for every agent. */
export const SHARED_SCOPE = "shared";

/** Everything a skill/behavior needs. bot/mcData are mineflayer (typed as any). */
export interface SkillContext {
  bot: any;
  mcData: any;
  memory: Memory;
  peers: PeerApi;
  routines: RoutineStore;
  rules: RuleStore;
  self: { username: string; owner: string | null };
  behaviors: Set<string>;
  /** Live activity-log sink (the agent's log); used to stream routine/rule progress. */
  note?: (msg: string) => void;
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
  /** Owner of a managed agent; null if unowned, undefined if not a managed agent. */
  ownerOf(name: string): string | null | undefined;
  /** Usernames of online agents sharing `owner` (null owner has no teammates). */
  teammates(owner: string | null): string[];
  /** Summon helper workers charged to `owner`'s per-user cap (null = uncapped). */
  summon(count: number, goal: string, owner: string | null): CreateResult;
}

export interface SkillEnv {
  memory: Memory;
  peers: PeerApi;
  routines: RoutineStore;
  rules: RuleStore;
}

/** A saved, replayable procedure the agent composes from existing skills. `steps` is interpreted, not code. */
export interface Routine {
  name: string;
  description: string;
  steps: any[];
}

/** Durable routine library, scoped per owner (shared across that owner's agents). */
export interface RoutineStore {
  saveRoutine(scope: string, routine: Routine): void;
  getRoutine(scope: string, name: string): Routine | undefined;
  listRoutines(scope: string): { name: string; description: string }[];
}

/** A bot-authored reactive setting: when `condition` holds, run `steps` (a routine body). */
export interface Rule {
  name: string;
  condition: string;
  steps: any[];
  enabled: boolean;
}

/** Durable rule library, scoped per owner. File-backed (a subdirectory, not the DB). */
export interface RuleStore {
  saveRule(scope: string, rule: Rule): void;
  listRules(scope: string): Rule[];
  deleteRule(scope: string, name: string): boolean;
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

