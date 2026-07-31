//! Iron-tier recipe/smelt/mining skills (port of skills_iron.ts).
//! azalea 0.15.1 exposes no client recipe DB, so recipe data comes from the static, name-keyed
//! `crate::recipes` table (generated from minecraft-data); matching is family-aware/type-agnostic.
use crate::llm::ToolDef;
use crate::recipes;
use crate::skill::rel;
use crate::skills::{SkillContext, Skill};
use crate::types::Pos;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

// TODO(verify): azalea re-export paths.
use azalea::block::{BlockState, BlockStates};
use azalea::entity::LookDirection;
use azalea::pathfinder::goals::BlockPosGoal;
use azalea::pathfinder::PathfinderClientExt;
use azalea::registry::builtin::BlockKind;
use azalea::registry::Registry;
use azalea::{BlockPos, Vec3};
use azalea::prelude::*; // BotClientExt, ContainerClientExt

const CARDINAL: &[(&str, (i32, i32))] =
    &[("north", (0, -1)), ("south", (0, 1)), ("east", (1, 0)), ("west", (-1, 0))];

// ---------- azalea helpers ----------

fn to_pos(v: Vec3) -> Pos {
    Pos { x: v.x, y: v.y, z: v.z }
}
fn bp_pos(bp: BlockPos) -> Pos {
    Pos { x: bp.x as f64, y: bp.y as f64, z: bp.z as f64 }
}
fn floor_pos(v: Vec3) -> BlockPos {
    BlockPos::new(v.x.floor() as i32, v.y.floor() as i32, v.z.floor() as i32)
}
fn sign_or1(v: f64) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        1
    }
}
fn dist(a: Vec3, b: Vec3) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

/// Player-inventory items as (item id, count), matching mineflayer `inventory.items()`.
fn inventory_items(ctx: &SkillContext) -> Vec<(u32, i32)> {
    let menu = ctx.bot.menu();
    let slots = menu.slots();
    menu.player_slots_range()
        .filter_map(|i| {
            let s = slots.get(i)?;
            if s.is_present() {
                Some((s.kind().to_u32(), s.count()))
            } else {
                None
            }
        })
        .collect()
}

fn have_item(ctx: &SkillContext, item_id: u32) -> bool {
    inventory_items(ctx).iter().any(|(id, _)| *id == item_id)
}

/// Player inventory as bare item name -> total count, for the name-keyed recipe table.
fn inventory_by_name(ctx: &SkillContext) -> HashMap<String, u32> {
    let mut m: HashMap<String, u32> = HashMap::new();
    for (id, count) in inventory_items(ctx) {
        if let Some(name) = ctx.mc_data.item_name(id) {
            *m.entry(name).or_default() += count.max(0) as u32;
        }
    }
    m
}

/// Select the hotbar slot holding `item_id`, if present. TODO(verify): hotbar-only equip.
fn equip_to_hand(ctx: &SkillContext, item_id: u32) -> bool {
    let menu = ctx.bot.menu();
    let slots = menu.slots();
    let range = menu.hotbar_slots_range();
    let start = *range.start();
    for i in range.clone() {
        if let Some(s) = slots.get(i) {
            if s.is_present() && s.kind().to_u32() == item_id {
                ctx.bot.set_selected_hotbar_slot((i - start) as u8); // TODO(verify)
                return true;
            }
        }
    }
    false
}

/// Block name at a position via mc_data, or None if unloaded.
fn block_name_at(ctx: &SkillContext, pos: BlockPos) -> Option<String> {
    let state: BlockState = ctx.bot.world().read().get_block_state(pos)?;
    let id = BlockKind::from(state).to_u32();
    ctx.mc_data.block_name(id)
}

/// Nearest block of `block_id` within `max_dist` (euclidean). TODO(verify): world find_block scan.
fn find_block_within(ctx: &SkillContext, block_id: u32, max_dist: f64) -> Option<BlockPos> {
    let kind = BlockKind::from_u32(block_id)?;
    let states: BlockStates = [kind].into();
    let here = ctx.bot.position();
    let found = ctx.bot.world().read().find_block(here, &states)?;
    if dist(here, found.center()) <= max_dist {
        Some(found)
    } else {
        None
    }
}

