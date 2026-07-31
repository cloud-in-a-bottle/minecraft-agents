//! Cross-agent skills (port of skills_multiagent.ts): summon, activate, collect drops, give, go-to.
use crate::llm::ToolDef;
use crate::mc;
use crate::skill::rel;
use crate::skills::{Skill, SkillContext};
use crate::types::{Pos, RejectReason};
use async_trait::async_trait;
use azalea::block::BlockState;
use azalea::entity::metadata::ItemItem;
use azalea::entity::{EntityKindComponent, Position};
use azalea::pathfinder::goals::RadiusGoal;
use azalea::prelude::*;
use azalea::registry::builtin::{BlockKind, EntityKind};
use azalea::{BlockPos, Vec3};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn skills() -> Vec<Arc<dyn Skill>> {
    vec![
        Arc::new(SummonAgents),
        Arc::new(ActivateBlock),
        Arc::new(CollectDrops),
        Arc::new(FindItems),
        Arc::new(GiveItem),
        Arc::new(GoToAgent),
    ]
}

struct SummonAgents;
#[async_trait]
impl Skill for SummonAgents {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "summon_agents".into(),
            description: "Summon <count> helper agents to work on <goal>. They are owned by your owner and count toward that player's agent cap.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "count": { "type": "integer" }, "goal": { "type": "string" } },
                "required": ["count", "goal"], "additionalProperties": false,
            }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let count = input.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
        let goal = input.get("goal").and_then(Value::as_str).unwrap_or("");
        let r = ctx.peers.summon(count, goal, ctx.self_.owner.as_deref());
        if r.created.is_empty() {
            return if r.reason == Some(RejectReason::UserLimit) {
                "your owner's agent cap is reached".into()
            } else {
                "cannot summon — agent limit reached".into()
            };
        }
        let over = if r.rejected > 0 { format!(" ({} over cap)", r.rejected) } else { String::new() };
        format!("summoned {} for: {goal}{over}", r.created.join(", "))
    }
}

struct ActivateBlock;
#[async_trait]
impl Skill for ActivateBlock {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "activate_block".into(),
            description: "Right-click the block at a coordinate to toggle a door, lever, button, or pressure plate.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "x": { "type": "integer" }, "y": { "type": "integer" }, "z": { "type": "integer" } },
                "required": ["x", "y", "z"], "additionalProperties": false,
            }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let (x, y, z) = (int(&input, "x"), int(&input, "y"), int(&input, "z"));
        let pos = BlockPos::new(x, y, z);
        let state = ctx.bot.world().read().get_block_state(pos); // TODO(verify): Instance::get_block_state
        let state = match state {
            Some(s) if !s.is_air() => s,
            _ => return "no block at that coordinate".into(),
        };
        let name = block_state_name(ctx, state);
        ctx.bot.block_interact(pos); // TODO(verify): fire-and-queue; TS awaited activateBlock with a 15s timeout
        format!("activated {name}")
    }
}

struct CollectDrops;
#[async_trait]
impl Skill for CollectDrops {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "collect_drops".into(),
            description: "Walk over nearby dropped items within a radius to pick them up. Returns how many were gathered.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "radius": { "type": "integer" } },
                "required": ["radius"], "additionalProperties": false,
            }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let radius = (int(&input, "radius")).max(1) as f64;
        let origin = ctx.bot.position();
        // TODO(verify): item drops as EntityKind::Item entities; nearest-first, distance-filtered.
        let drops: Vec<Vec3> = ctx
            .bot
            .nearest_entities_by::<&EntityKindComponent, ()>(|k: &EntityKindComponent| k.0 == EntityKind::Item)
            .into_iter()
            .map(|e| *ctx.bot.entity_component::<Position>(e))
            .filter(|p| origin.distance_to(*p) <= radius)
            .take(10)
            .collect();
        if drops.is_empty() {
            return format!("no dropped items within {}m", radius as i64);
        }
        let total = drops.len();
        let mut gathered = 0;
        for p in drops {
            let goal = RadiusGoal::new(p, 1.0); // walk onto the drop (TS GoalNear r=0)
            if mc::with_timeout(ctx.bot.goto(goal), 8_000, "collect_drops").await.is_ok() {
                gathered += 1;
            }
        }
        format!("gathered {gathered} of {total} dropped item(s)")
    }
}

