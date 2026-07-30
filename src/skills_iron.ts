import { obj, type Skill, type SkillContext } from "./skillkit.js";
import { Vec3, goals, sleep } from "./deps.js";

/** Consumed ingredients of a recipe as id→count, from delta/ingredients/inShape. */
function recipeIngredients(recipe: any): Map<number, number> {
  const m = new Map<number, number>();
  const add = (id: number, c: number): void => {
    if (id != null && id >= 0 && c > 0) m.set(id, (m.get(id) ?? 0) + c);
  };
  if (Array.isArray(recipe.delta)) for (const d of recipe.delta) { if (d.count < 0) add(d.id, -d.count); }
  else if (Array.isArray(recipe.ingredients)) for (const d of recipe.ingredients) add(d.id, Math.abs(d.count ?? 1));
  else if (Array.isArray(recipe.inShape)) for (const row of recipe.inShape) for (const c of row) if (c) add(c.id, c.count ?? 1);
  return m;
}

const CARDINAL: Record<string, [number, number]> = {
  north: [0, -1], south: [0, 1], east: [1, 0], west: [-1, 0],
};

/** Cardinal forward step derived from the bot's yaw. */
function forwardStep(bot: any): any {
  const yaw = bot.entity.yaw as number;
  const sx = -Math.sin(yaw), cz = Math.cos(yaw);
  return Math.abs(sx) > Math.abs(cz)
    ? new Vec3(Math.sign(sx) || 1, 0, 0)
    : new Vec3(0, 0, Math.sign(cz) || 1);
}

/** Dig one block, refusing lava and undiggable blocks. Returns whether it dug. */
async function digSafe(bot: any, pos: any): Promise<boolean> {
  const b = bot.blockAt(pos);
  if (!b || b.name === "air") return false;
  if (b.name.includes("lava")) throw new Error(`lava at (${pos.x}, ${pos.y}, ${pos.z})`);
  if (!bot.canDigBlock(b)) return false;
  await bot.dig(b);
  return true;
}

/** Best-effort torch on a nearby wall; failures are ignored. */
async function placeTorch(bot: any): Promise<void> {
  const torch = bot.inventory.items().find((i: any) => i.name === "torch");
  if (!torch) return;
  const base = bot.entity.position.floored();
  for (const [dx, dz] of [[1, 0], [-1, 0], [0, 1], [0, -1]] as [number, number][]) {
    const ref = bot.blockAt(base.offset(dx, -1, dz));
    if (ref && ref.name !== "air" && !ref.name.includes("lava")) {
      try {
        await bot.equip(torch, "hand");
        await bot.placeBlock(ref, new Vec3(0, 1, 0));
        return;
      } catch { /* try next face */ }
    }
  }
}