/// Cardinal forward step derived from the bot's yaw.
fn forward_step(ctx: &SkillContext) -> (i32, i32) {
    let yaw = ctx.bot.component::<LookDirection>().y_rot() as f64; // degrees; TODO(verify) convention
    let rad = yaw.to_radians();
    let sx = -rad.sin();
    let cz = rad.cos();
    if sx.abs() > cz.abs() {
        (sign_or1(sx), 0)
    } else {
        (0, sign_or1(cz))
    }
}

/// Dig one block, refusing lava and undiggable blocks. Returns whether it dug.
async fn dig_safe(ctx: &SkillContext, pos: BlockPos) -> anyhow::Result<bool> {
    let name = match block_name_at(ctx, pos) {
        Some(n) => n,
        None => return Ok(false),
    };
    if name == "air" {
        return Ok(false);
    }
    if name.contains("lava") {
        anyhow::bail!("lava at ({}, {}, {})", pos.x, pos.y, pos.z);
    }
    // canDigBlock: unbreakable (negative hardness) can't be dug.
    match ctx.mc_data.block_harvest(&name) {
        Some((h, _)) if h < 0.0 => return Ok(false),
        None => return Ok(false),
        _ => {}
    }
    let state = ctx.bot.world().read().get_block_state(pos);
    if let Some(state) = state {
        crate::mc::equip_best_tool(&ctx.bot, state, false).await;
    }
    ctx.bot.mine(pos).await;
    Ok(true)
}

/// Best-effort torch on a nearby wall; failures are ignored.
async fn place_torch(ctx: &SkillContext) {
    let torch_id = match ctx.mc_data.item_id("torch") {
        Some(id) => id,
        None => return,
    };
    if !have_item(ctx, torch_id) {
        return;
    }
    let base = floor_pos(ctx.bot.position());
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let ref_pos = BlockPos::new(base.x + dx, base.y - 1, base.z + dz);
        if let Some(n) = block_name_at(ctx, ref_pos) {
            if n != "air" && !n.contains("lava") {
                if equip_to_hand(ctx, torch_id) {
                    ctx.bot.look_at(ref_pos.center()); // TODO(verify) place on top face
                    ctx.bot.block_interact(ref_pos);
                    crate::mc::sleep(200).await;
                    return;
                }
            }
        }
    }
}

fn obj(props: Value, required: &[&str]) -> Value {
    let required: Vec<Value> = required.iter().map(|s| json!(s)).collect();
    json!({
        "type": "object",
        "properties": props,
        "required": required,
    })
}

// ---------- tools ----------

pub struct MatchItemNames;
#[async_trait]
impl Skill for MatchItemNames {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "match_item_names".into(),
            description: "Search item names by regex (case-insensitive) — Minecraft item ids are often unintuitive. Returns matching names.".into(),
            input_schema: obj(json!({ "pattern": { "type": "string" }, "limit": { "type": "integer" } }), &["pattern", "limit"]),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let pattern = input["pattern"].as_str().unwrap_or("");
        let limit = input["limit"].as_u64().unwrap_or(0) as usize;
        let re = match regex::RegexBuilder::new(pattern).case_insensitive(true).build() {
            Ok(re) => re,
            Err(_) => return format!("invalid regex: {pattern}"),
        };
        let names: Vec<String> = ctx
            .mc_data
            .item_names()
            .into_iter()
            .filter(|n| re.is_match(n))
            .take(limit)
            .collect();
        if names.is_empty() {
            "no item matches".into()
        } else {
            names.join(", ")
        }
    }
}

pub struct GetRecipe;
#[async_trait]
impl Skill for GetRecipe {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "get_recipe".into(),
            description: "Show the crafting recipe for an item: ingredients with counts, output count, and whether a crafting table is required. Interchangeable material families appear as a wildcard (e.g. *_planks).".into(),
            input_schema: obj(json!({ "item": { "type": "string" } }), &["item"]),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let item = input["item"].as_str().unwrap_or("");
        if ctx.mc_data.item_id(item).is_none() {
            return format!("unknown item \"{item}\"");
        }
        let recipes = recipes::recipes_all(item);
        if recipes.is_empty() {
            return format!("no recipe for {item}");
        }
        let c = recipes::consolidate(recipes);
        let ings = if c.ingredients.is_empty() {
            "unknown ingredients".to_string()
        } else {
            c.ingredients.join(", ")
        };
        format!(
            "{item} x{} from {ings} (crafting table {})",
            c.out,
            if c.requires_table { "required" } else { "not needed" }
        )
    }
}

