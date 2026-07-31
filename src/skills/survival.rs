//! Survival skill (dig_down_safe) + auto-behaviors (port of skills_survival.ts).
//! Behaviors keep transient flags in Arc<Atomic/Mutex> and kick off async work via
//! tokio::spawn; on_tick/on_health stay sync and return fast.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use azalea::entity::inventory::Inventory;
use azalea::entity::Position;
use azalea::pathfinder::goals::RadiusGoal;
use azalea::pathfinder::Pathfinder;
use azalea::prelude::*; // BotClientExt, PathfinderClientExt, Client
use azalea::registry::builtin::BlockKind;
use azalea::{BlockPos, Vec3};
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::llm::ToolDef;
use crate::skills::{BehaviorHandler, Skill, SkillContext};

// --- shared perception helpers ---

/// air/void/fluid two-below counts as a hazard to descend into (missing = unloaded = hazard).
fn is_hazard_name(n: Option<&str>) -> bool {
    match n {
        None => true,
        Some(s) => {
            s == "air" || s == "cave_air" || s == "void_air" || s.contains("lava") || s.contains("water")
        }
    }
}

fn is_lava(n: Option<&str>) -> bool {
    n.map_or(false, |s| s.contains("lava"))
}

/// Full-cube placeable/diggable face (approx of mineflayer boundingBox=="block"). TODO(verify): exact shape.
fn is_solid_face(name: &str) -> bool {
    name != "air"
        && name != "cave_air"
        && name != "void_air"
        && !name.contains("lava")
        && !name.contains("water")
}

fn block_name_at(bot: &Client, pos: BlockPos) -> Option<String> {
    let state = bot.world().read().get_block_state(pos)?;
    Some(crate::mc::name_of_block_kind(BlockKind::from(state)))
}

fn sign(v: f64) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

/// Horizontal unit-block offset the bot faces, from its yaw.
/// TODO(verify): azalea `direction()` yaw is degrees; mineflayer used radians.
fn heading_offset(bot: &Client) -> (i32, i32) {
    let (yaw_deg, _) = bot.direction();
    let yaw = (yaw_deg as f64).to_radians();
    let fx = -yaw.sin();
    let fz = -yaw.cos();
    let dx = if fx.abs() >= fz.abs() { sign(fx) } else { 0 };
    let dz = if fz.abs() > fx.abs() { sign(fz) } else { 0 };
    (dx, dz)
}

/// azalea 0.15 exposes no per-block light; mirror the TS `?? 15` fallback (never dark).
/// TODO(verify): read chunk block/sky light when azalea surfaces it.
fn feet_light(_bot: &Client, _pos: BlockPos) -> u8 {
    15
}

fn is_pathfinding(bot: &Client) -> bool {
    bot.get_component::<Pathfinder>()
        .map_or(false, |p| p.goal.is_some() || p.is_calculating)
}

/// First hotbar slot holding a torch. TODO(verify): TS equips torches from anywhere in inventory.
fn find_torch_hotbar(bot: &Client) -> Option<u8> {
    let inv = bot.get_component::<Inventory>()?;
    let menu = &inv.inventory_menu;
    let slots = menu.slots();
    let hotbar = &slots[menu.hotbar_slots_range()];
    hotbar
        .iter()
        .position(|s| crate::mc::name_of_item_kind(s.kind()).contains("torch"))
        .map(|i| i as u8)
}

// --- skill: dig_down_safe ---

struct DigDownSafe;

