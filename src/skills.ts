import type Anthropic from "@anthropic-ai/sdk";
import type { Bot } from "mineflayer";
import type { Vec3 as Vec3T } from "vec3";
import { obj, type SkillContext } from "./skillkit.js";
import { ALL_BEHAVIORS, ALL_SKILLS } from "./registry.js";
import { Vec3, goals, nearestHostile, sleep, withTimeout } from "./deps.js";

/** Compact perception snapshot fed to the planner every step. */
export function observe(bot: Bot): string {
  const p = bot.entity?.position;
  const pos = p ? `(${p.x.toFixed(0)}, ${p.y.toFixed(0)}, ${p.z.toFixed(0)})` : "unknown";
  const inv = bot.inventory.items().map((i) => `${i.name}x${i.count}`).join(", ") || "empty";
  const players = Object.keys(bot.players).filter((n) => n !== bot.username).join(", ") || "none";
  const mobs = p
    ? Object.values(bot.entities)
        .filter((e) => e.type === "mob" && e.position.distanceTo(p) < 16)
        .map((e) => e.name)
        .slice(0, 8)
        .join(", ") || "none"
    : "none";
  return [
    `position=${pos} health=${bot.health ?? "?"} food=${bot.food ?? "?"} held=${bot.heldItem?.name ?? "nothing"}`,
    `inventory: ${inv}`,
    `players nearby: ${players}`,
    `mobs within 16m: ${mobs}`,
  ].join("\n");
}