pub struct GetBlockInfo;
#[async_trait]
impl Skill for GetBlockInfo {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "get_block_info".into(),
            description: "Report a block's hardness, the tool tier needed to get drops, and whether the held item can harvest it.".into(),
            input_schema: obj(json!({ "block": { "type": "string" } }), &["block"]),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let block = input["block"].as_str().unwrap_or("");
        let (hardness, tools) = match ctx.mc_data.block_harvest(block) {
            Some(h) => h,
            None => return format!("unknown block \"{block}\""),
        };
        let tool_names = if tools.is_empty() { "any".to_string() } else { tools.join("/") };
        let held = ctx.bot.get_held_item();
        let held_name = if held.is_present() {
            ctx.mc_data.item_name(held.kind().to_u32())
        } else {
            None
        };
        let can_harvest = if tools.is_empty() {
            true
        } else {
            held_name.as_ref().map(|n| tools.contains(n)).unwrap_or(false)
        };
        let hardness_s = if hardness < 0.0 { "unbreakable".to_string() } else { format!("{hardness}") };
        format!(
            "{block}: hardness {hardness_s}, harvest with {tool_names}; held item {} {} harvest it for drops",
            held_name.as_deref().unwrap_or("none"),
            if can_harvest { "can" } else { "cannot" }
        )
    }
}

pub struct InventoryGap;
#[async_trait]
impl Skill for InventoryGap {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "inventory_gap".into(),
            description: "Recursively expand a target item's recipe tree, subtract current inventory, and list the base materials still missing.".into(),
            input_schema: obj(json!({ "item": { "type": "string" }, "count": { "type": "integer" } }), &["item", "count"]),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let item = input["item"].as_str().unwrap_or("");
        let count = input["count"].as_i64().unwrap_or(0).max(0) as u32;
        if ctx.mc_data.item_id(item).is_none() {
            return format!("unknown item \"{item}\"");
        }
        let inventory = inventory_by_name(ctx);
        let missing: Vec<String> = recipes::missing_base_materials(item, count, &inventory)
            .into_iter()
            .map(|(name, c)| format!("{name} x{c}"))
            .collect();
        if missing.is_empty() {
            "all base materials already on hand".into()
        } else {
            format!("missing base materials: {}", missing.join(", "))
        }
    }
}

pub struct Smelt;
#[async_trait]
impl Skill for Smelt {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "smelt".into(),
            description: "Smelt items in the nearest furnace (within 6 blocks) using the given fuel. If no furnace is near, asks the planner to place one via craft_station.".into(),
            input_schema: obj(json!({ "input": { "type": "string" }, "fuel": { "type": "string" }, "count": { "type": "integer" } }), &["input", "fuel", "count"]),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let in_name = input["input"].as_str().unwrap_or("");
        let fuel_name = input["fuel"].as_str().unwrap_or("");
        let in_id = match ctx.mc_data.item_id(in_name) {
            Some(id) => id,
            None => return format!("unknown input item \"{in_name}\""),
        };
        let fuel_id = match ctx.mc_data.item_id(fuel_name) {
            Some(id) => id,
            None => return format!("unknown fuel item \"{fuel_name}\""),
        };
        let furnace_id = match ctx.mc_data.block_id("furnace") {
            Some(id) => id,
            None => return "no furnace within 6 blocks — use craft_station to place a furnace first".into(),
        };
        let block = match find_block_within(ctx, furnace_id, 6.0) {
            Some(b) => b,
            None => return "no furnace within 6 blocks — use craft_station to place a furnace first".into(),
        };
        // Open the furnace and load fuel + input. Furnace menu slots: 0=input, 1=fuel, 2=output.
        // TODO(verify): shift-click transfers whole stacks, not exact counts (no putFuel/putInput in azalea).
        let handle = match ctx.bot.open_container_at(block).await {
            Some(h) => h,
            None => return "error: could not open furnace".into(),
        };
        let count = (input["count"].as_i64().unwrap_or(1) as i32).clamp(1, 64);

