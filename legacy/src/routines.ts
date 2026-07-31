// Deterministic interpreter for saved routines: composes existing skills with
// repeat/until/when control flow. No eval — steps are plain data, tools are whitelisted.

export class RoutineError extends Error {}

const CMP: Record<string, (a: number, b: number) => boolean> = {
  ">=": (a, b) => a >= b,
  "<=": (a, b) => a <= b,
  "==": (a, b) => a === b,
  "!=": (a, b) => a !== b,
  ">": (a, b) => a > b,
  "<": (a, b) => a < b,
};

function invCount(bot: any, item: string): number {
  return bot.inventory.items().filter((i: any) => i.name === item).reduce((s: number, i: any) => s + i.count, 0);
}

function nearbyCount(bot: any, mcData: any, block: string): number {
  const b = mcData?.blocksByName?.[block];
  return b ? bot.findBlocks({ matching: b.id, maxDistance: 32, count: 64 }).length : 0;
}

/** Evaluate a resolved condition: "have:cobblestone>=64", "find:iron_ore==0", "health<8", "food<10". */
export function evalCondition(bot: any, mcData: any, cond: string): boolean {
  const m = String(cond).replace(/\s+/g, "").match(/^(.+?)(>=|<=|==|!=|>|<)(-?\d+)$/);
  if (!m) return false;
  const [, left, op, num] = m;
  let lhs = 0;
  if (left.startsWith("have:")) lhs = invCount(bot, left.slice(5));
  else if (left.startsWith("find:")) lhs = nearbyCount(bot, mcData, left.slice(5));
  else if (left === "health") lhs = bot.health ?? 0;
  else if (left === "food") lhs = bot.food ?? 0;
  else return false;
  return CMP[op](lhs, Number(num));
}

/** Substitute {param} placeholders from args, recursively, in strings/arrays/objects. */
function resolve(value: any, args: Record<string, any>): any {
  if (typeof value === "string") return value.replace(/\{(\w+)\}/g, (_, k) => (k in args ? String(args[k]) : `{${k}}`));
  if (Array.isArray(value)) return value.map((v) => resolve(v, args));
  if (value && typeof value === "object") {
    const o: Record<string, any> = {};
    for (const [k, v] of Object.entries(value)) o[k] = resolve(v, args);
    return o;
  }
  return value;
}

/** Tool names referenced anywhere in a step tree — used to validate a routine before saving. */
export function referencedTools(steps: any[], out = new Set<string>()): Set<string> {
  for (const s of steps ?? []) {
    if (s && typeof s === "object") {
      if (typeof s.tool === "string") out.add(s.tool);
      if (Array.isArray(s.do)) referencedTools(s.do, out);
      if (Array.isArray(s.else)) referencedTools(s.else, out);
    }
  }
  return out;
}

export interface RunCtx {
  exec: (tool: string, args: any) => Promise<string>;
  bot: any;
  mcData: any;
  budget: { steps: number; max: number };
  deadline: number;
  log: string[];
  /** Live progress sink (agent activity log); receives each control-flow entry and tool step. */
  note?: (msg: string) => void;
}

/** Record a line to the summary log and stream it live. */
function emit(o: RunCtx, msg: string): void {
  o.log.push(msg);
  o.note?.(msg);
}

async function runStep(step: any, args: Record<string, any>, o: RunCtx): Promise<void> {
  if (Date.now() > o.deadline) throw new RoutineError("time budget exhausted");
  if (o.budget.steps >= o.budget.max) throw new RoutineError(`step budget (${o.budget.max}) exhausted`);
  if (!step || typeof step !== "object") return;

  if (Array.isArray(step.do)) {
    if (typeof step.until === "string") {
      const cond = resolve(step.until, args);
      const max = Math.min(Number(step.max) || 64, 256);
      emit(o, `until ${cond} (max ${max})`);
      for (let i = 0; i < max && !evalCondition(o.bot, o.mcData, cond); i++) await runSteps(step.do, args, o);
      return;
    }
    if (typeof step.repeat === "number") {
      const n = Math.min(step.repeat, 256);
      emit(o, `repeat ${n}x`);
      for (let i = 0; i < n; i++) await runSteps(step.do, args, o);
      return;
    }
    if (typeof step.when === "string") {
      const cond = resolve(step.when, args);
      const ok = evalCondition(o.bot, o.mcData, cond);
      emit(o, `when ${cond} → ${ok ? "do" : "else"}`);
      await runSteps(ok ? step.do : Array.isArray(step.else) ? step.else : [], args, o);
      return;
    }
    await runSteps(step.do, args, o); // bare group
    return;
  }

  if (typeof step.tool === "string") {
    o.budget.steps++;
    const result = await o.exec(step.tool, resolve(step.args ?? {}, args));
    emit(o, `${step.tool} -> ${result}`);
    if (step.stop_on_error && /^(error|unknown|cannot|no such|not carrying)/i.test(result)) {
      throw new RoutineError(`step ${step.tool} failed: ${result}`);
    }
  }
}

export async function runSteps(steps: any[], args: Record<string, any>, o: RunCtx): Promise<void> {
  for (const s of steps ?? []) await runStep(s, args, o);
}
