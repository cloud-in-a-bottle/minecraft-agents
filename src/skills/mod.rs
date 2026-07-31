//! Azalea-touching skill contract (port of the SkillContext/Skill half of skillkit.ts) + registry.
//! The azalea-free traits live in crate::skill. Layer 4 (gated on the workspace) fills the modules.
pub mod base;
pub mod iron;
pub mod memory;
pub mod messaging;
pub mod multiagent;
pub mod presence;
pub mod rules;
pub mod survival;

use crate::llm::ToolDef;
use crate::skill::{Memory, PeerApi, RoutineStore, RuleStore};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::Arc;

/// Block/item registry facade (port of the `minecraft-data` usage: blocksByName, itemsByName,
/// items[id], foodsByName, hardness, harvestTools). Implemented in crate::mc over azalea's registries.
pub trait McData: Send + Sync {
    fn block_id(&self, name: &str) -> Option<u32>;
    fn block_name(&self, id: u32) -> Option<String>;
    fn item_id(&self, name: &str) -> Option<u32>;
    fn item_name(&self, id: u32) -> Option<String>;
    fn is_food(&self, name: &str) -> bool;
    fn block_names(&self) -> Vec<String>;
    fn item_names(&self) -> Vec<String>;
    /// (hardness, harvest tool item names, whether unbreakable).
    fn block_harvest(&self, name: &str) -> Option<(f32, Vec<String>)>;
}

#[derive(Clone)]
pub struct SelfInfo {
    pub username: String,
    pub owner: Option<String>,
}

/// Everything a skill/behavior needs. `bot` is the azalea client (Clone).
#[derive(Clone)]
pub struct SkillContext {
    pub bot: azalea::Client, // TODO(verify): azalea 0.15 Client type/path
    pub mc_data: Arc<dyn McData>,
    pub memory: Arc<dyn Memory>,
    pub peers: Arc<dyn PeerApi>,
    pub routines: Arc<dyn RoutineStore>,
    pub rules: Arc<dyn RuleStore>,
    pub self_: SelfInfo,
    pub behaviors: Arc<Mutex<HashSet<String>>>,
    /// Live activity-log sink (the agent's log).
    pub note: Arc<dyn Fn(&str) + Send + Sync>,
    /// True when the planner has a pending injected message (owner prompt / damage) — routines poll
    /// this to abort early so the planner can react instead of finishing a long routine first.
    pub wake: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl SkillContext {
    pub fn note(&self, msg: &str) {
        (self.note)(msg)
    }
    /// Memory scope: owner if any, else own username.
    pub fn scope(&self) -> String {
        crate::skill::scope_of(self.self_.owner.as_deref(), &self.self_.username)
    }
}

/// A pluggable tool: its schema + executor. Base tools are handled by base::execute's match instead.
#[async_trait]
pub trait Skill: Send + Sync {
    fn tool(&self) -> ToolDef;
    async fn run(&self, ctx: &SkillContext, input: serde_json::Value) -> String;
}

/// A background auto-behavior, toggled by set_behavior, run on health/tick.
pub trait BehaviorHandler: Send + Sync {
    fn name(&self) -> &str;
    fn on_health(&self, _ctx: &SkillContext) {}
    fn on_tick(&self, _ctx: &SkillContext) {}
}

/// Aggregated registry (port of registry.ts). Each module exposes `skills()` / `behaviors()`.
pub fn all_skills() -> Vec<Arc<dyn Skill>> {
    let mut v: Vec<Arc<dyn Skill>> = Vec::new();
    v.extend(iron::skills());
    v.extend(memory::skills());
    v.extend(survival::skills());
    v.extend(multiagent::skills());
    v.extend(presence::skills());
    v.extend(messaging::skills());
    v.extend(rules::skills());
    v
}

pub fn all_behaviors() -> Vec<Arc<dyn BehaviorHandler>> {
    survival::behaviors()
}