        // Move fuel then input from player inventory into the furnace.
        if let Some(i) = find_container_slot(&handle, fuel_id, 3) {
            handle.shift_click(i);
        }
        crate::mc::sleep(300).await;
        if let Some(i) = find_container_slot(&handle, in_id, 3) {
            handle.shift_click(i);
        }

        // Wait until the output has accumulated `count` (bounded by a generous per-item deadline).
        let deadline_ms = (count as u64) * 12_000;
        let mut waited = 0u64;
        while waited < deadline_ms {
            crate::mc::sleep(1000).await;
            waited += 1000;
            let out = handle.slots().and_then(|s| s.get(2).cloned());
            if let Some(out) = out {
                if out.is_present() && out.count() >= count {
                    break;
                }
            }
        }
        // Take output.
        let out_count = handle
            .slots()
            .and_then(|s| s.get(2).cloned())
            .map(|s| s.count())
            .unwrap_or(0);
        if out_count > 0 {
            handle.shift_click(2usize);
            crate::mc::sleep(200).await;
        }
        handle.close();
        if out_count > 0 {
            format!("smelted {out_count} {in_name}")
        } else {
            "nothing smelted (timed out or no fuel)".into()
        }
    }
}

/// First player-area slot (index >= `from`) holding `item_id` in an open container.
fn find_container_slot(handle: &azalea::container::ContainerHandle, item_id: u32, from: usize) -> Option<usize> {
    let slots = handle.slots()?;
    slots
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, s)| s.is_present() && s.kind().to_u32() == item_id)
        .map(|(i, _)| i)
}

pub struct CraftStation;
#[async_trait]
impl Skill for CraftStation {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "craft_station".into(),
            description: "Ensure a crafting_table, furnace, or blast_furnace is placed within reach. Returns its offset from you (+x east, +y up, +z south) or a clear error.".into(),
            input_schema: obj(json!({ "station": { "type": "string", "enum": ["crafting_table", "furnace", "blast_furnace"] } }), &["station"]),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let station = input["station"].as_str().unwrap_or("");
        let block_id = ctx.mc_data.block_id(station);
        let item_id = ctx.mc_data.item_id(station);
        let (block_id, item_id) = match (block_id, item_id) {
            (Some(b), Some(i)) => (b, i),
            _ => return format!("unknown station \"{station}\""),
        };
        let here = ctx.bot.position();
        if let Some(existing) = find_block_within(ctx, block_id, 4.0) {
            return format!("{station} already at {}", rel(to_pos(here), bp_pos(existing)));
        }
        // Ensure we hold the station item, crafting it if needed.
        if !have_item(ctx, item_id) {
            let msg = craft(ctx, station, 1).await;
            if !have_item(ctx, item_id) {
                return format!("error: {msg}");
            }
        }
        // Place on top of a solid block in an empty adjacent cell.
        let base = floor_pos(here);
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let target = BlockPos::new(base.x + dx, base.y, base.z + dz);
            let below = BlockPos::new(target.x, target.y - 1, target.z);
            let t = block_name_at(ctx, target);
            let b = block_name_at(ctx, below);
            let t_air = t.as_deref() == Some("air");
            let b_solid = b.as_ref().map(|n| n != "air" && !n.contains("lava")).unwrap_or(false);
            if t_air && b_solid {
                equip_to_hand(ctx, item_id);
                ctx.bot.look_at(target.center()); // TODO(verify) place against top of `below`
                ctx.bot.block_interact(below);
                crate::mc::sleep(300).await;
                return format!("placed {station} at {}", rel(to_pos(here), bp_pos(target)));
            }
        }
        format!("error: no valid spot to place {station} nearby")
    }
}

/// One recipe cell: the crafting-menu grid slot and the concrete ingredient it wants.
struct Cell {
    slot: usize,
    name: String,
}

