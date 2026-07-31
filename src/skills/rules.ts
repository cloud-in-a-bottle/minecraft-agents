import { obj, SHARED_SCOPE, type Rule, type Skill, type SkillContext } from "../skillkit.js";
import { referencedTools } from "../routines.js";

const FORBIDDEN = ["save_routine", "task_complete", "create_setting", "delete_setting", "list_settings"];

/** Valid if it matches <lhs><op><int> and lhs is have:/find:/health/food. */
const validCondition = (cond: string): boolean => {
  const m = cond.match(/^(.+?)(>=|<=|==|!=|>|<)(-?\d+)$/);
  if (!m) return false;
  const left = m[1];
  return left.startsWith("have:") || left.startsWith("find:") || left === "health" || left === "food";
};

export const skills: Skill[] = [
  {
    tool: {
      name: "create_setting",
      description:
        "Create a reactive rule: whenever condition holds, steps run automatically. " +
        "Condition: have:<item><op>N, find:<block><op>N, health<op>N, food<op>N (op is >=,<=,>,<,==,!=). " +
        "Steps use the SAME grammar as save_routine. Example: condition food<14 -> steps that collect and eat food.",
      input_schema: {
        type: "object",
        properties: {
          name: { type: "string" },
          condition: { type: "string" },
          steps: { type: "array", items: { type: "object" } },
        },
        required: ["name", "condition", "steps"],
        additionalProperties: false,
      },
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      const steps = Array.isArray(input.steps) ? input.steps : [];
      if (!input.name || steps.length === 0) return "need a name and non-empty steps";
      const condition = String(input.condition ?? "").replace(/\s+/g, "");
      if (!validCondition(condition)) return "condition must look like food<14, health<=6, have:cooked_beef>=1, or find:oak_log>0";
      const refs = referencedTools(steps);
      const bad = [...refs].filter((t) => FORBIDDEN.includes(t));
      if (bad.length) return `settings can't use: ${bad.join(", ")}`;
      const rule: Rule = { name: String(input.name), condition, steps, enabled: true };
      ctx.rules.saveRule(SHARED_SCOPE, rule);
      return `saved setting "${rule.name}": when ${condition}, run ${refs.size} skill(s)`;
    },
  },
  {
    tool: {
      name: "list_settings",
      description: "List reactive settings (name, on/off, condition).",
      input_schema: obj({}, []),
    },
    run: async (ctx: SkillContext, _input: any): Promise<string> => {
      const rules = ctx.rules.listRules(SHARED_SCOPE);
      if (!rules.length) return "no settings yet";
      return rules.map((r) => `${r.name} [${r.enabled ? "on" : "off"}]: when ${r.condition}`).join("\n");
    },
  },
  {
    tool: {
      name: "toggle_setting",
      description: "Enable or disable a reactive setting by name.",
      input_schema: obj({ name: { type: "string" }, enabled: { type: "boolean" } }, ["name", "enabled"]),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      const scope = SHARED_SCOPE;
      const name = String(input.name);
      const rule = ctx.rules.listRules(scope).find((r) => r.name === name);
      if (!rule) return `no setting "${name}"`;
      const enabled = Boolean(input.enabled);
      ctx.rules.saveRule(scope, { ...rule, enabled });
      return `setting "${name}" ${enabled ? "on" : "off"}`;
    },
  },
  {
    tool: {
      name: "delete_setting",
      description: "Delete a reactive setting by name.",
      input_schema: obj({ name: { type: "string" } }, ["name"]),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      const name = String(input.name);
      const ok = ctx.rules.deleteRule(SHARED_SCOPE, name);
      return ok ? `deleted setting "${name}"` : `no setting "${name}"`;
    },
  },
];
