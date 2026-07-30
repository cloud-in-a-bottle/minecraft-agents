import { obj, type Skill, type SkillContext } from "./skillkit.js";
import { Vec3, goals, withTimeout } from "./deps.js";

export const skills: Skill[] = [
  {
    tool: {
      name: "summon_agents",
      description: "Summon <count> helper agents to work on <goal>. They are owned by your owner and count toward that player's agent cap.",
      strict: true,
      input_schema: obj({ count: { type: "integer" }, goal: { type: "string" } }, ["count", "goal"]),
    },
    run: async (ctx: SkillContext, input): Promise<string> => {
      const r = ctx.peers.summon(input.count, input.goal, ctx.self.owner);
      if (!r.created.length) return r.reason === "user_limit" ? "your owner's agent cap is reached" : "cannot summon — agent limit reached";
      return `summoned ${r.created.join(", ")} for: ${input.goal}` + (r.rejected ? ` (${r.rejected} over cap)` : "");
    },
  },
  {
    tool: {
      name: "activate_block",
      description: "Right-click the block at a coordinate to toggle a door, lever, button, or pressure plate.",
      strict: true,
      input_schema: obj({ x: { type: "integer" }, y: { type: "integer" }, z: { type: "integer" } }, ["x", "y", "z"]),
    },
    run: async (ctx: SkillContext, input): Promise<string> => {
      const bot = ctx.bot as any;
      try {
        const block = bot.blockAt(new Vec3(input.x, input.y, input.z));
        if (!block || block.name === "air") return "no block at that coordinate";
        await withTimeout(bot.activateBlock(block), 15_000);
        return `activated ${block.name}`;
      } catch (err) {
        return `error: ${(err as Error).message}`;
      }
    },
  },
  {
    tool: {
      name: "collect_drops",
      description: "Walk over nearby dropped items within a radius to pick them up. Returns how many were gathered.",
      strict: true,
      input_schema: obj({ radius: { type: "integer" } }, ["radius"]),
    },
    run: async (ctx: SkillContext, input): Promise<string> => {
      const bot = ctx.bot as any;
      try {
        const origin = bot.entity?.position;
        if (!origin) return "position unknown";
        const radius = Math.max(1, Number(input.radius));
        const drops = Object.values(bot.entities)
          .filter((e: any) => (e.name === "item" || e.objectType === "Item") && e.position && e.position.distanceTo(origin) <= radius)
          .slice(0, 10);
        if (drops.length === 0) return `no dropped items within ${radius}m`;
        let gathered = 0;
        for (const e of drops as any[]) {
          try {
            const p = e.position;
            await withTimeout(bot.pathfinder.goto(new goals.GoalNear(p.x, p.y, p.z, 0)), 8_000);
            gathered++;
          } catch {
            /* skip unreachable drop */
          }
        }
        return `gathered ${gathered} of ${drops.length} dropped item(s)`;
      } catch (err) {
        return `error: ${(err as Error).message}`;
      }
    },
  },
  {
    tool: {
      name: "give_item",
      description: "Toss items to another agent or a human player: face them and drop the given count of the item.",
      strict: true,
      input_schema: obj({ target: { type: "string" }, item: { type: "string" }, count: { type: "integer" } }, ["target", "item", "count"]),
    },
    run: async (ctx: SkillContext, input): Promise<string> => {
      const bot = ctx.bot as any;
      try {
        const info = ctx.mcData.itemsByName[input.item];
        if (!info) return `unknown item "${input.item}"`;
        const have = bot.inventory.items().filter((i: any) => i.name === input.item).reduce((s: number, i: any) => s + i.count, 0);
        if (have <= 0) return `not carrying any ${input.item}`;
        const peer = ctx.peers.position(input.target);
        const pos = peer ?? bot.players[input.target]?.entity?.position;
        if (!pos) return `target "${input.target}" not found`;
        const n = Math.min(Number(input.count), have);
        await bot.lookAt(new Vec3(pos.x, pos.y, pos.z));
        await withTimeout(bot.toss(info.id, null, n), 15_000);
        return `gave ${n} ${input.item} to ${input.target}`;
      } catch (err) {
        return `error: ${(err as Error).message}`;
      }
    },
  },
  {
    tool: {
      name: "go_to_agent",
      description: "Walk to within <range> blocks of another agent's current position.",
      strict: true,
      input_schema: obj({ agent: { type: "string" }, range: { type: "integer" } }, ["agent", "range"]),
    },
    run: async (ctx: SkillContext, input): Promise<string> => {
      const bot = ctx.bot as any;
      try {
        const p = ctx.peers.position(input.agent);
        if (!p) return `agent "${input.agent}" not online/visible`;
        await withTimeout(bot.pathfinder.goto(new goals.GoalNear(p.x, p.y, p.z, Math.max(1, Number(input.range)))), 60_000);
        return `reached ${input.agent}`;
      } catch (err) {
        return `error: ${(err as Error).message}`;
      }
    },
  },
  {
    tool: {
      name: "send_agent_message",
      description: "Send an in-process message to another agent's planning loop (coordination, no game chat).",
      strict: true,
      input_schema: obj({ agent: { type: "string" }, message: { type: "string" } }, ["agent", "message"]),
    },
    run: async (ctx: SkillContext, input): Promise<string> => {
      try {
        const ok = ctx.peers.send(input.agent, ctx.self.username, String(input.message));
        return ok ? `delivered to ${input.agent}` : `${input.agent} not online`;
      } catch (err) {
        return `error: ${(err as Error).message}`;
      }
    },
  },
];