/// Lay a recipe onto a `grid_w`×`grid_w` menu grid (slots 1-based, row-major). None if it doesn't fit.
fn plan_cells(r: &recipes::Recipe, grid_w: usize) -> Option<Vec<Cell>> {
    let mut cells = Vec::new();
    if let Some(shape) = &r.in_shape {
        if shape.len() > grid_w {
            return None;
        }
        for (ri, row) in shape.iter().enumerate() {
            if row.len() > grid_w {
                return None;
            }
            for (ci, cell) in row.iter().enumerate() {
                if let Some(n) = cell {
                    cells.push(Cell { slot: 1 + ri * grid_w + ci, name: n.clone() });
                }
            }
        }
    } else if let Some(ings) = &r.ingredients {
        if ings.len() > grid_w * grid_w {
            return None;
        }
        for (i, n) in ings.iter().enumerate() {
            cells.push(Cell { slot: 1 + i, name: n.clone() });
        }
    }
    Some(cells)
}

/// Player-inventory slot holding `concrete` (exact item preferred, else same material family).
fn find_source(handle: &azalea::container::ContainerHandle, concrete: &str, ctx: &SkillContext) -> Option<usize> {
    let menu = handle.menu()?;
    let slots = menu.slots();
    let family = recipes::item_family(concrete);
    let mut fam_hit = None;
    for i in menu.player_slots_range() {
        let Some(s) = slots.get(i) else { continue };
        if !s.is_present() {
            continue;
        }
        let Some(nm) = ctx.mc_data.item_name(s.kind().to_u32()) else { continue };
        if nm == concrete {
            return Some(i);
        }
        if fam_hit.is_none() && recipes::item_family(&nm) == family {
            fam_hit = Some(i);
        }
    }
    fam_hit
}

fn slot_count(handle: &azalea::container::ContainerHandle, i: usize) -> i32 {
    handle.slots().and_then(|s| s.get(i).map(|x| x.count())).unwrap_or(0)
}

fn count_item(ctx: &SkillContext, item_id: u32) -> i32 {
    inventory_items(ctx).iter().filter(|(id, _)| *id == item_id).map(|(_, c)| *c).sum()
}

/// Wait (up to ~1s) for the server to populate the crafting result slot.
async fn wait_result(handle: &azalea::container::ContainerHandle, result_slot: usize) -> bool {
    for _ in 0..10 {
        crate::mc::sleep(100).await;
        if let Some(s) = handle.slots().and_then(|s| s.get(result_slot).cloned()) {
            if s.is_present() {
                return true;
            }
        }
    }
    false
}

/// Deposit `per_cell` of each ingredient into its grid cell via cursor pick-up + right-click.
/// False if an ingredient ran out mid-lay. Leaves the cursor empty.
async fn place_cells(
    handle: &azalea::container::ContainerHandle,
    cells: &[Cell],
    per_cell: i32,
    ctx: &SkillContext,
) -> bool {
    let mut held: Option<(usize, i32)> = None; // (source slot, remaining on cursor)
    let mut held_family = String::new();
    let mut ok = true;
    'outer: for cell in cells {
        let fam = recipes::item_family(&cell.name).to_string();
        for _ in 0..per_cell {
            let reuse = matches!(&held, Some((_, rem)) if *rem > 0) && held_family == fam;
            if !reuse {
                if let Some((src, rem)) = held.take() {
                    if rem > 0 {
                        handle.left_click(src);
                        crate::mc::sleep(50).await;
                    }
                }
                let Some(src) = find_source(handle, &cell.name, ctx) else {
                    ok = false;
                    break 'outer;
                };
                let cnt = slot_count(handle, src);
                handle.left_click(src);
                crate::mc::sleep(50).await;
                held = Some((src, cnt));
                held_family = fam.clone();
            }
            handle.right_click(cell.slot);
            crate::mc::sleep(50).await;
            if let Some((_, rem)) = held.as_mut() {
                *rem -= 1;
            }
        }
    }
    if let Some((src, rem)) = held.take() {
        if rem > 0 {
            handle.left_click(src);
            crate::mc::sleep(50).await;
        }
    }
    ok
}

