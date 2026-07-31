//! Base tools (port of skills.ts): observe snapshot, the big `execute` match, tool schemas,
//! result summarizer, and the auto-behavior tick loop. Non-base tools dispatch to all_skills().

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use azalea::bot::BotClientExt;
use azalea::container::ContainerClientExt;
use azalea::entity::inventory::Inventory;
use azalea::entity::metadata::Health;
use azalea::entity::{EntityKindComponent, EntityUuid, LocalEntity, Position};
use azalea::local_player::Hunger;
use azalea::pathfinder::goals::{RadiusGoal, XZGoal};
use azalea::pathfinder::PathfinderClientExt;
use azalea::registry::builtin::EntityKind;
use azalea::{BlockPos, Client, Vec3};
use futures::future::BoxFuture;
use serde_json::{json, Map, Value};

use crate::llm::ToolDef;
use crate::mc::{
    self, block_kind_from_name, equip_best_tool, find_blocks_near, item_kind_from_name,
    name_of_block_kind, name_of_item_kind, nearest_block, nearest_hostile, with_timeout,
};
use crate::routines::{referenced_tools, run_steps, Budget, RunCtx};
use crate::rules::RuleEngine;
use crate::skill::{rel, BotView, Exec, Routine, SHARED_SCOPE};
use crate::skills::{all_behaviors, all_skills, SkillContext};
use crate::types::Pos;

const COMPASS8: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];

tokio::task_local! {
    static ROUTINE_DEPTH: u32;
}
fn current_depth() -> u32 {
    ROUTINE_DEPTH.try_with(|d| *d).unwrap_or(0)
}

// --- small accessors ---

fn self_pos(bot: &Client) -> Option<Vec3> {
    bot.get_component::<Position>().map(|p| *p)
}
fn self_health(bot: &Client) -> Option<f32> {
    bot.get_component::<Health>().map(|h| *h)
}
fn self_food(bot: &Client) -> Option<u32> {
    bot.get_component::<Hunger>().map(|h| h.food)
}
fn to_pos_v(v: Vec3) -> Pos {
    Pos { x: v.x, y: v.y, z: v.z }
}
fn to_pos_b(b: BlockPos) -> Pos {
    Pos { x: b.x as f64, y: b.y as f64, z: b.z as f64 }
}
fn gi(input: &Value, k: &str) -> i64 {
    input.get(k).and_then(Value::as_i64).unwrap_or(0)
}
fn gf(input: &Value, k: &str) -> f64 {
    input.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}
fn gs<'a>(input: &'a Value, k: &str) -> &'a str {
    input.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Per-stack inventory listing ("namexcount"), matching mineflayer `inventory.items()`.
fn inv_stacks(bot: &Client) -> Vec<(String, i32)> {
    let Some(inv) = bot.get_component::<Inventory>() else {
        return vec![];
    };
    let menu = &inv.inventory_menu;
    let slots = menu.slots();
    let mut out = Vec::new();
    for i in menu.player_slots_range() {
        if let Some(s) = slots.get(i) {
            if s.is_present() {
                out.push((name_of_item_kind(s.kind()), s.count()));
            }
        }
    }
    out
}

fn held_name(bot: &Client) -> String {
    match bot.get_component::<Inventory>() {
        Some(inv) => {
            let held = inv.held_item();
            if held.is_present() {
                name_of_item_kind(held.kind())
            } else {
                "nothing".to_string()
            }
        }
        None => "nothing".to_string(),
    }
}

/// 8-point compass bearing + horizontal distance (MC axes: north=-z, east=+x).
fn bearing(from: Vec3, to: Vec3) -> String {
    let (dx, dy, dz) = (to.x - from.x, to.y - from.y, to.z - from.z);
    let idx = (((dx.atan2(-dz) / (std::f64::consts::PI / 4.0)).round() as i64 % 8) + 8) % 8;
    let vert = if dy.abs() >= 4.0 {
        if dy > 0.0 {
            " above"
        } else {
            " below"
        }
    } else {
        ""
    };
    format!("{} {}m{}", COMPASS8[idx as usize], dx.hypot(dz).round() as i64, vert)
}

/// Nearby non-local entities (nearest-first) with kind + position, up to `max` meters.
fn nearby_entities(bot: &Client, max: f64) -> Vec<(EntityKind, Vec3)> {
    let Some(here) = self_pos(bot) else {
        return vec![];
    };
    let mut out = Vec::new();
    for e in bot.nearest_entities_by::<(), azalea::ecs::query::Without<LocalEntity>>(|_: ()| true) {
        let (Some(k), Some(p)) =
            (bot.get_entity_component::<EntityKindComponent>(e), bot.get_entity_component::<Position>(e))
        else {
            continue;
        };
        let pv: Vec3 = *p;
        if here.distance_to(pv) > max {
            break; // sorted nearest-first
        }
        out.push((*k, pv));
    }
    out
}

/// Walk onto nearby item drops so Minecraft auto-picks them up; best-effort, returns count reached.
async fn collect_nearby_drops(bot: &Client, radius: f64) -> usize {
    let Some(origin) = self_pos(bot) else {
        return 0;
    };
    let drops: Vec<Vec3> = bot
        .nearest_entities_by::<&EntityKindComponent, ()>(|k: &EntityKindComponent| k.0 == EntityKind::Item)
        .into_iter()
        .map(|e| *bot.entity_component::<Position>(e))
        .filter(|p| origin.distance_to(*p) <= radius)
        .take(16)
        .collect();
    let mut n = 0;
    for p in drops {
        if with_timeout(bot.goto(RadiusGoal::new(p, 1.0)), 8_000, "collect_block pickup").await.is_ok() {
            n += 1;
        }
    }
    n
}

// --- observe ---

