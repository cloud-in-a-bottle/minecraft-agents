import { evalCondition, runSteps, type RunCtx } from "./routines.js";
import { scopeOf, type SkillContext } from "./skillkit.js";

/** Drives bot-authored reactive rules: on each tick, fire any rule whose condition holds. */
export class RuleEngine {
  private readonly running = new Set<string>();
  private readonly cooldownUntil = new Map<string, number>();

  tick(ctx: SkillContext, exec: (tool: string, args: any) => Promise<string>): void {
    const scope = scopeOf(ctx);
    for (const rule of ctx.rules.listRules(scope)) {
      try {
        if (!rule.enabled || this.running.has(rule.name)) continue;
        if (Date.now() < (this.cooldownUntil.get(rule.name) ?? 0)) continue;
        if (!evalCondition(ctx.bot, ctx.mcData, rule.condition)) continue;

        this.running.add(rule.name);
        const rc: RunCtx = {
          exec,
          bot: ctx.bot,
          mcData: ctx.mcData,
          budget: { steps: 0, max: 100 },
          deadline: Date.now() + 60_000,
          log: [],
        };
        void runSteps(rule.steps, {}, rc)
          .catch(() => {}) // swallow (RoutineError or otherwise): a failing rule must not crash the tick
          .finally(() => {
            this.running.delete(rule.name);
            this.cooldownUntil.set(rule.name, Date.now() + 10_000);
          });
      } catch {
        // one bad rule can't stop the others
      }
    }
  }
}