/// Craft `count` of `name`, driving the 3x3 table (if required/near) or the 2x2 player grid.
/// Type/family-agnostic on ingredients; returns a planner-facing sentence.
pub async fn craft(ctx: &SkillContext, name: &str, count: i32) -> String {
    let item_id = match ctx.mc_data.item_id(name) {
        Some(id) => id,
        None => return format!("unknown item \"{name}\""),
    };
    if recipes::recipes_all(name).is_empty() {
        return format!("no recipe for {name}");
    }
    let want = count.max(1);
    let table = ctx.mc_data.block_id("crafting_table").and_then(|id| find_block_within(ctx, id, 4.0));
    let inv = inventory_by_name(ctx);
    let recipe = match recipes::choose_recipe(name, &inv, table.is_some()) {
        Some(r) => r,
        None => {
            return if recipes::choose_recipe(name, &inv, true).is_some() {
                format!("need a crafting table nearby to craft {name} — use craft_station first")
            } else {
                format!("cannot craft {name} (missing materials)")
            };
        }
    };
    let grid_w = if recipe.requires_table { 3 } else { 2 };
    let cells = match plan_cells(recipe, grid_w) {
        Some(c) if !c.is_empty() => c,
        _ => return format!("cannot lay out recipe for {name}"),
    };
    let out_per = (recipe.result.count.max(1)) as i32;
    let crafts = ((want + out_per - 1) / out_per).min(64);

    let before = count_item(ctx, item_id);
    let handle = if recipe.requires_table {
        match ctx.bot.open_container_at(table.unwrap()).await {
            Some(h) => h,
            None => return "error: could not open crafting table".into(),
        }
    } else {
        match ctx.bot.open_inventory() {
            Some(h) => h,
            None => return "error: could not open inventory (a container is already open)".into(),
        }
    };

    place_cells(&handle, &cells, crafts, ctx).await;
    // One shift-click per craft: the server refills the grid + result until ingredients run dry.
    for _ in 0..crafts {
        if !wait_result(&handle, 0).await {
            break;
        }
        handle.shift_click(0usize);
        crate::mc::sleep(120).await;
    }
    // Return any unconsumed ingredients from the grid, then close.
    for cell in &cells {
        handle.shift_click(cell.slot);
        crate::mc::sleep(40).await;
    }
    handle.close();
    crate::mc::sleep(150).await;

    let delta = (count_item(ctx, item_id) - before).max(0);
    if delta > 0 {
        format!("crafted {delta} {name}")
    } else {
        format!("could not craft {name} (missing materials or no result)")
    }
}

pub struct DigStaircase;
#[async_trait]
impl Skill for DigStaircase {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "dig_staircase".into(),
            description: "Dig a descending 2-high staircase down to a target Y (bounded), placing torches as it goes and stopping at lava.".into(),
            input_schema: obj(json!({ "target_y": { "type": "integer" } }), &["target_y"]),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let target_y = input["target_y"].as_i64().unwrap_or(0) as i32;
        let (fx, fz) = forward_step(ctx);
        let start_y = ctx.bot.position().y.floor() as i32;
        let mut i = 0;
        while i < 80 && (ctx.bot.position().y.floor() as i32) > target_y {
            let feet = floor_pos(ctx.bot.position());
            let step = BlockPos::new(feet.x + fx, feet.y - 1, feet.z + fz);
            for up in 0..3 {
                let p = BlockPos::new(step.x, step.y + up, step.z);
                if let Err(e) = dig_safe(ctx, p).await {
                    return format!("error: {e} at y={}", ctx.bot.position().y.floor() as i32);
                }
            }
            let _ = ctx.bot.goto(BlockPosGoal(step)).await; // catch(() => {})
            if i % 6 == 0 {
                place_torch(ctx).await;
            }
            i += 1;
        }
        let depth = ctx.bot.position().y.floor() as i32;
        format!("staircase reached y={depth} (from y={start_y})")
    }
}