/// Compact perception snapshot fed to the planner every step.
pub fn observe(ctx: &SkillContext) -> String {
    let bot = &ctx.bot;
    let p = self_pos(bot);
    let pos = match p {
        Some(v) => format!("({:.0}, {:.0}, {:.0})", v.x, v.y, v.z),
        None => "unknown".to_string(),
    };
    let inv = inv_stacks(bot);
    let inv = if inv.is_empty() {
        "empty".to_string()
    } else {
        inv.iter().map(|(n, c)| format!("{n}x{c}")).collect::<Vec<_>>().join(", ")
    };

    let players = players_nearby(bot, p);
    let mobs = match p {
        Some(_) => {
            let names: Vec<String> = nearby_entities(bot, 16.0)
                .into_iter()
                .filter(|(k, _)| *k != EntityKind::Player)
                .take(8)
                .map(|(k, _)| name_of_entity_kind(k))
                .collect();
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            }
        }
        None => "none".to_string(),
    };

    let health = self_health(bot).map(|h| format!("{h}")).unwrap_or_else(|| "?".to_string());
    let food = self_food(bot).map(|f| format!("{f}")).unwrap_or_else(|| "?".to_string());
    // Lean snapshot: always the vitals + inventory; players/mobs lines only when non-empty
    // (no constant "none" noise). See LEAN_PLANNER.md for the surrounding thread contract.
    let mut lines = vec![
        format!("position={pos} health={health} food={food} held={}", held_name(bot)),
        format!("inventory: {inv}"),
    ];
    if players != "none" {
        lines.push(format!("players nearby: {players}"));
    }
    if mobs != "none" {
        lines.push(format!("mobs within 16m: {mobs}"));
    }
    lines.join("\n")
}

fn players_nearby(bot: &Client, p: Option<Vec3>) -> String {
    let me = bot.username();
    let list: Vec<String> = bot
        .tab_list()
        .values()
        .filter(|pi| pi.profile.name != me)
        .map(|pi| {
            let name = pi.profile.name.clone();
            let ent = bot.entity_by_uuid(pi.profile.uuid).and_then(|e| bot.get_entity_component::<Position>(e));
            match (ent, p) {
                (Some(ep), Some(here)) => format!("{name} ({})", bearing(here, *ep)),
                _ => name,
            }
        })
        .collect();
    if list.is_empty() {
        "none".to_string()
    } else {
        list.join(", ")
    }
}

fn name_of_entity_kind(k: EntityKind) -> String {
    let s = k.to_string();
    s.strip_prefix("minecraft:").unwrap_or(&s).to_string()
}

// --- BotView over a live bot (for run_routine / rules condition eval) ---

struct BaseBotView {
    bot: Client,
}
impl BotView for BaseBotView {
    fn inv_count(&self, item: &str) -> i64 {
        inv_stacks(&self.bot).iter().filter(|(n, _)| n == item).map(|(_, c)| *c as i64).sum()
    }
    fn nearby_count(&self, block: &str) -> i64 {
        match block_kind_from_name(block) {
            Some(k) => find_blocks_near(&self.bot, k, 32, 64).len() as i64,
            None => 0,
        }
    }
    fn health(&self) -> f32 {
        self_health(&self.bot).unwrap_or(0.0)
    }
    fn food(&self) -> f32 {
        self_food(&self.bot).unwrap_or(0) as f32
    }
}

fn view_of(ctx: &SkillContext) -> Arc<dyn BotView + Send + Sync> {
    Arc::new(BaseBotView { bot: ctx.bot.clone() })
}

/// Build an Exec that re-enters `execute` at the given routine depth.
fn make_exec(ctx: SkillContext, depth: u32) -> Exec {
    Arc::new(move |tool: String, args: Value| {
        let ctx = ctx.clone();
        Box::pin(ROUTINE_DEPTH.scope(depth, async move { execute(&ctx, &tool, args).await }))
            as BoxFuture<'static, String>
    })
}

// --- execute ---

/// Executes one skill, returning a short natural-language result. A caught error becomes
/// `error: <msg>`; unknown names dispatch to the registered skill modules.
pub async fn execute(ctx: &SkillContext, name: &str, input: Value) -> String {
    match run(ctx, name, &input).await {
        Ok(s) => s,
        Err(e) => format!("error: {e}"),
    }
}

