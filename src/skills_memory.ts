import { obj, scopeOf, type Skill, type SkillContext } from "./skillkit.js";
import { Vec3, goals } from "./deps.js";

const pos = (ctx: SkillContext) => {
  const p = (ctx.bot as any).entity.position;
  return { x: Math.round(p.x), y: Math.round(p.y), z: Math.round(p.z) };
};

const dist = (ctx: SkillContext, x: number, y: number, z: number) =>
  Math.round((ctx.bot as any).entity.position.distanceTo(new Vec3(x, y, z)));

export const skills: Skill[] = [
  {
    tool: {
      name: "save_waypoint",
      description: "Record the bot's current position under a name for later recall (use \"base\" for a home base).",
      strict: true,
      input_schema: obj({ name: { type: "string" } }, ["name"]),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      const p = pos(ctx);
      ctx.memory.setWaypoint(scopeOf(ctx), String(input.name), p);
      return `saved waypoint "${input.name}" at (${p.x}, ${p.y}, ${p.z})`;
    },
  },
  {
    tool: {
      name: "goto_waypoint",
      description: "Pathfind to a previously saved waypoint by name.",
      strict: true,
      input_schema: obj({ name: { type: "string" } }, ["name"]),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      const scope = scopeOf(ctx);
      const wp = ctx.memory.getWaypoint(scope, String(input.name));
      if (!wp) {
        const names = ctx.memory.listWaypoints(scope).map(([n]) => n).join(", ") || "none";
        return `no waypoint "${input.name}" (known: ${names})`;
      }
      try {
        await (ctx.bot as any).pathfinder.goto(new goals.GoalNear(wp.x, wp.y, wp.z, 1));
        return `arrived at "${input.name}" (${wp.x}, ${wp.y}, ${wp.z})`;
      } catch (err) {
        return `error going to "${input.name}": ${(err as Error).message}`;
      }
    },
  },
  {
    tool: {
      name: "list_waypoints",
      description: "List saved waypoints with coordinates and distance from the bot.",
      strict: true,
      input_schema: obj({}, []),
    },
    run: async (ctx: SkillContext, _input: any): Promise<string> => {
      const wps = ctx.memory.listWaypoints(scopeOf(ctx));
      if (!wps.length) return "none saved";
      return wps
        .map(([n, p]) => `${n}: (${p.x}, ${p.y}, ${p.z}) ${dist(ctx, p.x, p.y, p.z)}m`)
        .join("; ");
    },
  },
  {
    tool: {
      name: "remember_note",
      description: "Store a freeform note or learning under a key for later recall.",
      strict: true,
      input_schema: obj({ key: { type: "string" }, text: { type: "string" } }, ["key", "text"]),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      ctx.memory.setNote(scopeOf(ctx), String(input.key), String(input.text));
      return `noted "${input.key}"`;
    },
  },
  {
    tool: {
      name: "recall_notes",
      description: "Recall stored notes, optionally filtered by a query (empty string returns all).",
      strict: true,
      input_schema: obj({ query: { type: "string" } }, ["query"]),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      const q = String(input.query ?? "");
      const notes = ctx.memory.listNotes(scopeOf(ctx), q || undefined);
      if (!notes.length) return "no matching notes";
      return notes.map(([k, v]) => `${k}: ${v}`).join("\n");
    },
  },
  {
    tool: {
      name: "update_ledger",
      description: "Set the status (todo|doing|done) of a ledger item, adding it if new.",
      strict: true,
      input_schema: obj(
        { item: { type: "string" }, status: { type: "string", enum: ["todo", "doing", "done"] } },
        ["item", "status"],
      ),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      const status = input.status as "todo" | "doing" | "done";
      const ledger = ctx.memory.setLedgerItem(scopeOf(ctx), String(input.item), status);
      return ledger.map((i) => `[${i.status}] ${i.text}`).join("; ") || "ledger empty";
    },
  },
  {
    tool: {
      name: "read_ledger",
      description: "Read the current task ledger.",
      strict: true,
      input_schema: obj({}, []),
    },
    run: async (ctx: SkillContext, _input: any): Promise<string> => {
      const ledger = ctx.memory.ledger(scopeOf(ctx));
      if (!ledger.length) return "ledger empty";
      return ledger.map((i) => `[${i.status}] ${i.text}`).join("\n");
    },
  },
];