export const skills: Skill[] = [
  {
    tool: {
      name: "match_item_names",
      description: "Search item names by regex (case-insensitive) — Minecraft item ids are often unintuitive. Returns matching names.",
      strict: true,
      input_schema: obj({ pattern: { type: "string" }, limit: { type: "integer" } }, ["pattern", "limit"]),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      let re: RegExp;
      try { re = new RegExp(input.pattern, "i"); } catch { return `invalid regex: ${input.pattern}`; }
      const names = Object.keys(ctx.mcData.itemsByName).filter((n) => re.test(n)).slice(0, input.limit);
      return names.length ? names.join(", ") : "no item matches";
    },
  },
  {
    tool: {
      name: "get_recipe",
      description: "Show the first crafting recipe for an item: ingredients with counts, output count, and whether a crafting table is required.",
      strict: true,
      input_schema: obj({ item: { type: "string" } }, ["item"]),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      const bot = ctx.bot as any, mcData = ctx.mcData as any;
      const info = mcData.itemsByName[input.item];
      if (!info) return `unknown item "${input.item}"`;
      try {
        const recipe = bot.recipesAll(info.id, null, true)[0];
        if (!recipe) return `no recipe for ${input.item}`;
        const ings = [...recipeIngredients(recipe).entries()]
          .map(([id, c]) => `${mcData.items[id]?.name ?? id} x${c}`)
          .join(", ");
        const out = recipe.result?.count ?? 1;
        return `${input.item} x${out} from ${ings || "unknown ingredients"} (crafting table ${recipe.requiresTable ? "required" : "not needed"})`;
      } catch (err) {
        return `error: ${(err as Error).message}`;
      }
    },
  },
  {
    tool: {
      name: "get_block_info",
      description: "Report a block's hardness, the tool tier needed to get drops, and whether the held item can harvest it.",
      strict: true,
      input_schema: obj({ block: { type: "string" } }, ["block"]),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      const bot = ctx.bot as any, mcData = ctx.mcData as any;
      const b = mcData.blocksByName[input.block];
      if (!b) return `unknown block "${input.block}"`;
      const tools = b.harvestTools as Record<string, unknown> | undefined;
      const toolNames = tools
        ? Object.keys(tools).map((id) => mcData.items[Number(id)]?.name ?? id).join("/")
        : "any";
      const held = bot.heldItem;
      const canHarvest = !tools ? true : !!(held && tools[String(held.type)]);
      return `${input.block}: hardness ${b.hardness ?? "unbreakable"}, harvest with ${toolNames}; held item ${held?.name ?? "none"} ${canHarvest ? "can" : "cannot"} harvest it for drops`;
    },
  },
  {
    tool: {
      name: "inventory_gap",
      description: "Recursively expand a target item's recipe tree, subtract current inventory, and list the base materials still missing.",
      strict: true,
      input_schema: obj({ item: { type: "string" }, count: { type: "integer" } }, ["item", "count"]),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      const bot = ctx.bot as any, mcData = ctx.mcData as any;
      const info = mcData.itemsByName[input.item];
      if (!info) return `unknown item "${input.item}"`;
      try {
        const need = new Map<number, number>();
        const expand = (id: number, count: number, depth: number): void => {
          const recipe = depth > 0 ? bot.recipesAll(id, null, true)[0] : null;
          if (!recipe) { need.set(id, (need.get(id) ?? 0) + count); return; }
          const out = recipe.result?.count ?? 1;
          const times = Math.ceil(count / out);
          for (const [ingId, ingCount] of recipeIngredients(recipe)) expand(ingId, ingCount * times, depth - 1);
        };
        expand(info.id, input.count, 6);
        const have = new Map<number, number>();
        for (const it of bot.inventory.items()) have.set(it.type, (have.get(it.type) ?? 0) + it.count);
        const missing = [...need.entries()]
          .map(([id, c]) => [id, c - (have.get(id) ?? 0)] as [number, number])
          .filter(([, c]) => c > 0)
          .map(([id, c]) => `${mcData.items[id]?.name ?? id} x${c}`);
        return missing.length ? `missing base materials: ${missing.join(", ")}` : "all base materials already on hand";
      } catch (err) {
        return `error: ${(err as Error).message}`;
      }
    },
  },
  {
    tool: {
      name: "smelt",
      description: "Smelt items in the nearest furnace (within 6 blocks) using the given fuel. If no furnace is near, asks the planner to place one via craft_station.",
      strict: true,
      input_schema: obj({ input: { type: "string" }, fuel: { type: "string" }, count: { type: "integer" } }, ["input", "fuel", "count"]),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      const bot = ctx.bot as any, mcData = ctx.mcData as any;
      const inInfo = mcData.itemsByName[input.input];
      const fuelInfo = mcData.itemsByName[input.fuel];
      if (!inInfo) return `unknown input item "${input.input}"`;
      if (!fuelInfo) return `unknown fuel item "${input.fuel}"`;
      const block = bot.findBlock({ matching: mcData.blocksByName.furnace.id, maxDistance: 6 });
      if (!block) return "no furnace within 6 blocks — use craft_station to place a furnace first";
      let f: any;
      try {
        f = await bot.openFurnace(block);
        const count = Math.max(1, Math.min(Number(input.count), 64));
        const fuelN = Math.max(1, Math.ceil(count / 8));
        await f.putFuel(fuelInfo.id, null, fuelN);
        await f.putInput(inInfo.id, null, count);
        const deadline = Date.now() + count * 12_000;
        while (Date.now() < deadline) {
          await sleep(1000);
          const out = f.outputItem();
          if (out && out.count >= count) break;
        }
        const taken = f.outputItem() ? await f.takeOutput() : null;
        const n = taken?.count ?? 0;
        return n > 0 ? `smelted ${n} ${input.input}` : `nothing smelted (timed out or no fuel)`;
      } catch (err) {
        return `error: ${(err as Error).message}`;
      } finally {
        try { f?.close(); } catch { /* already closed */ }
      }
    },
  },
  {
    tool: {
      name: "craft_station",
      description: "Ensure a crafting_table, furnace, or blast_furnace is placed within reach. Returns its coordinates or a clear error.",
      strict: true,
      input_schema: obj({ station: { type: "string", enum: ["crafting_table", "furnace", "blast_furnace"] } }, ["station"]),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      const bot = ctx.bot as any, mcData = ctx.mcData as any;
      const blockDef = mcData.blocksByName[input.station];
      const itemDef = mcData.itemsByName[input.station];
      if (!blockDef || !itemDef) return `unknown station "${input.station}"`;
      const existing = bot.findBlock({ matching: blockDef.id, maxDistance: 4 });
      if (existing) return `${input.station} already at (${existing.position.x}, ${existing.position.y}, ${existing.position.z})`;
      try {
        let held = bot.inventory.items().find((i: any) => i.name === input.station);
        if (!held) {
          const table = bot.findBlock({ matching: mcData.blocksByName.crafting_table.id, maxDistance: 4 }) ?? undefined;
          const recipe = bot.recipesFor(itemDef.id, null, 1, table)[0];
          if (!recipe) return `error: cannot craft ${input.station} (missing materials${input.station !== "crafting_table" ? " or a crafting table" : ""})`;
          await bot.craft(recipe, 1, table);
          held = bot.inventory.items().find((i: any) => i.name === input.station);
          if (!held) return `error: crafted ${input.station} but it is not in inventory`;
        }
        const base = bot.entity.position.floored();
        for (const [dx, dz] of [[1, 0], [-1, 0], [0, 1], [0, -1]] as [number, number][]) {
          const target = base.offset(dx, 0, dz);
          const t = bot.blockAt(target);
          const below = bot.blockAt(target.offset(0, -1, 0));
          if (t && t.name === "air" && below && below.name !== "air" && !below.name.includes("lava")) {
            await bot.equip(held, "hand");
            await bot.placeBlock(below, new Vec3(0, 1, 0));
            return `placed ${input.station} at (${target.x}, ${target.y}, ${target.z})`;
          }
        }
        return `error: no valid spot to place ${input.station} nearby`;
      } catch (err) {
        return `error: ${(err as Error).message}`;
      }
    },
  },
  {
    tool: {
      name: "dig_staircase",
      description: "Dig a descending 2-high staircase down to a target Y (bounded), placing torches as it goes and stopping at lava.",
      strict: true,
      input_schema: obj({ target_y: { type: "integer" } }, ["target_y"]),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      const bot = ctx.bot as any;
      const fwd = forwardStep(bot);
      const startY = Math.floor(bot.entity.position.y);
      try {
        for (let i = 0; i < 80 && Math.floor(bot.entity.position.y) > input.target_y; i++) {
          const feet = bot.entity.position.floored();
          const step = feet.plus(fwd).offset(0, -1, 0);
          await digSafe(bot, step);
          await digSafe(bot, step.offset(0, 1, 0));
          await digSafe(bot, step.offset(0, 2, 0));
          await bot.pathfinder.goto(new goals.GoalNear(step.x, step.y, step.z, 0)).catch(() => {});
          if (i % 6 === 0) await placeTorch(bot);
        }
        const depth = Math.floor(bot.entity.position.y);
        return `staircase reached y=${depth} (from y=${startY})`;
      } catch (err) {
        return `error: ${(err as Error).message} at y=${Math.floor(bot.entity.position.y)}`;
      }
    },
  },
  {
    tool: {
      name: "strip_mine",
      description: "Dig a 1-wide, 2-high tunnel in a cardinal direction for N blocks (max 64), placing torches and stopping at lava.",
      strict: true,
      input_schema: obj({ direction: { type: "string", enum: ["north", "south", "east", "west"] }, length: { type: "integer" } }, ["direction", "length"]),
    },
    run: async (ctx: SkillContext, input: any): Promise<string> => {
      const bot = ctx.bot as any;
      const step = CARDINAL[input.direction];
      if (!step) return `unknown direction "${input.direction}"`;
      const [dx, dz] = step;
      const length = Math.max(0, Math.min(Number(input.length), 64));
      let mined = 0;
      try {
        for (let i = 1; i <= length; i++) {
          const feet = bot.entity.position.floored();
          const ahead = feet.offset(dx, 0, dz);
          if (await digSafe(bot, ahead)) mined++;
          if (await digSafe(bot, ahead.offset(0, 1, 0))) mined++;
          await bot.pathfinder.goto(new goals.GoalNear(ahead.x, ahead.y, ahead.z, 0)).catch(() => {});
          if (i % 6 === 0) await placeTorch(bot);
        }
        return `strip-mined ${input.direction} for ${length} blocks, removed ${mined} blocks`;
      } catch (err) {
        return `error: ${(err as Error).message} after removing ${mined} blocks`;
      }
    },
  },
];
