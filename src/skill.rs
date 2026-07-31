//! Azalea-free shared contract (port of skillkit.ts, minus the azalea-touching SkillContext/Skill,
//! which live in skills/mod.rs). Layers 1–3 depend only on this.
use crate::types::{CreateResult, Pos};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One shared library collection for every agent (routines + settings).
pub const SHARED_SCOPE: &str = "shared";

/// Memory scope: the owner if any, else the bot's own name.
pub fn scope_of(owner: Option<&str>, username: &str) -> String {
    owner.unwrap_or(username).to_string()
}

/// A coordinate as a signed offset from `from`, with 3D distance: "+5 -2 +3 (6m)".
pub fn rel(from: Pos, to: Pos) -> String {
    let (dx, dy, dz) = (
        (to.x - from.x).round() as i64,
        (to.y - from.y).round() as i64,
        (to.z - from.z).round() as i64,
    );
    let s = |n: i64| if n >= 0 { format!("+{n}") } else { n.to_string() };
    let dist = ((dx * dx + dy * dy + dz * dz) as f64).sqrt().round() as i64;
    format!("{} {} {} ({}m)", s(dx), s(dy), s(dz), dist)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LedgerStatus {
    Todo,
    Doing,
    Done,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerItem {
    pub text: String,
    pub status: LedgerStatus,
}

/// A saved, replayable procedure. `steps` is interpreted data, not code.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Routine {
    pub name: String,
    pub description: String,
    pub steps: Vec<serde_json::Value>,
}

/// A bot-authored reactive setting: when `condition` holds, run `steps`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    pub condition: String,
    pub steps: Vec<serde_json::Value>,
    pub enabled: bool,
}

/// Host-side durable memory, scoped per owner. Backed by SQLite (store.rs).
pub trait Memory: Send + Sync {
    fn set_waypoint(&self, scope: &str, name: &str, pos: Pos);
    fn get_waypoint(&self, scope: &str, name: &str) -> Option<Pos>;
    fn list_waypoints(&self, scope: &str) -> Vec<(String, Pos)>;
    fn set_note(&self, scope: &str, key: &str, text: &str);
    fn list_notes(&self, scope: &str, query: Option<&str>) -> Vec<(String, String)>;
    fn ledger(&self, scope: &str) -> Vec<LedgerItem>;
    fn set_ledger_item(&self, scope: &str, text: &str, status: LedgerStatus) -> Vec<LedgerItem>;
    fn summary(&self, scope: &str) -> String;
}

pub trait RoutineStore: Send + Sync {
    fn save_routine(&self, scope: &str, routine: Routine);
    fn get_routine(&self, scope: &str, name: &str) -> Option<Routine>;
    /// (name, description) pairs.
    fn list_routines(&self, scope: &str) -> Vec<(String, String)>;
}

pub trait RuleStore: Send + Sync {
    fn save_rule(&self, scope: &str, rule: Rule);
    fn list_rules(&self, scope: &str) -> Vec<Rule>;
    fn delete_rule(&self, scope: &str, name: &str) -> bool;
}

/// Owner lookup: None = not a managed agent (TS `undefined`); Some(None) = unowned (`null`); Some(Some) = owner.
pub type OwnerLookup = Option<Option<String>>;

/// Cross-agent access, backed by the manager.
pub trait PeerApi: Send + Sync {
    fn position(&self, name: &str) -> Option<Pos>;
    fn online(&self, name: &str) -> bool;
    fn send(&self, to: &str, from: &str, message: &str) -> bool;
    fn owner_of(&self, name: &str) -> OwnerLookup;
    fn teammates(&self, owner: Option<&str>) -> Vec<String>;
    fn summon(&self, count: usize, goal: &str, owner: Option<&str>) -> CreateResult;
}

/// Read view over one bot for condition eval (routines/rules), implemented by the azalea layer.
/// Keeps layers 1–3 free of azalea imports.
pub trait BotView {
    fn inv_count(&self, item: &str) -> i64;
    fn nearby_count(&self, block: &str) -> i64;
    fn health(&self) -> f32;
    fn food(&self) -> f32;
}

/// Tool executor seam: (tool name, json args) -> natural-language result. Boxed so the
/// routine/rule interpreters never import azalea.
pub type Exec = Arc<dyn Fn(String, serde_json::Value) -> BoxFuture<'static, String> + Send + Sync>;