async fn run(ctx: &SkillContext, name: &str, input: &Value) -> anyhow::Result<String> {
    let bot = &ctx.bot;
    let mc = &ctx.mc_data;

    let out = match name {
        "list_inventory" => {
            let items = inv_stacks(bot);
            if items.is_empty() {
                "inventory empty".to_string()
            } else {
                items.iter().map(|(n, c)| format!("{n}x{c}")).collect::<Vec<_>>().join(", ")
            }
        }

        "find_blocks" => {
            let bname = gs(input, "name");
            let Some(kind) = block_kind_from_name(bname) else {
                return Ok(format!("unknown block \"{bname}\""));
            };
            let max_d = gi(input, "max_distance") as i32;
            let count = gi(input, "count").max(1) as usize;
            let found = find_blocks_near(bot, kind, max_d, count);
            if found.is_empty() {
                return Ok(format!("no {bname} within {max_d}m"));
            }
            let from = to_pos_v(self_pos(bot).unwrap_or(Vec3::default()));
            found.iter().map(|b| rel(from, to_pos_b(*b))).collect::<Vec<_>>().join("; ")
        }

        "get_position" => match self_pos(bot) {
            Some(v) => format!("({}, {}, {})", v.x.round(), v.y.round(), v.z.round()),
            None => "position unknown".to_string(),
        },

        "whoami" => {
            let owner = ctx.self_.owner.as_deref().unwrap_or("nobody (you are unowned)");
            format!("you are {}, owner: {owner}", ctx.self_.username)
        }

        "go_to" => {
            let (x, y, z) = (gi(input, "x"), gi(input, "y"), gi(input, "z"));
            let goal = RadiusGoal::new(Vec3::new(x as f64, y as f64, z as f64), 1.0);
            with_timeout(bot.goto(goal), 60_000, "go_to").await?;
            format!("arrived near ({x}, {y}, {z})")
        }

        "go_to_player" => {
            let user = gs(input, "username");
            let Some(pos) = player_pos(bot, user) else {
                return Ok(format!("player \"{user}\" not visible"));
            };
            let goal = RadiusGoal::new(pos, 2.0);
            with_timeout(bot.goto(goal), 60_000, "go_to_player").await?;
            format!("reached {user}")
        }

        "go_toward" => {
            let target = gs(input, "target");
            let dist = (gi(input, "distance").max(1)).min(64) as f64;
            let start = self_pos(bot).unwrap_or(Vec3::default());
            let (tx, tz) = if let Some(dir) = cardinal(target) {
                ((start.x + dir.0 * dist).round() as i32, (start.z + dir.1 * dist).round() as i32)
            } else {
                let Some(kind) = block_kind_from_name(target) else {
                    return Ok(format!(
                        "\"{target}\" is neither a direction (north/south/east/west) nor a known block"
                    ));
                };
                let Some(near) = nearest_block(bot, kind, 128) else {
                    return Ok(format!("no {target} within 128m to head toward"));
                };
                let (dx, dz) = (near.x as f64 - start.x, near.z as f64 - start.z);
                let horiz = dx.hypot(dz).max(1.0);
                let reach = dist.min(horiz.round());
                ((start.x + dx / horiz * reach).round() as i32, (start.z + dz / horiz * reach).round() as i32)
            };
            let _ = with_timeout(bot.goto(XZGoal { x: tx, z: tz }), 60_000, "go_toward").await;
            let end = self_pos(bot).unwrap_or(start);
            format!(
                "moved {}m toward {target}, now at ({:.0}, {:.0}, {:.0})",
                start.distance_to(end).round() as i64,
                end.x,
                end.y,
                end.z
            )
        }

        "collect_block" => {
            let bname = gs(input, "name");
            let Some(kind) = block_kind_from_name(bname) else {
                return Ok(format!("unknown block \"{bname}\""));
            };
            let count = gi(input, "count").max(1) as usize;
            let targets = find_blocks_near(bot, kind, 32, count);
            if targets.is_empty() {
                return Ok(format!("no {bname} within 32m to collect"));
            }
            let mut mined = 0usize;
            let mut picked = 0usize;
            for at in &targets {
                let center = at.center();
                let _ = with_timeout(bot.goto(RadiusGoal::new(center, 2.0)), 20_000, "approach").await;
                if self_pos(bot).map(|p| p.distance_to(center)).unwrap_or(99.0) > 5.0 {
                    continue; // couldn't get in reach
                }
                let state = bot.world().read().get_block_state(*at).unwrap_or_default();
                equip_best_tool(bot, state, false).await;
                if with_timeout(bot.mine(*at), 30_000, "collect_block").await.is_ok() {
                    mined += 1;
                    // Pick up per block: let the drop entity spawn, then walk onto nearby drops.
                    // Doing it here (not just at the end) means spread-out targets don't strand drops.
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    picked += collect_nearby_drops(bot, 5.0).await;
                }
            }
            if mined == 0 {
                return Ok(format!(
                    "can't path to the nearest {bname} (blocked or too far). Relocate first with go_toward \"{bname}\", then collect again."
                ));
            }
            picked += collect_nearby_drops(bot, 6.0).await; // final sweep for stragglers
            if picked > 0 {
                format!("collected up to {mined} {bname} ({picked} drop(s) gathered)")
            } else {
                format!("collected up to {mined} {bname}")
            }
        }

        "mine_block" => {
            let (x, y, z) = (gi(input, "x") as i32, gi(input, "y") as i32, gi(input, "z") as i32);
            let at = BlockPos::new(x, y, z);
            let mut state = bot.world().read().get_block_state(at).unwrap_or_default();
            if state.is_air() {
                return Ok("no block at that coordinate".to_string());
            }
            let in_reach = |b: &Client| self_pos(b).map(|p| p.distance_to(at.center())).unwrap_or(99.0) <= 4.5;
            if !in_reach(bot) {
                let _ = with_timeout(bot.goto(RadiusGoal::new(at.center(), 2.0)), 60_000, "approach").await;
                state = bot.world().read().get_block_state(at).unwrap_or_default();
                if state.is_air() {
                    return Ok("no block at that coordinate".to_string());
                }
            }
            if !in_reach(bot) {
                return Ok(format!(
                    "can't reach the block at ({x}, {y}, {z}) — path blocked; clear a way or mine from an adjacent spot"
                ));
            }
            let bn = name_of_block_kind(state.into());
            if !equip_best_tool(bot, state, true).await {
                return Ok(format!(
                    "can't harvest {bn} — no tool you carry would drop it; craft/equip a stronger tool (see get_block_info)"
                ));
            }
            bot.look_at(at.center());
            with_timeout(bot.mine(at), 60_000, "mine_block").await?;
            format!("mined {bn}")
        }

        "place_block" => {
            let bname = gs(input, "name");
            let (x, y, z) = (gi(input, "x") as i32, gi(input, "y") as i32, gi(input, "z") as i32);
            if !carrying(bot, bname) {
                return Ok(format!("not carrying {bname}"));
            }
            let at = BlockPos::new(x, y, z);
            if self_pos(bot).map(|p| p.distance_to(at.center())).unwrap_or(99.0) > 4.0 {
                let _ = with_timeout(bot.goto(RadiusGoal::new(at.center(), 2.0)), 60_000, "approach").await;
            }
            let below = BlockPos::new(x, y - 1, z);
            let below_state = bot.world().read().get_block_state(below).unwrap_or_default();
            if below_state.is_air() {
                return Ok("no solid block to place against below the target".to_string());
            }
            if self_pos(bot).map(|p| p.distance_to(at.center())).unwrap_or(99.0) > 5.0 {
                return Ok(format!("can't reach ({x}, {y}, {z}) to place — path blocked; move closer"));
            }
            // Select the block, face the target, place against the top face of the block below.
            // TODO(verify): azalea placement is face-inferred from look; mineflayer passed an explicit face.
            select_to_hand(bot, bname);
            bot.look_at(at.center());
            bot.block_interact(below);
            format!("placed {bname}")
        }

        "craft_item" => {
            // TODO(verify): azalea 0.15 exposes no crafting/recipe API; the iron module owns real
            // crafting. Base craft is a stub so unknown-item errors still match.
            let iname = gs(input, "name");
            if item_kind_from_name(iname).is_none() {
                return Ok(format!("unknown item \"{iname}\""));
            }
            format!("cannot craft {iname} (crafting not yet implemented in the azalea port)")
        }

        "equip_item" => {
            let iname = gs(input, "name");
            if !carrying(bot, iname) {
                return Ok(format!("not carrying {iname}"));
            }
            // Only main-hand is modeled via hotbar select; armor/off-hand need slot clicks.
            // TODO(verify): non-hand destinations are best-effort.
            select_to_hand(bot, iname);
            format!("equipped {iname}")
        }

        "attack_nearest" => {
            let Some(target) = nearest_hostile(bot, None) else {
                return Ok("no hostile mob nearby".to_string());
            };
            let kname = bot
                .get_entity_component::<EntityKindComponent>(target)
                .map(|k| name_of_entity_kind(*k))
                .unwrap_or_else(|| "mob".to_string());
            if let Some(p) = bot.get_entity_component::<Position>(target) {
                let _ = with_timeout(bot.goto(RadiusGoal::new(*p, 2.0)), 20_000, "approach").await;
            }
            bot.attack(target);
            format!("attacked {kname}")
        }

        "attack_player" => {
            let user = gs(input, "username");
            let Some(target) = player_entity(bot, user) else {
                return Ok(format!("player \"{user}\" not visible"));
            };
            if let Some(p) = bot.get_entity_component::<Position>(target) {
                let _ = with_timeout(bot.goto(RadiusGoal::new(*p, 2.0)), 20_000, "approach").await;
            }
            bot.attack(target);
            format!("attacked {user}")
        }

        "fight" => {
            let target = gs(input, "target");
            let (entity, label) = if target == "nearest" {
                match nearest_hostile(bot, None) {
                    Some(e) => (e, entity_label(bot, e)),
                    None => return Ok(format!("no target \"{target}\" nearby")),
                }
            } else if let Some(e) = player_entity(bot, target) {
                (e, target.to_string())
            } else if let Some(e) = entity_by_kind_name(bot, target) {
                (e, target.to_string())
            } else {
                return Ok(format!("no target \"{target}\" nearby"));
            };
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline {
                let Some(p) = bot.get_entity_component::<Position>(entity) else {
                    break; // target gone/dead
                };
                let pv: Vec3 = *p;
                if self_pos(bot).map(|s| s.distance_to(pv)).unwrap_or(99.0) > 3.0 {
                    let _ = with_timeout(bot.goto(RadiusGoal::new(pv, 2.0)), 5_000, "approach").await;
                }
                if !bot.has_attack_cooldown() {
                    bot.attack(entity);
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            bot.stop_pathfinding();
            format!("finished fighting {label}")
        }

        "flee" => {
            let Some(threat) = nearest_hostile(bot, None) else {
                return Ok("no hostile mob to flee from".to_string());
            };
            let tpos: Vec3 = match bot.get_entity_component::<Position>(threat) {
                Some(p) => *p,
                None => return Ok("no hostile mob to flee from".to_string()),
            };
            let p = self_pos(bot).unwrap_or(Vec3::default());
            let name = entity_label(bot, threat);
            let away = p - tpos;
            let mag = away.length();
            let dir = if mag > 0.001 { away * (1.0 / mag) } else { Vec3::new(1.0, 0.0, 0.0) };
            let dist = (gf(input, "distance").max(4.0)).min(32.0);
            let dest = p + dir * dist;
            let _ = with_timeout(bot.goto(RadiusGoal::new(dest, 2.0)), 30_000, "flee").await;
            format!("fled from {name}")
        }

        "follow_player" => {
            let user = gs(input, "username");
            if player_entity(bot, user).is_none() {
                return Ok(format!("player \"{user}\" not visible"));
            }
            let secs = (gi(input, "seconds").max(0)).min(300) as u64;
            let deadline = Instant::now() + Duration::from_secs(secs);
            while Instant::now() < deadline {
                let Some(pos) = player_pos(bot, user) else {
                    break;
                };
                if self_pos(bot).map(|p| p.distance_to(pos)).unwrap_or(99.0) > 3.0 {
                    bot.start_goto(RadiusGoal::new(pos, 2.0));
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            bot.stop_pathfinding();
            format!("followed {user}")
        }

        "deposit" | "withdraw" => container_transfer(bot, name, input).await?,

        "match_block_names" => {
            let pat = gs(input, "pattern");
            let limit = gi(input, "limit").max(1) as usize;
            let re = match regex::RegexBuilder::new(pat).case_insensitive(true).build() {
                Ok(re) => re,
                Err(_) => return Ok(format!("invalid regex: {pat}")),
            };
            let names: Vec<String> =
                mc.block_names().into_iter().filter(|n| re.is_match(n)).take(limit).collect();
            if names.is_empty() {
                format!("no block names match /{pat}/")
            } else {
                names.join(", ")
            }
        }

        "scan_area" => {
            let r = (gi(input, "radius").max(1)).min(8) as i32;
            let origin: BlockPos = self_pos(bot).unwrap_or(Vec3::default()).into();
            let world = bot.world();
            let inst = world.read();
            let mut counts: std::collections::HashMap<String, i32> = std::collections::HashMap::new();
            for dx in -r..=r {
                for dy in -r..=r {
                    for dz in -r..=r {
                        let bp = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
                        if let Some(st) = inst.get_block_state(bp) {
                            if !st.is_air() {
                                *counts.entry(name_of_block_kind(st.into())).or_insert(0) += 1;
                            }
                        }
                    }
                }
            }
            if counts.is_empty() {
                "only air within range".to_string()
            } else {
                let mut v: Vec<_> = counts.into_iter().collect();
                v.sort_by(|a, b| b.1.cmp(&a.1));
                v.into_iter().take(25).map(|(n, c)| format!("{n} x{c}")).collect::<Vec<_>>().join(", ")
            }
        }

        "top_down" => {
            let pos = self_pos(bot).unwrap_or(Vec3::default());
            let foot_y = pos.y.floor() as i32;
            let ground_y = foot_y - 1;
            let start_y = foot_y + 1;
            let (cx, cz) = (pos.x.floor() as i32, pos.z.floor() as i32);
            let world = bot.world();
            let inst = world.read();
            let mut rows = Vec::new();
            for dz in -2..=2 {
                let mut cells = Vec::new();
                for dx in -2..=2 {
                    let mut cell = "∅".to_string();
                    let mut y = start_y;
                    while y >= start_y - 32 {
                        let st = inst.get_block_state(BlockPos::new(cx + dx, y, cz + dz));
                        if let Some(st) = st {
                            if !st.is_air() {
                                let level = y - ground_y;
                                let sign = if level >= 0 { "+" } else { "" };
                                cell = format!("{}{sign}{level}", name_of_block_kind(st.into()));
                                break;
                            }
                        }
                        y -= 1;
                    }
                    cells.push(cell);
                }
                rows.push(cells.join("  "));
            }
            format!("top-down 5x5 (rows north→south, cols west→east, you=center)\n{}", rows.join("\n"))
        }

        "set_behavior" => {
            let behavior = gs(input, "behavior").to_string();
            let enabled = input.get("enabled").and_then(Value::as_bool).unwrap_or(false);
            if enabled {
                ctx.behaviors.lock().insert(behavior.clone());
            } else {
                ctx.behaviors.lock().remove(&behavior);
            }
            format!("{behavior} {}", if enabled { "enabled" } else { "disabled" })
        }

        "save_routine" => {
            let steps: Vec<Value> =
                input.get("steps").and_then(Value::as_array).cloned().unwrap_or_default();
            let rname = gs(input, "name").to_string();
            if rname.is_empty() || steps.is_empty() {
                return Ok("need a name and non-empty steps".to_string());
            }
            let refs = referenced_tools(&steps);
            let known = tool_names();
            let bad: Vec<String> = refs
                .iter()
                .filter(|t| !known.contains(*t) || t.as_str() == "save_routine" || t.as_str() == "task_complete")
                .cloned()
                .collect();
            if !bad.is_empty() {
                return Ok(format!("steps reference tools that can't be used in a routine: {}", bad.join(", ")));
            }
            let desc = gs(input, "description").to_string();
            let n = refs.len();
            ctx.routines.save_routine(SHARED_SCOPE, Routine { name: rname.clone(), description: desc, steps });
            format!("saved routine \"{rname}\" ({n} distinct skills)")
        }

        "list_routines" => {
            let list = ctx.routines.list_routines(SHARED_SCOPE);
            if list.is_empty() {
                "no routines saved yet".to_string()
            } else {
                list.iter().map(|(n, d)| format!("{n}: {d}")).collect::<Vec<_>>().join("\n")
            }
        }

        "run_routine" => run_routine(ctx, input).await,

        _ => {
            if let Some(skill) = all_skills().into_iter().find(|s| s.tool().name == name) {
                skill.run(ctx, input.clone()).await
            } else {
                format!("unknown skill \"{name}\"")
            }
        }
    };
    Ok(out)
}

// --- run_routine ---

async fn run_routine(ctx: &SkillContext, input: &Value) -> String {
    let depth = current_depth();
    if depth >= 3 {
        return "routine nesting too deep (max 3)".to_string();
    }
    let rname = gs(input, "name").to_string();
    let Some(routine) = ctx.routines.get_routine(SHARED_SCOPE, &rname) else {
        let names = ctx
            .routines
            .list_routines(SHARED_SCOPE)
            .iter()
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>()
            .join(", ");
        let names = if names.is_empty() { "none".to_string() } else { names };
        return format!("no routine \"{rname}\" (known: {names})");
    };
    let note = ctx.note.clone();
    let rn = routine.name.clone();
    let step_note: Arc<dyn Fn(&str) + Send + Sync> =
        Arc::new(move |m: &str| note(&format!("↻ {rn}: {m}")));
    let mut rc = RunCtx {
        exec: make_exec(ctx.clone(), depth + 1),
        view: view_of(ctx),
        budget: Budget { steps: 0, max: 300 },
        deadline: Instant::now() + Duration::from_secs(300),
        log: Vec::new(),
        note: Some(step_note),
        interrupt: Some(ctx.wake.clone()), // owner prompt / damage aborts the routine so the planner re-plans
    };
    let args: Map<String, Value> =
        input.get("args").and_then(Value::as_object).cloned().unwrap_or_default();
    let result = run_steps(&routine.steps, &args, &mut rc).await;
    match result {
        Ok(()) => {
            let tail = tail_join(&rc.log, 4);
            format!("ran \"{}\" ({} skill calls). {tail}", routine.name, rc.budget.steps).trim().to_string()
        }
        Err(e) => {
            let tail = tail_join(&rc.log, 3);
            format!("routine \"{}\" stopped: {}. {tail}", routine.name, e).trim().to_string()
        }
    }
}

fn tail_join(log: &[String], n: usize) -> String {
    let start = log.len().saturating_sub(n);
    log[start..].join(" | ")
}

// --- container deposit/withdraw ---

async fn container_transfer(bot: &Client, name: &str, input: &Value) -> anyhow::Result<String> {
    let item = gs(input, "item");
    if item_kind_from_name(item).is_none() {
        return Ok(format!("unknown item \"{item}\""));
    }
    let (x, y, z) = (gi(input, "x") as i32, gi(input, "y") as i32, gi(input, "z") as i32);
    let at = BlockPos::new(x, y, z);
    if bot.world().read().get_block_state(at).is_none() {
        return Ok("no block at that coordinate".to_string());
    }
    if self_pos(bot).map(|p| p.distance_to(at.center())).unwrap_or(99.0) > 4.0 {
        let _ = with_timeout(bot.goto(RadiusGoal::new(at.center(), 2.0)), 30_000, "approach").await;
    }
    let Some(handle) = with_timeout(bot.open_container_at(at), 15_000, "open chest").await? else {
        return Ok("could not open the container".to_string());
    };
    let Some(menu) = handle.menu() else {
        return Ok("could not read the container".to_string());
    };
    let want = gi(input, "count").max(0);
    let slots = menu.slots();
    let player_range = menu.player_slots_range();
    let player_start = *player_range.start();
    // deposit moves player slots into the container; withdraw moves container slots out.
    // TODO(verify): shift_click moves whole stacks, so the moved count is stack-granular.
    let indices: Vec<usize> = if name == "deposit" {
        player_range.collect()
    } else {
        (0..player_start).collect()
    };
    let mut moved = 0i64;
    for i in indices {
        if moved >= want {
            break;
        }
        if let Some(s) = slots.get(i) {
            if s.is_present() && name_of_item_kind(s.kind()) == item {
                handle.shift_click(i);
                moved += s.count() as i64;
            }
        }
    }
    let msg = if name == "deposit" {
        if moved <= 0 {
            format!("not carrying any {item}")
        } else {
            format!("deposited {} {item}", moved.min(want))
        }
    } else if moved <= 0 {
        format!("chest has no {item}")
    } else {
        format!("withdrew {} {item}", moved.min(want))
    };
    handle.close();
    Ok(msg)
}

// --- entity/player lookup helpers ---

fn player_entity(bot: &Client, username: &str) -> Option<azalea::ecs::entity::Entity> {
    bot.player_uuid_by_username(username).and_then(|u| bot.entity_by_uuid(u))
}
fn player_pos(bot: &Client, username: &str) -> Option<Vec3> {
    player_entity(bot, username).and_then(|e| bot.get_entity_component::<Position>(e)).map(|p| *p)
}
fn entity_by_kind_name(bot: &Client, kind_name: &str) -> Option<azalea::ecs::entity::Entity> {
    bot.nearest_entity_by::<&EntityKindComponent, azalea::ecs::query::Without<LocalEntity>>(
        move |k: &EntityKindComponent| name_of_entity_kind(**k) == kind_name,
    )
}
fn entity_label(bot: &Client, e: azalea::ecs::entity::Entity) -> String {
    bot.get_entity_component::<EntityKindComponent>(e)
        .map(|k| name_of_entity_kind(*k))
        .unwrap_or_else(|| "target".to_string())
}

fn cardinal(dir: &str) -> Option<(f64, f64)> {
    match dir.to_lowercase().as_str() {
        "north" => Some((0.0, -1.0)),
        "south" => Some((0.0, 1.0)),
        "east" => Some((1.0, 0.0)),
        "west" => Some((-1.0, 0.0)),
        _ => None,
    }
}

fn carrying(bot: &Client, name: &str) -> bool {
    inv_stacks(bot).iter().any(|(n, _)| n == name)
}

/// Switch the held item to `name` if it's in the hotbar. Returns whether it was selected.
fn select_to_hand(bot: &Client, name: &str) -> bool {
    let Some(inv) = bot.get_component::<Inventory>() else {
        return false;
    };
    let menu = &inv.inventory_menu;
    let slots = menu.slots();
    for (i, idx) in menu.hotbar_slots_range().enumerate() {
        if let Some(s) = slots.get(idx) {
            if s.is_present() && name_of_item_kind(s.kind()) == name {
                bot.set_selected_hotbar_slot(i as u8);
                return true;
            }
        }
    }
    false
}

// --- auto-behaviors (port of installAutoBehaviors + autoEat + defend) ---

/// Wire the background toggles: built-in defend/auto_eat, registered behavior handlers,
/// and bot-authored rules, evaluated on a 1s tick. Returns the loop handle.
pub fn install_auto_behaviors(
    ctx: SkillContext,
    is_friendly: Arc<dyn Fn(&str) -> bool + Send + Sync>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let engine = RuleEngine::new();
        let mut last_health = self_health(&ctx.bot).unwrap_or(20.0);
        let mut eating = false;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if self_pos(&ctx.bot).is_none() {
                break; // disconnected
            }
            let behaviors = ctx.behaviors.lock().clone();
            let health = self_health(&ctx.bot).unwrap_or(20.0);
            let food = self_food(&ctx.bot).unwrap_or(20);

            if behaviors.contains("auto_eat") && food <= 14 && !eating {
                eating = true;
                auto_eat(&ctx).await;
                eating = false;
            }
            if health < last_health && behaviors.contains("defend") {
                defend(&ctx, &is_friendly);
            }
            for h in all_behaviors() {
                if behaviors.contains(h.name()) {
                    if health < last_health {
                        h.on_health(&ctx);
                    }
                    h.on_tick(&ctx);
                }
            }
            last_health = health;

            let rules = ctx.rules.list_rules(&ctx.scope());
            engine.tick(&rules, view_of(&ctx), make_exec(ctx.clone(), 0), Some(ctx.note.clone()));
        }
    })
}

/// Eat the first carried food when hungry (port of autoEat).
async fn auto_eat(ctx: &SkillContext) {
    let bot = &ctx.bot;
    let food_name = inv_stacks(bot).into_iter().map(|(n, _)| n).find(|n| ctx.mc_data.is_food(n));
    let Some(food_name) = food_name else {
        return;
    };
    if !select_to_hand(bot, &food_name) {
        return;
    }
    bot.start_use_item();
    // Hold the use for ~1.6s (32 ticks) so the item is consumed. TODO(verify).
    tokio::time::sleep(Duration::from_millis(1_600)).await;
}

/// Hit back the nearest attacker within 5m: hostile mobs, or non-friendly players (port of defend).
fn defend(ctx: &SkillContext, is_friendly: &Arc<dyn Fn(&str) -> bool + Send + Sync>) {
    let bot = &ctx.bot;
    for e in bot.nearest_entities_by::<(), azalea::ecs::query::Without<LocalEntity>>(|_: ()| true) {
        let Some(kind) = bot.get_entity_component::<EntityKindComponent>(e) else {
            continue;
        };
        let Some(pos) = bot.get_entity_component::<Position>(e) else {
            continue;
        };
        if self_pos(bot).map(|p| p.distance_to(*pos)).unwrap_or(99.0) > 5.0 {
            break; // nearest-first
        }
        if mc::is_hostile(*kind) {
            bot.attack(e);
            return;
        }
        if *kind == EntityKind::Player {
            let user = bot
                .get_entity_component::<EntityUuid>(e)
                .and_then(|u| bot.tab_list().get(&*u).map(|p| p.profile.name.clone()));
            if let Some(user) = user {
                if !is_friendly(&user) {
                    bot.attack(e);
                    return;
                }
            }
        }
    }
}

// --- tool schemas + summaries ---

fn schema(props: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false,
    })
}