struct FindItems;
#[async_trait]
impl Skill for FindItems {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "find_items".into(),
            description: "Scan for dropped items within <max_distance>m. name \"\" lists all; a non-empty name filters to that item. Each result: \"<item> x<count>  <offset>\", nearest-first.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "name": { "type": "string" }, "max_distance": { "type": "integer" } },
                "required": ["name", "max_distance"], "additionalProperties": false,
            }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        use azalea::registry::Registry;
        let name = input.get("name").and_then(Value::as_str).unwrap_or("");
        let max = (int(&input, "max_distance")).max(1) as f64;
        let origin = ctx.bot.position();
        let from = Pos { x: origin.x, y: origin.y, z: origin.z };
        let mut lines = Vec::new();
        // nearest_entities_by is already sorted nearest-first.
        for e in ctx.bot.nearest_entities_by::<&EntityKindComponent, ()>(|k: &EntityKindComponent| k.0 == EntityKind::Item) {
            let p = *ctx.bot.entity_component::<Position>(e);
            if origin.distance_to(p) > max {
                continue;
            }
            let stack = ctx.bot.get_entity_component::<ItemItem>(e).map(|s| s.0);
            let (item, count) = match stack.as_ref().filter(|s| s.is_present()) {
                Some(s) => (
                    ctx.mc_data.item_name(s.kind().to_u32()).unwrap_or_else(|| "item".into()),
                    s.count().max(1),
                ),
                None => ("item".into(), 1),
            };
            if !name.is_empty() && item != name {
                continue;
            }
            let to = Pos { x: p.x, y: p.y, z: p.z };
            lines.push(format!("{item} x{count}  {}", rel(from, to)));
            if lines.len() >= 20 {
                break;
            }
        }
        if lines.is_empty() {
            return if name.is_empty() {
                format!("no dropped items within {}m", max as i64)
            } else {
                format!("no {name} dropped within {}m", max as i64)
            };
        }
        lines.join("\n")
    }
}

struct GiveItem;
#[async_trait]
impl Skill for GiveItem {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "give_item".into(),
            description: "Toss items to another agent or a human player: face them and drop the given count of the item.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "target": { "type": "string" }, "item": { "type": "string" }, "count": { "type": "integer" } },
                "required": ["target", "item", "count"], "additionalProperties": false,
            }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let item = input.get("item").and_then(Value::as_str).unwrap_or("");
        let target = input.get("target").and_then(Value::as_str).unwrap_or("");
        let count = int(&input, "count") as i64;
        let item_id = match ctx.mc_data.item_id(item) {
            Some(id) => id,
            None => return format!("unknown item \"{item}\""),
        };
        let have = inv_count(ctx, item_id);
        if have <= 0 {
            return format!("not carrying any {item}");
        }
        let pos = ctx
            .peers
            .position(target)
            .map(|p| Vec3::new(p.x, p.y, p.z))
            .or_else(|| player_pos(ctx, target));
        let pos = match pos {
            Some(p) => p,
            None => return format!("target \"{target}\" not found"),
        };
        let n = count.min(have);
        ctx.bot.look_at(pos);
        // TODO(verify): azalea 0.15 exposes no toss/drop; port via a drop packet or ContainerClickEvent (Drop mode).
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        format!("gave {n} {item} to {target}")
    }
}

struct GoToAgent;
#[async_trait]
impl Skill for GoToAgent {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "go_to_agent".into(),
            description: "Walk to within <range> blocks of another agent's current position.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "agent": { "type": "string" }, "range": { "type": "integer" } },
                "required": ["agent", "range"], "additionalProperties": false,
            }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let agent = input.get("agent").and_then(Value::as_str).unwrap_or("");
        let range = (int(&input, "range")).max(1) as f32;
        let p = match ctx.peers.position(agent) {
            Some(p) => p,
            None => return format!("agent \"{agent}\" not online/visible"),
        };
        let goal = RadiusGoal::new(Vec3::new(p.x, p.y, p.z), range);
        match mc::with_timeout(ctx.bot.goto(goal), 60_000, "go_to_agent").await {
            Ok(()) => format!("reached {agent}"),
            Err(e) => format!("error: {e}"),
        }
    }
}

fn int(v: &Value, key: &str) -> i32 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0) as i32
}

/// Total carried count of an item across the player inventory.
fn inv_count(ctx: &SkillContext, item_id: u32) -> i64 {
    use azalea::registry::Registry;
    ctx.bot
        .menu()
        .slots() // TODO(verify): Menu::slots() -> Vec<ItemStack>
        .iter()
        .filter(|s| s.kind().to_u32() == item_id)
        .map(|s| s.count() as i64)
        .sum()
}

/// Position of a visible player by username, via a GameProfile match.
// TODO(verify): GameProfileComponent / metadata::Player paths + Position deref.
fn player_pos(ctx: &SkillContext, name: &str) -> Option<Vec3> {
    use azalea::entity::metadata::Player;
    use azalea::player::GameProfileComponent;
    use azalea::ecs::query::With;
    let e = ctx.bot.any_entity_by::<&GameProfileComponent, With<Player>>(
        |p: &GameProfileComponent| p.name == name,
    )?;
    Some(*ctx.bot.entity_component::<Position>(e))
}

/// Human-readable block name for a state, via mc_data.
fn block_state_name(ctx: &SkillContext, state: BlockState) -> String {
    use azalea::registry::Registry;
    let kind: BlockKind = state.into(); // TODO(verify): BlockState -> BlockKind
    ctx.mc_data.block_name(kind.to_u32()).unwrap_or_else(|| "block".into())
}