const BASE_TOOLS: Anthropic.Tool[] = [
  { name: "chat", description: "Send a message to public chat (everyone).", strict: true, input_schema: obj({ message: { type: "string" } }, ["message"]) },
  { name: "say_to", description: "Reply to a player in public chat, addressed as @<player>.", strict: true, input_schema: obj({ player: { type: "string" }, message: { type: "string" } }, ["player", "message"]) },
  { name: "whisper", description: "Reply privately to a player (/msg).", strict: true, input_schema: obj({ player: { type: "string" }, message: { type: "string" } }, ["player", "message"]) },
  { name: "list_inventory", description: "List everything the bot is carrying.", strict: true, input_schema: obj({}, []) },
  {
    name: "find_blocks",
    description: "Locate the nearest blocks of a type (e.g. oak_log, stone, iron_ore). Returns coordinates.",
    strict: true,
    input_schema: obj({ name: { type: "string" }, count: { type: "integer" }, max_distance: { type: "integer" } }, ["name", "count", "max_distance"]),
  },
  { name: "go_to", description: "Walk to a coordinate.", strict: true, input_schema: obj({ x: { type: "integer" }, y: { type: "integer" }, z: { type: "integer" } }, ["x", "y", "z"]) },
  { name: "go_to_player", description: "Walk to within 2 blocks of a named player.", strict: true, input_schema: obj({ username: { type: "string" } }, ["username"]) },
  { name: "collect_block", description: "Find, mine, and pick up N blocks of a type (handles tools and pathing).", strict: true, input_schema: obj({ name: { type: "string" }, count: { type: "integer" } }, ["name", "count"]) },
  { name: "mine_block", description: "Dig the block at an exact coordinate.", strict: true, input_schema: obj({ x: { type: "integer" }, y: { type: "integer" }, z: { type: "integer" } }, ["x", "y", "z"]) },
  { name: "place_block", description: "Place a carried block at a coordinate (needs a solid block below it).", strict: true, input_schema: obj({ name: { type: "string" }, x: { type: "integer" }, y: { type: "integer" }, z: { type: "integer" } }, ["name", "x", "y", "z"]) },
  { name: "craft_item", description: "Craft an item; uses a nearby crafting table if one is required.", strict: true, input_schema: obj({ name: { type: "string" }, count: { type: "integer" } }, ["name", "count"]) },
  { name: "equip_item", description: "Equip a carried item.", strict: true, input_schema: obj({ name: { type: "string" }, destination: { type: "string", enum: ["hand", "head", "torso", "legs", "feet", "off-hand"] } }, ["name", "destination"]) },
  { name: "attack_nearest", description: "Approach and hit the nearest hostile mob once.", strict: true, input_schema: obj({}, []) },
  { name: "attack_player", description: "Approach and hit a specific player once.", strict: true, input_schema: obj({ username: { type: "string" } }, ["username"]) },
  { name: "fight", description: "Sustained melee until the target dies, flees, or ~30s pass. target = \"nearest\" (nearest hostile), a mob name (e.g. zombie), or a player username.", strict: true, input_schema: obj({ target: { type: "string" } }, ["target"]) },
  { name: "flee", description: "Run away from the nearest hostile mob to roughly <distance> blocks (max 32).", strict: true, input_schema: obj({ distance: { type: "integer" } }, ["distance"]) },
  { name: "follow_player", description: "Continuously follow a player for up to <seconds> (max 300), staying ~2 blocks away.", strict: true, input_schema: obj({ username: { type: "string" }, seconds: { type: "integer" } }, ["username", "seconds"]) },
  { name: "deposit", description: "Put items into a chest at a coordinate (move next to it first).", strict: true, input_schema: obj({ item: { type: "string" }, count: { type: "integer" }, x: { type: "integer" }, y: { type: "integer" }, z: { type: "integer" } }, ["item", "count", "x", "y", "z"]) },
  { name: "withdraw", description: "Take items from a chest at a coordinate (move next to it first).", strict: true, input_schema: obj({ item: { type: "string" }, count: { type: "integer" }, x: { type: "integer" }, y: { type: "integer" }, z: { type: "integer" } }, ["item", "count", "x", "y", "z"]) },
  { name: "match_block_names", description: "Search block names by regex — Minecraft names are often unintuitive (e.g. 'log' matches oak_log, spruce_log). Returns matching names.", strict: true, input_schema: obj({ pattern: { type: "string" }, limit: { type: "integer" } }, ["pattern", "limit"]) },
  { name: "scan_area", description: "Wide look around: counts every solid block within a radius (max 8) by type. Use to understand surroundings.", strict: true, input_schema: obj({ radius: { type: "integer" } }, ["radius"]) },
  { name: "top_down", description: "Top-down 5x5 heightmap: for each column around you, the first block going down from eye height, with its height vs. the ground you stand on (eye=+2, waist=+1, ground=0, below negative).", strict: true, input_schema: obj({}, []) },
  { name: "set_behavior", description: "Toggle a background auto-behavior that runs until turned off: defend, auto_eat, maintain_light, retreat_if_low_health, lava_guard, anti_stuck.", strict: true, input_schema: obj({ behavior: { type: "string", enum: ["defend", "auto_eat", "maintain_light", "retreat_if_low_health", "lava_guard", "anti_stuck"] }, enabled: { type: "boolean" } }, ["behavior", "enabled"]) },
  { name: "task_complete", description: "Call when the goal is achieved or is impossible. Ends the task.", strict: true, input_schema: obj({ summary: { type: "string" } }, ["summary"]) },
];

/** Base tools plus everything registered by the category modules. */
export const TOOLS: Anthropic.Tool[] = [...BASE_TOOLS, ...ALL_SKILLS.map((s) => s.tool)];

/** Per-tool compact summary for verbose results; the rest fall back to first-line + cap. */
const TOOL_SUMMARY: Record<string, (full: string) => string> = {
  top_down: () => "(top-down heightmap surveyed)",
  scan_area: () => "(area scanned)",
  find_blocks: (f) => `(located: ${f.split(";")[0].trim()}…)`,
  match_block_names: () => "(block-name search done)",
  match_item_names: () => "(item-name search done)",
  list_inventory: () => "(inventory checked)",
  get_recipe: (f) => `(recipe: ${f.split("\n")[0].slice(0, 80)})`,
  inventory_gap: (f) => `(gap: ${f.slice(0, 80)})`,
};

/** Deterministically shrink an old tool result for compacted history. No model call. */
export function summarizeResult(name: string, full: string): string {
  const fn = TOOL_SUMMARY[name];
  if (fn) return fn(full);
  const firstLine = full.split("\n")[0];
  return firstLine.length > 160 ? `${firstLine.slice(0, 157)}…` : firstLine;
}

type Input = Record<string, any>;