fn tool(name: &str, description: &str, input_schema: Value) -> ToolDef {
    ToolDef { name: name.to_string(), description: description.to_string(), input_schema }
}

/// Base tool schemas (exact names + arg names — the planner calls them by name).
pub fn base_tools() -> Vec<ToolDef> {
    vec![
        tool("list_inventory", "List everything the bot is carrying.", schema(json!({}), &[])),
        tool("find_blocks", "Locate the nearest blocks of a type (e.g. oak_log, stone, iron_ore). Returns each as a signed offset from you (+x east, +y up, +z south) with distance, e.g. \"+5 -2 +3 (6m)\". Add an offset to your position (get_position) to get an absolute coordinate for go_to/mine_block.",
            schema(json!({ "name": { "type": "string" }, "count": { "type": "integer" }, "max_distance": { "type": "integer" } }), &["name", "count", "max_distance"])),
        tool("get_position", "Read your current absolute world coordinates (x, y, z). find_blocks and similar report locations as offsets from you; add them to this for absolute coordinates.", schema(json!({}), &[])),
        tool("whoami", "Report your own bot username and your owner's name — the owner is who you answer to and the one you can `message`. Returns \"unowned\" if you have no owner.", schema(json!({}), &[])),
        tool("go_to", "Walk to a coordinate.", schema(json!({ "x": { "type": "integer" }, "y": { "type": "integer" }, "z": { "type": "integer" } }), &["x", "y", "z"])),
        tool("go_to_player", "Walk to within 2 blocks of a named player.", schema(json!({ "username": { "type": "string" } }), &["username"])),
        tool("go_toward", "Travel up to <distance> blocks toward a heading: a cardinal direction (north/south/east/west) or the nearest block of a named type (e.g. oak_log). Best-effort relocation when go_to/collect_block can't path to an exact spot; reports where you end up.", schema(json!({ "target": { "type": "string" }, "distance": { "type": "integer" } }), &["target", "distance"])),
        tool("collect_block", "Find, mine, and pick up N blocks of a type (handles tools and pathing).", schema(json!({ "name": { "type": "string" }, "count": { "type": "integer" } }), &["name", "count"])),
        tool("mine_block", "Dig the block at an exact coordinate.", schema(json!({ "x": { "type": "integer" }, "y": { "type": "integer" }, "z": { "type": "integer" } }), &["x", "y", "z"])),
        tool("place_block", "Place a carried block at a coordinate (needs a solid block below it).", schema(json!({ "name": { "type": "string" }, "x": { "type": "integer" }, "y": { "type": "integer" }, "z": { "type": "integer" } }), &["name", "x", "y", "z"])),
        tool("craft_item", "Craft an item; uses a nearby crafting table if one is required.", schema(json!({ "name": { "type": "string" }, "count": { "type": "integer" } }), &["name", "count"])),
        tool("equip_item", "Equip a carried item.", schema(json!({ "name": { "type": "string" }, "destination": { "type": "string", "enum": ["hand", "head", "torso", "legs", "feet", "off-hand"] } }), &["name", "destination"])),
        tool("attack_nearest", "Approach and hit the nearest hostile mob once.", schema(json!({}), &[])),
        tool("attack_player", "Approach and hit a specific player once.", schema(json!({ "username": { "type": "string" } }), &["username"])),
        tool("fight", "Sustained melee until the target dies, flees, or ~30s pass. target = \"nearest\" (nearest hostile), a mob name (e.g. zombie), or a player username.", schema(json!({ "target": { "type": "string" } }), &["target"])),
        tool("flee", "Run away from the nearest hostile mob to roughly <distance> blocks (max 32).", schema(json!({ "distance": { "type": "integer" } }), &["distance"])),
        tool("follow_player", "Continuously follow a player for up to <seconds> (max 300), staying ~2 blocks away.", schema(json!({ "username": { "type": "string" }, "seconds": { "type": "integer" } }), &["username", "seconds"])),
        tool("deposit", "Put items into a chest at a coordinate (move next to it first).", schema(json!({ "item": { "type": "string" }, "count": { "type": "integer" }, "x": { "type": "integer" }, "y": { "type": "integer" }, "z": { "type": "integer" } }), &["item", "count", "x", "y", "z"])),
        tool("withdraw", "Take items from a chest at a coordinate (move next to it first).", schema(json!({ "item": { "type": "string" }, "count": { "type": "integer" }, "x": { "type": "integer" }, "y": { "type": "integer" }, "z": { "type": "integer" } }), &["item", "count", "x", "y", "z"])),
        tool("match_block_names", "Search block names by regex — Minecraft names are often unintuitive (e.g. 'log' matches oak_log, spruce_log). Returns matching names.", schema(json!({ "pattern": { "type": "string" }, "limit": { "type": "integer" } }), &["pattern", "limit"])),
        tool("scan_area", "Wide look around: counts every solid block within a radius (max 8) by type. Use to understand surroundings.", schema(json!({ "radius": { "type": "integer" } }), &["radius"])),
        tool("top_down", "Top-down 5x5 heightmap: for each column around you, the first block going down from eye height, with its height vs. the ground you stand on (eye=+2, waist=+1, ground=0, below negative).", schema(json!({}), &[])),
        tool("set_behavior", "Toggle a background auto-behavior that runs until turned off: defend, auto_eat, maintain_light, retreat_if_low_health, lava_guard, anti_stuck.", schema(json!({ "behavior": { "type": "string", "enum": ["defend", "auto_eat", "maintain_light", "retreat_if_low_health", "lava_guard", "anti_stuck"] }, "enabled": { "type": "boolean" } }), &["behavior", "enabled"])),
        tool("task_complete", "Call when the goal is achieved or is impossible. Ends the task.", schema(json!({ "summary": { "type": "string" } }), &["summary"])),
        tool("save_routine",
            "Save a reusable procedure built from other skills, so repetitive work runs without planning each step. A step is one of: {\"tool\":\"<skill>\",\"args\":{...}} | {\"repeat\":N,\"do\":[steps]} | {\"until\":\"<cond>\",\"max\":N,\"do\":[steps]} | {\"when\":\"<cond>\",\"do\":[steps],\"else\":[steps]}. Use {param} placeholders in args/conditions, filled by run_routine. Conditions: have:<item><op>N, find:<block><op>N, health<op>N, food<op>N (op is >=,<=,>,<,==,!=). Example gather: [{\"until\":\"have:{block}>={count}\",\"max\":30,\"do\":[{\"tool\":\"collect_block\",\"args\":{\"name\":\"{block}\",\"count\":16}},{\"when\":\"find:{block}==0\",\"do\":[{\"tool\":\"dig_staircase\",\"args\":{\"direction\":\"down\",\"length\":8}}]}]}].",
            json!({ "type": "object", "properties": { "name": { "type": "string" }, "description": { "type": "string" }, "steps": { "type": "array", "items": { "type": "object" } } }, "required": ["name", "description", "steps"], "additionalProperties": false })),
        tool("run_routine", "Run a saved routine by name, filling its {param} placeholders from args (e.g. {\"block\":\"cobblestone\",\"count\":64}). Executes deterministically with no per-step planning.",
            json!({ "type": "object", "properties": { "name": { "type": "string" }, "args": { "type": "object" } }, "required": ["name", "args"], "additionalProperties": false })),
        tool("list_routines", "List saved routines (name + description) available to reuse.", schema(json!({}), &[])),
    ]
}

