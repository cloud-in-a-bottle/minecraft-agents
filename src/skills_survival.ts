import { obj, type BehaviorHandler, type Skill, type SkillContext } from "./skillkit.js";
import { Vec3, goals, nearestHostile, sleep } from "./deps.js";

const isHazardName = (n: string | undefined): boolean =>
  !n || n === "air" || n === "cave_air" || n === "void_air" || /lava|water/.test(n);

const isLava = (b: any): boolean => !!b && /lava/.test(b.name);

/** Horizontal unit-block offset the bot is facing, from its yaw. */
function headingOffset(bot: any): [number, number] {
  const yaw = bot.entity?.yaw ?? 0;
  const fx = -Math.sin(yaw);
  const fz = -Math.cos(yaw);
  const dx = Math.abs(fx) >= Math.abs(fz) ? Math.sign(fx) : 0;
  const dz = Math.abs(fz) > Math.abs(fx) ? Math.sign(fz) : 0;
  return [dx, dz];
}

/** Nearest hostile mob to the bot, or undefined. */
export const skills: Skill[] = [
  {
    tool: {
      name: "dig_down_safe",
      description: "Mine straight down up to <depth> blocks, stopping before any lava/water/void two blocks below. Returns blocks descended.",
      strict: true,
      input_schema: obj({ depth: { type: "integer" } }, ["depth"]),
    },
    async run(ctx: SkillContext, input: any): Promise<string> {
      const bot = ctx.bot as any;
      const depth = Math.max(1, Math.min(Number(input.depth) || 0, 64));
      let descended = 0;
      for (let i = 0; i < depth; i++) {
        const pos = bot.entity.position.floored();
        const below2 = bot.blockAt(pos.offset(0, -2, 0));
        if (isHazardName(below2?.name)) return `stopped after ${descended} blocks: hazard below (${below2?.name ?? "void"})`;
        const target = bot.blockAt(pos.offset(0, -1, 0));
        if (target && target.name !== "air") {
          try {
            await bot.dig(target);
          } catch (err) {
            return `stopped after ${descended} blocks: dig failed (${(err as Error).message})`;
          }
        }
        await sleep(400);
        descended++;
      }
      return `descended ${descended} blocks`;
    },
  },
];

export const behaviors: BehaviorHandler[] = [
  {
    name: "maintain_light",
    onTick(ctx: SkillContext): void {
      const bot = ctx.bot as any;
      if (bot._litUp) return;
      const feet = bot.entity?.position;
      if (!feet) return;
      const b = bot.blockAt(feet);
      const light = b ? (b.light ?? b.skyLight ?? 15) : 15;
      if (light >= 8) return;
      const torch = bot.inventory.items().find((i: any) => i.name.includes("torch"));
      if (!torch) return;
      bot._litUp = true;
      void (async () => {
        try {
          await bot.equip(torch, "hand");
          const base = feet.floored();
          const refs = [
            base.offset(0, -1, 0),
            base.offset(1, 0, 0),
            base.offset(-1, 0, 0),
            base.offset(0, 0, 1),
            base.offset(0, 0, -1),
          ];
          for (const r of refs) {
            const rb = bot.blockAt(r);
            if (rb && rb.name !== "air" && rb.boundingBox === "block") {
              const face = base.minus(r);
              await bot.placeBlock(rb, new Vec3(face.x, face.y, face.z));
              break;
            }
          }
        } catch {
          /* ignore */
        } finally {
          bot._litUp = false;
        }
      })();
    },
  },
  {
    name: "retreat_if_low_health",
    onHealth(ctx: SkillContext): void {
      const bot = ctx.bot as any;
      if (bot.health > 7 || bot._retreating) return;
      const p = bot.entity?.position;
      if (!p) return;
      const threat = nearestHostile(bot);
      bot._retreating = true;
      void (async () => {
        try {
          let dest;
          if (threat) {
            const away = p.minus(threat.position);
            const mag = away.norm();
            const dir = mag > 0.001 ? away.scaled(1 / mag) : new Vec3(1, 0, 0);
            dest = p.plus(dir.scaled(10));
          } else {
            dest = p.offset(10, 0, 0);
          }
          await bot.pathfinder.goto(new goals.GoalNear(dest.x, dest.y, dest.z, 2));
        } catch {
          /* ignore */
        } finally {
          setTimeout(() => {
            bot._retreating = false;
          }, 2000);
        }
      })();
    },
  },
  {
    name: "lava_guard",
    onTick(ctx: SkillContext): void {
      const bot = ctx.bot as any;
      if (bot._lavaGuarding) return;
      const p = bot.entity?.position;
      if (!p) return;
      const [dx, dz] = headingOffset(bot);
      if (dx === 0 && dz === 0) return;
      const ahead = p.floored().offset(dx, 0, dz);
      const aheadBlock = bot.blockAt(ahead);
      const belowAhead = bot.blockAt(ahead.offset(0, -1, 0));
      let drop = 0;
      for (let i = 1; i <= 4; i++) {
        const bb = bot.blockAt(ahead.offset(0, -i, 0));
        if (bb && bb.name !== "air" && bb.name !== "cave_air") break;
        drop++;
      }
      if (!isLava(aheadBlock) && !isLava(belowAhead) && drop <= 3) return;
      bot._lavaGuarding = true;
      void (async () => {
        try {
          bot.pathfinder?.stop?.();
          bot.pathfinder?.setGoal?.(null);
          bot.clearControlStates?.();
          bot.setControlState("forward", false);
          bot.setControlState("back", true);
          await sleep(300);
          bot.setControlState("back", false);
        } catch {
          /* ignore */
        } finally {
          setTimeout(() => {
            bot._lavaGuarding = false;
          }, 500);
        }
      })();
    },
  },
  {
    name: "anti_stuck",
    onTick(ctx: SkillContext): void {
      const bot = ctx.bot as any;
      if (!bot.pathfinder?.isMoving?.()) {
        bot._stallCount = 0;
        return;
      }
      const p = bot.entity?.position;
      if (!p) return;
      const last = bot._lastPos;
      const moved = !last || p.distanceTo(last) > 0.2;
      bot._lastPos = p.clone();
      if (moved) {
        bot._stallCount = 0;
        return;
      }
      bot._stallCount = (bot._stallCount ?? 0) + 1;
      if (bot._stallCount < 3 || bot._unsticking) return;
      bot._unsticking = true;
      bot._stallCount = 0;
      void (async () => {
        try {
          bot.setControlState("jump", true);
          await sleep(300);
          bot.setControlState("jump", false);
          const [dx, dz] = headingOffset(bot);
          const ahead = p.floored().offset(dx, 0, dz);
          const b = bot.blockAt(ahead);
          if (b && b.name !== "air" && b.boundingBox === "block") await bot.dig(b).catch(() => {});
        } catch {
          /* ignore */
        } finally {
          bot._unsticking = false;
        }
      })();
    },
  },
];
