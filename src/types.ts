export type AuthMode = "offline" | "microsoft";

export type AgentState =
  | "connecting"
  | "idle"
  | "working"
  | "error"
  | "stopped";

export interface McConfig {
  host: string;
  port: number;
  version?: string;
  auth: AuthMode;
  /** Sent in chat on spawn if non-empty (e.g. "/login <pw>" for an offline server). Live-editable. */
  loginMessage: string;
}

export interface LlmConfig {
  apiKey: string;
  model: string;
  effort: "low" | "medium" | "high" | "xhigh" | "max";
  maxSteps: number;
}

/** One bot. `goal` and `model` override the shared defaults when present. */
export interface BotSpec {
  username: string;
  goal?: string;
  model?: string;
}

export interface AppConfig {
  port: number;
  /** SQLite file for settings, ownership, and memory (OpenHost app-data dir). */
  dbPath: string;
  mc: McConfig;
  llm: LlmConfig;
  bots: BotSpec[];
  /** Username of the always-on dispatcher players tag to summon workers. */
  dispatcherName: string;
  /** Usernames allowed to issue @dispatcher commands; empty = anyone. */
  commandAllowlist: string[];
  /** Hard cap on concurrent *online* worker bots. */
  maxBots: number;
  /** Cap on online workers a single owner may hold (0 = unlimited). Live-editable. */
  maxPerUser: number;
}

export interface SpawnResult {
  ok: boolean;
  username?: string;
  reason?: "at_capacity" | "user_limit";
}

/** Result of `new [n]` — created worker names plus any over a cap. */
export interface CreateResult {
  created: string[];
  rejected: number;
  reason?: "at_capacity" | "user_limit";
}

/** Result of a per-agent batch op (task/release/claim/give). */
export interface BatchResult {
  done: string[];
  skipped: { name: string; reason: string }[];
}

export interface AgentStatus {
  username: string;
  owner: string | null;
  state: AgentState;
  goal: string | null;
  step: number;
  convSteps: number;
  tokensIn: number;
  tokensOut: number;
  cacheReadTokens: number;
  netIn: number;
  netOut: number;
  health: number | null;
  food: number | null;
  position: { x: number; y: number; z: number } | null;
  log: string[];
}