#[async_trait]
impl Skill for DigDownSafe {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "dig_down_safe".into(),
            description: "Mine straight down up to <depth> blocks, stopping before any lava/water/void two blocks below. Returns blocks descended.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "depth": { "type": "integer" } },
                "required": ["depth"],
                "additionalProperties": false,
            }),
        }
    }

    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let bot = &ctx.bot;
        let raw = input
            .get("depth")
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
            .unwrap_or(0);
        let depth = raw.min(64).max(1);
        let mut descended = 0i64;
        for _ in 0..depth {
            let feet: Vec3 = match bot.get_component::<Position>() {
                Some(p) => *p,
                None => break,
            };
            let base: BlockPos = feet.into();
            let below2 = block_name_at(bot, base.down(2));
            if is_hazard_name(below2.as_deref()) {
                return format!(
                    "stopped after {descended} blocks: hazard below ({})",
                    below2.as_deref().unwrap_or("void")
                );
            }
            let target = base.down(1);
            let state = bot.world().read().get_block_state(target);
            if let Some(state) = state {
                if crate::mc::name_of_block_kind(BlockKind::from(state)) != "air" {
                    crate::mc::equip_best_tool(bot, state, false).await;
                    if crate::mc::with_timeout(bot.mine(target), 15_000, "dig").await.is_err() {
                        return format!("stopped after {descended} blocks: dig failed (timed out)");
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
            descended += 1;
        }
        format!("descended {descended} blocks")
    }
}

pub fn skills() -> Vec<Arc<dyn Skill>> {
    vec![Arc::new(DigDownSafe)]
}

// --- behaviors ---

/// Place a torch when standing in the dark (gated on light, currently always-lit; see feet_light).
struct MaintainLight {
    lit_up: Arc<AtomicBool>,
}

impl BehaviorHandler for MaintainLight {
    fn name(&self) -> &str {
        "maintain_light"
    }
    fn on_tick(&self, ctx: &SkillContext) {
        if self.lit_up.load(Ordering::Relaxed) {
            return;
        }
        let feet = match ctx.bot.get_component::<Position>() {
            Some(p) => *p,
            None => return,
        };
        let base: BlockPos = feet.into();
        if feet_light(&ctx.bot, base) >= 8 {
            return;
        }
        if find_torch_hotbar(&ctx.bot).is_none() {
            return;
        }
        self.lit_up.store(true, Ordering::Relaxed);
        let bot = ctx.bot.clone();
        let flag = self.lit_up.clone();
        tokio::spawn(async move {
            if let Some(slot) = find_torch_hotbar(&bot) {
                bot.set_selected_hotbar_slot(slot);
                let refs = [
                    base.down(1),
                    base + BlockPos::new(1, 0, 0),
                    base + BlockPos::new(-1, 0, 0),
                    base + BlockPos::new(0, 0, 1),
                    base + BlockPos::new(0, 0, -1),
                ];
                for r in refs {
                    let solid = bot
                        .world()
                        .read()
                        .get_block_state(r)
                        .map(|s| is_solid_face(&crate::mc::name_of_block_kind(BlockKind::from(s))))
                        .unwrap_or(false);
                    if solid {
                        // Place against the referenced face: look at it, then interact. TODO(verify): face selection.
                        bot.look_at(r.center());
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        bot.block_interact(r);
                        break;
                    }
                }
            }
            flag.store(false, Ordering::Relaxed);
        });
    }
}

/// Flee 10 blocks (away from the nearest hostile if any) when health drops to <=7.
struct RetreatIfLowHealth {
    retreating: Arc<AtomicBool>,
}

impl BehaviorHandler for RetreatIfLowHealth {
    fn name(&self) -> &str {
        "retreat_if_low_health"
    }
    fn on_health(&self, ctx: &SkillContext) {
        if ctx.bot.health() > 7.0 || self.retreating.load(Ordering::Relaxed) {
            return;
        }
        let p = match ctx.bot.get_component::<Position>() {
            Some(p) => *p,
            None => return,
        };
        let threat_pos = crate::mc::nearest_hostile(&ctx.bot, None)
            .map(|e| *ctx.bot.entity_component::<Position>(e));
        self.retreating.store(true, Ordering::Relaxed);
        let bot = ctx.bot.clone();
        let flag = self.retreating.clone();
        tokio::spawn(async move {
            let dest = if let Some(tp) = threat_pos {
                let (ax, ay, az) = (p.x - tp.x, p.y - tp.y, p.z - tp.z);
                let mag = (ax * ax + ay * ay + az * az).sqrt();
                let (dx, dy, dz) = if mag > 0.001 {
                    (ax / mag, ay / mag, az / mag)
                } else {
                    (1.0, 0.0, 0.0)
                };
                Vec3::new(p.x + dx * 10.0, p.y + dy * 10.0, p.z + dz * 10.0)
            } else {
                Vec3::new(p.x + 10.0, p.y, p.z)
            };
            bot.goto(RadiusGoal::new(dest, 2.0)).await;
            tokio::time::sleep(Duration::from_millis(2000)).await;
            flag.store(false, Ordering::Relaxed);
        });
    }
}

/// Back away when lava/a >3-block drop is directly ahead in the facing direction.
struct LavaGuard {
    guarding: Arc<AtomicBool>,
}

impl BehaviorHandler for LavaGuard {
    fn name(&self) -> &str {
        "lava_guard"
    }
    fn on_tick(&self, ctx: &SkillContext) {
        if self.guarding.load(Ordering::Relaxed) {
            return;
        }
        let p = match ctx.bot.get_component::<Position>() {
            Some(p) => *p,
            None => return,
        };
        let (dx, dz) = heading_offset(&ctx.bot);
        if dx == 0 && dz == 0 {
            return;
        }
        let base: BlockPos = p.into();
        let ahead = base + BlockPos::new(dx, 0, dz);
        let ahead_name = block_name_at(&ctx.bot, ahead);
        let below_ahead = block_name_at(&ctx.bot, ahead.down(1));
        let mut drop = 0;
        for i in 1i32..=4 {
            match block_name_at(&ctx.bot, ahead.down(i)) {
                Some(n) if n != "air" && n != "cave_air" => break,
                _ => drop += 1,
            }
        }
        if !is_lava(ahead_name.as_deref()) && !is_lava(below_ahead.as_deref()) && drop <= 3 {
            return;
        }
        self.guarding.store(true, Ordering::Relaxed);
        let bot = ctx.bot.clone();
        let flag = self.guarding.clone();
        tokio::spawn(async move {
            // Stop pathing and step back briefly. TODO(verify): control-state mapping (clear/back).
            bot.stop_pathfinding();
            bot.walk(azalea::WalkDirection::None);
            bot.walk(azalea::WalkDirection::Backward);
            tokio::time::sleep(Duration::from_millis(300)).await;
            bot.walk(azalea::WalkDirection::None);
            tokio::time::sleep(Duration::from_millis(500)).await;
            flag.store(false, Ordering::Relaxed);
        });
    }
}

/// Jump (and dig ahead) when pathfinding but not making progress.
struct AntiStuck {
    stall: Arc<AtomicI32>,
    last_pos: Arc<Mutex<Option<Vec3>>>,
    unsticking: Arc<AtomicBool>,
}

impl BehaviorHandler for AntiStuck {
    fn name(&self) -> &str {
        "anti_stuck"
    }
    fn on_tick(&self, ctx: &SkillContext) {
        if !is_pathfinding(&ctx.bot) {
            self.stall.store(0, Ordering::Relaxed);
            return;
        }
        let p = match ctx.bot.get_component::<Position>() {
            Some(p) => *p,
            None => return,
        };
        let last = *self.last_pos.lock();
        let moved = last.map_or(true, |l| p.distance_to(l) > 0.2);
        *self.last_pos.lock() = Some(p);
        if moved {
            self.stall.store(0, Ordering::Relaxed);
            return;
        }
        let count = self.stall.fetch_add(1, Ordering::Relaxed) + 1;
        if count < 3 || self.unsticking.load(Ordering::Relaxed) {
            return;
        }
        self.unsticking.store(true, Ordering::Relaxed);
        self.stall.store(0, Ordering::Relaxed);
        let (dx, dz) = heading_offset(&ctx.bot);
        let ahead = { let base: BlockPos = p.into(); base + BlockPos::new(dx, 0, dz) };
        let bot = ctx.bot.clone();
        let flag = self.unsticking.clone();
        tokio::spawn(async move {
            // Jump-clear, then dig a full block directly ahead. TODO(verify): control-state (jump).
            bot.set_jumping(true);
            tokio::time::sleep(Duration::from_millis(300)).await;
            bot.set_jumping(false);
            let dig = bot
                .world()
                .read()
                .get_block_state(ahead)
                .map(|s| is_solid_face(&crate::mc::name_of_block_kind(BlockKind::from(s))))
                .unwrap_or(false);
            if dig {
                let _ = crate::mc::with_timeout(bot.mine(ahead), 10_000, "unstick dig").await;
            }
            flag.store(false, Ordering::Relaxed);
        });
    }
}

pub fn behaviors() -> Vec<Arc<dyn BehaviorHandler>> {
    vec![
        Arc::new(MaintainLight { lit_up: Arc::new(AtomicBool::new(false)) }),
        Arc::new(RetreatIfLowHealth { retreating: Arc::new(AtomicBool::new(false)) }),
        Arc::new(LavaGuard { guarding: Arc::new(AtomicBool::new(false)) }),
        Arc::new(AntiStuck {
            stall: Arc::new(AtomicI32::new(0)),
            last_pos: Arc::new(Mutex::new(None)),
            unsticking: Arc::new(AtomicBool::new(false)),
        }),
    ]
}