/** Executes one skill and returns a short natural-language result for the planner. */
export async function execute(bot: Bot, mcData: any, name: string, input: Input, ctx: SkillContext): Promise<string> {
  try {
    switch (name) {
      case "chat":
        bot.chat(String(input.message));
        return "message sent";

      case "say_to":
        bot.chat(`@${input.player} ${input.message}`);
        return `replied to ${input.player} in chat`;

      case "whisper":
        (bot as any).whisper(input.player, input.message);
        return `whispered ${input.player}`;

      case "list_inventory":
        return bot.inventory.items().map((i) => `${i.name}x${i.count}`).join(", ") || "inventory empty";

      case "find_blocks": {
        const block = mcData.blocksByName[input.name];
        if (!block) return `unknown block "${input.name}"`;
        const found: Vec3T[] = bot.findBlocks({ matching: block.id, maxDistance: input.max_distance, count: input.count });
        if (found.length === 0) return `no ${input.name} within ${input.max_distance}m`;
        return found.map((v) => `(${v.x}, ${v.y}, ${v.z})`).join("; ");
      }

      case "go_to":
        await withTimeout(bot.pathfinder.goto(new goals.GoalNear(input.x, input.y, input.z, 1)), 60_000, "go_to");
        return `arrived near (${input.x}, ${input.y}, ${input.z})`;

      case "go_to_player": {
        const player = bot.players[input.username]?.entity;
        if (!player) return `player "${input.username}" not visible`;
        const g = new goals.GoalNear(player.position.x, player.position.y, player.position.z, 2);
        await withTimeout(bot.pathfinder.goto(g), 60_000, "go_to_player");
        return `reached ${input.username}`;
      }

      case "collect_block": {
        const block = mcData.blocksByName[input.name];
        if (!block) return `unknown block "${input.name}"`;
        const targets = bot.findBlocks({ matching: block.id, maxDistance: 64, count: input.count });
        if (targets.length === 0) return `no ${input.name} nearby to collect`;
        const blocks = targets.map((v: Vec3T) => bot.blockAt(v)).filter(Boolean);
        await withTimeout((bot as any).collectBlock.collect(blocks), 120_000, "collect_block");
        return `collected up to ${blocks.length} ${input.name}`;
      }

      case "mine_block": {
        const block = bot.blockAt(new Vec3(input.x, input.y, input.z));
        if (!block || block.name === "air") return "no block at that coordinate";
        await withTimeout(bot.dig(block), 60_000, "mine_block");
        return `mined ${block.name}`;
      }

      case "place_block": {
        const item = bot.inventory.items().find((i) => i.name === input.name);
        if (!item) return `not carrying ${input.name}`;
        const below = bot.blockAt(new Vec3(input.x, input.y - 1, input.z));
        if (!below || below.name === "air") return "no solid block to place against below the target";
        await bot.equip(item, "hand");
        await withTimeout(bot.placeBlock(below, new Vec3(0, 1, 0)), 30_000, "place_block");
        return `placed ${input.name}`;
      }

      case "craft_item": {
        const item = mcData.itemsByName[input.name];
        if (!item) return `unknown item "${input.name}"`;
        const table = bot.findBlock({ matching: mcData.blocksByName.crafting_table.id, maxDistance: 4 });
        const recipe = bot.recipesFor(item.id, null, input.count, table)[0];
        if (!recipe) return `cannot craft ${input.name} (missing ingredients or a crafting table)`;
        await bot.craft(recipe, input.count, table ?? undefined);
        return `crafted ${input.count} ${input.name}`;
      }

      case "equip_item": {
        const item = bot.inventory.items().find((i) => i.name === input.name);
        if (!item) return `not carrying ${input.name}`;
        await bot.equip(item, input.destination);
        return `equipped ${input.name}`;
      }

      case "attack_nearest": {
        const target = nearestHostile(bot);
        if (!target) return "no hostile mob nearby";
        const g = new goals.GoalNear(target.position.x, target.position.y, target.position.z, 2);
        await withTimeout(bot.pathfinder.goto(g), 20_000, "approach").catch(() => {});
        bot.attack(target);
        return `attacked ${target.name}`;
      }

      case "attack_player": {
        const target = bot.players[input.username]?.entity;
        if (!target) return `player "${input.username}" not visible`;
        const g = new goals.GoalNear(target.position.x, target.position.y, target.position.z, 2);
        await withTimeout(bot.pathfinder.goto(g), 20_000, "approach").catch(() => {});
        bot.attack(target);
        return `attacked ${input.username}`;
      }

      case "fight": {
        let target: any;
        if (input.target === "nearest") target = nearestHostile(bot);
        else target = bot.players[input.target]?.entity ?? bot.nearestEntity((e) => e.name === input.target || (e as any).username === input.target);
        if (!target) return `no target "${input.target}" nearby`;
        const label = (target as any).username ?? target.name;
        (bot as any).pvp.attack(target);
        await new Promise<void>((resolve) => {
          const done = () => {
            clearTimeout(timer);
            (bot as any).removeListener("stoppedAttacking", done);
            resolve();
          };
          const timer = setTimeout(done, 30_000);
          (bot as any).once("stoppedAttacking", done);
        });
        (bot as any).pvp.stop();
        return `finished fighting ${label}`;
      }

      case "flee": {
        const threat = nearestHostile(bot);
        if (!threat) return "no hostile mob to flee from";
        const p = bot.entity.position;
        const away = p.minus(threat.position);
        const mag = away.norm();
        const dir = mag > 0.001 ? away.scaled(1 / mag) : new Vec3(1, 0, 0);
        const dist = Math.min(Math.max(Number(input.distance), 4), 32);
        const dest = p.plus(dir.scaled(dist));
        await withTimeout(bot.pathfinder.goto(new goals.GoalNear(dest.x, dest.y, dest.z, 2)), 30_000, "flee").catch(() => {});
        return `fled from ${threat.name}`;
      }

      case "follow_player": {
        const entity = bot.players[input.username]?.entity;
        if (!entity) return `player "${input.username}" not visible`;
        bot.pathfinder.setGoal(new goals.GoalFollow(entity, 2), true);
        const deadline = Date.now() + Math.min(Number(input.seconds), 300) * 1000;
        while (Date.now() < deadline && bot.players[input.username]?.entity) await sleep(1000);
        bot.pathfinder.setGoal(null);
        return `followed ${input.username}`;
      }

      case "deposit":
      case "withdraw": {
        const info = mcData.itemsByName[input.item];
        if (!info) return `unknown item "${input.item}"`;
        const block = bot.blockAt(new Vec3(input.x, input.y, input.z));
        if (!block) return "no block at that coordinate";
        const chest: any = await withTimeout(bot.openContainer(block), 15_000, "open chest");
        try {
          if (name === "deposit") {
            const have = bot.inventory.items().filter((i) => i.name === input.item).reduce((s, i) => s + i.count, 0);
            const n = Math.min(Number(input.count), have);
            if (n <= 0) return `not carrying any ${input.item}`;
            await chest.deposit(info.id, null, n);
            return `deposited ${n} ${input.item}`;
          }
          const avail = chest.containerItems().filter((i: any) => i.name === input.item).reduce((s: number, i: any) => s + i.count, 0);
          const n = Math.min(Number(input.count), avail);
          if (n <= 0) return `chest has no ${input.item}`;
          await chest.withdraw(info.id, null, n);
          return `withdrew ${n} ${input.item}`;
        } finally {
          chest.close();
        }
      }

      case "match_block_names": {
        let re: RegExp;
        try {
          re = new RegExp(input.pattern, "i");
        } catch {
          return `invalid regex: ${input.pattern}`;
        }
        const names = Object.keys(mcData.blocksByName).filter((n) => re.test(n)).slice(0, input.limit);
        return names.length ? names.join(", ") : `no block names match /${input.pattern}/`;
      }

      case "scan_area": {
        const r = Math.min(Math.max(Number(input.radius), 1), 8);
        const origin = bot.entity.position.floored();
        const counts = new Map<string, number>();
        for (let dx = -r; dx <= r; dx++)
          for (let dy = -r; dy <= r; dy++)
            for (let dz = -r; dz <= r; dz++) {
              const b = bot.blockAt(origin.offset(dx, dy, dz));
              if (b && b.name !== "air") counts.set(b.name, (counts.get(b.name) ?? 0) + 1);
            }
        if (counts.size === 0) return "only air within range";
        return [...counts.entries()]
          .sort((a, b) => b[1] - a[1])
          .slice(0, 25)
          .map(([n, c]) => `${n} x${c}`)
          .join(", ");
      }

      case "top_down": {
        const pos = bot.entity.position;
        const footY = Math.floor(pos.y);
        const groundY = footY - 1; // block you stand on = level 0
        const startY = footY + 1; // eye block = level +2
        const cx = Math.floor(pos.x);
        const cz = Math.floor(pos.z);
        const rows: string[] = [];
        for (let dz = -2; dz <= 2; dz++) {
          const cells: string[] = [];
          for (let dx = -2; dx <= 2; dx++) {
            let cell = "∅";
            for (let y = startY; y >= startY - 32; y--) {
              const b = bot.blockAt(new Vec3(cx + dx, y, cz + dz));
              if (b && b.name !== "air") {
                const level = y - groundY;
                cell = `${b.name}${level >= 0 ? "+" : ""}${level}`;
                break;
              }
            }
            cells.push(cell);
          }
          rows.push(cells.join("  "));
        }
        return `top-down 5x5 (rows north→south, cols west→east, you=center)\n${rows.join("\n")}`;
      }

      case "set_behavior": {
        const set = (bot as any)._behaviors as Set<string>;
        if (input.enabled) set.add(input.behavior);
        else set.delete(input.behavior);
        return `${input.behavior} ${input.enabled ? "enabled" : "disabled"}`;
      }

      default: {
        const skill = ALL_SKILLS.find((s) => s.tool.name === name);
        if (skill) return skill.run(ctx, input);
        return `unknown skill "${name}"`;
      }
    }
  } catch (err) {
    return `error: ${(err as Error).message}`;
  }
}