/// Base tools plus everything registered by the category modules.
pub fn tools() -> Vec<ToolDef> {
    let mut v = base_tools();
    v.extend(all_skills().into_iter().map(|s| s.tool()));
    v
}

/// All valid tool names (for routine validation).
fn tool_names() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| tools().into_iter().map(|t| t.name).collect())
}

/// Deterministically shrink an old tool result for compacted history (no model call).
pub fn summarize_result(name: &str, full: &str) -> String {
    let first_line = full.split('\n').next().unwrap_or("");
    match name {
        "top_down" => "(top-down heightmap surveyed)".to_string(),
        "scan_area" => "(area scanned)".to_string(),
        "find_blocks" => format!("(located: {}…)", full.split(';').next().unwrap_or("").trim()),
        "match_block_names" => "(block-name search done)".to_string(),
        "match_item_names" => "(item-name search done)".to_string(),
        "list_inventory" => "(inventory checked)".to_string(),
        "get_recipe" => format!("(recipe: {})", first_line.chars().take(80).collect::<String>()),
        "inventory_gap" => format!("(gap: {})", full.chars().take(80).collect::<String>()),
        "list_routines" => "(routines listed)".to_string(),
        "run_routine" => full.split('.').next().unwrap_or("").to_string(),
        _ => {
            if first_line.chars().count() > 160 {
                let truncated: String = first_line.chars().take(157).collect();
                format!("{truncated}…")
            } else {
                first_line.to_string()
            }
        }
    }
}