pub struct StripMine;
#[async_trait]
impl Skill for StripMine {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "strip_mine".into(),
            description: "Dig a 1-wide, 2-high tunnel in a cardinal direction for N blocks (max 64), placing torches and stopping at lava.".into(),
            input_schema: obj(json!({ "direction": { "type": "string", "enum": ["north", "south", "east", "west"] }, "length": { "type": "integer" } }), &["direction", "length"]),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let direction = input["direction"].as_str().unwrap_or("");
        let (dx, dz) = match CARDINAL.iter().find(|(d, _)| *d == direction) {
            Some((_, s)) => *s,
            None => return format!("unknown direction \"{direction}\""),
        };
        let length = (input["length"].as_i64().unwrap_or(0) as i32).clamp(0, 64);
        let mut mined = 0;
        for i in 1..=length {
            let feet = floor_pos(ctx.bot.position());
            let ahead = BlockPos::new(feet.x + dx, feet.y, feet.z + dz);
            for up in 0..2 {
                let p = BlockPos::new(ahead.x, ahead.y + up, ahead.z);
                match dig_safe(ctx, p).await {
                    Ok(true) => mined += 1,
                    Ok(false) => {}
                    Err(e) => return format!("error: {e} after removing {mined} blocks"),
                }
            }
            let _ = ctx.bot.goto(BlockPosGoal(ahead)).await;
            if i % 6 == 0 {
                place_torch(ctx).await;
            }
        }
        format!("strip-mined {direction} for {length} blocks, removed {mined} blocks")
    }
}

pub fn skills() -> Vec<Arc<dyn Skill>> {
    vec![
        Arc::new(MatchItemNames),
        Arc::new(GetRecipe),
        Arc::new(GetBlockInfo),
        Arc::new(InventoryGap),
        Arc::new(Smelt),
        Arc::new(CraftStation),
        Arc::new(DigStaircase),
        Arc::new(StripMine),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shaped(rows: &[&[Option<&str>]], requires_table: bool) -> recipes::Recipe {
        recipes::Recipe {
            result: recipes::ItemStack { name: "x".into(), count: 1 },
            ingredients: None,
            in_shape: Some(
                rows.iter().map(|r| r.iter().map(|c| c.map(str::to_string)).collect()).collect(),
            ),
            requires_table,
        }
    }

    fn cells_of(r: &recipes::Recipe, grid_w: usize) -> Vec<(usize, String)> {
        plan_cells(r, grid_w).unwrap().into_iter().map(|c| (c.slot, c.name)).collect()
    }

    #[test]
    fn shapeless_fills_leading_grid_slots() {
        let r = recipes::Recipe {
            result: recipes::ItemStack { name: "oak_planks".into(), count: 4 },
            ingredients: Some(vec!["oak_log".into()]),
            in_shape: None,
            requires_table: false,
        };
        assert_eq!(cells_of(&r, 2), vec![(1, "oak_log".to_string())]);
    }

    #[test]
    fn shaped_2x2_maps_row_major_from_slot_1() {
        let r = shaped(&[&[Some("oak_planks"), Some("oak_planks")], &[Some("oak_planks"), Some("oak_planks")]], false);
        let slots: Vec<usize> = cells_of(&r, 2).into_iter().map(|(s, _)| s).collect();
        assert_eq!(slots, vec![1, 2, 3, 4]);
    }

    #[test]
    fn shaped_pickaxe_places_gaps_correctly() {
        // planks across the top row, sticks down the middle column; nulls are skipped.
        let r = shaped(
            &[
                &[Some("cherry_planks"), Some("cherry_planks"), Some("cherry_planks")],
                &[None, Some("stick"), None],
                &[None, Some("stick"), None],
            ],
            true,
        );
        assert_eq!(
            cells_of(&r, 3),
            vec![
                (1, "cherry_planks".into()),
                (2, "cherry_planks".into()),
                (3, "cherry_planks".into()),
                (5, "stick".into()),
                (8, "stick".into()),
            ]
        );
    }

    #[test]
    fn rejects_shape_wider_than_grid() {
        let r = shaped(&[&[Some("a"), Some("a"), Some("a")]], false);
        assert!(plan_cells(&r, 2).is_none(), "3-wide shape must not fit a 2x2 grid");
        assert!(plan_cells(&r, 3).is_some());
    }
}