/** Wires background toggles. Built-in defend/auto_eat plus any registered BehaviorHandlers. */
export function installAutoBehaviors(bot: Bot, getMcData: () => any, isFriendly: (name: string) => boolean, makeCtx: () => SkillContext): void {
  let lastHealth = bot.health ?? 20;
  bot.on("health", () => {
    const ctx = makeCtx();
    const behaviors = ctx.behaviors;
    if (behaviors.has("auto_eat") && bot.food <= 14) void autoEat(bot, getMcData());
    if (bot.health < lastHealth && behaviors.has("defend")) defend(bot, isFriendly);
    for (const h of ALL_BEHAVIORS) if (h.onHealth && behaviors.has(h.name)) try { h.onHealth(ctx); } catch { /* ignore */ }
    lastHealth = bot.health;
  });
  const tick = setInterval(() => {
    const ctx = makeCtx();
    for (const h of ALL_BEHAVIORS) if (h.onTick && ctx.behaviors.has(h.name)) try { h.onTick(ctx); } catch { /* ignore */ }
  }, 1000);
  bot.once("end", () => clearInterval(tick));
}

async function autoEat(bot: Bot, mcData: any): Promise<void> {
  if ((bot as any)._eating) return;
  const foods = mcData?.foodsByName ?? {};
  const item = bot.inventory.items().find((i) => foods[i.name]);
  if (!item) return;
  try {
    (bot as any)._eating = true;
    await bot.equip(item, "hand");
    await (bot as any).consume();
  } catch {
    /* interrupted or not edible right now */
  } finally {
    (bot as any)._eating = false;
  }
}

/** Hit back the nearest attacker: hostile mobs, or non-friendly players (never the owner/other agents). */
function defend(bot: Bot, isFriendly: (name: string) => boolean): void {
  const p = bot.entity?.position;
  if (!p) return;
  const target = bot.nearestEntity((e) => {
    if (!e.position || e.position.distanceTo(p) > 5) return false;
    if (e.type === "mob" && (e as any).kind === "Hostile mobs") return true;
    if (e.type === "player" && !!e.username && !isFriendly(e.username)) return true;
    return false;
  });
  if (target) bot.attack(target);
}
