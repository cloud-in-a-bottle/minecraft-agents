//! Memory skills (port of skills_memory.ts): waypoints, notes, task ledger.
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use azalea::pathfinder::PathfinderClientExt;

use crate::llm::ToolDef;
use crate::skill::LedgerStatus;
use crate::skills::{Skill, SkillContext};
use crate::types::Pos;

// TODO(verify): bot.position() -> Vec3 with f64 x/y/z.
fn pos(ctx: &SkillContext) -> Pos {
    let p = ctx.bot.position();
    Pos { x: p.x.round(), y: p.y.round(), z: p.z.round() }
}

fn dist(ctx: &SkillContext, x: f64, y: f64, z: f64) -> i64 {
    let p = ctx.bot.position();
    let (dx, dy, dz) = (p.x - x, p.y - y, p.z - z);
    (dx * dx + dy * dy + dz * dz).sqrt().round() as i64
}

fn arg(input: &Value, key: &str) -> String {
    input.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn ledger_line(status: LedgerStatus, text: &str) -> String {
    let s = match status {
        LedgerStatus::Todo => "todo",
        LedgerStatus::Doing => "doing",
        LedgerStatus::Done => "done",
    };
    format!("[{s}] {text}")
}

struct SaveWaypoint;
#[async_trait]
impl Skill for SaveWaypoint {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "save_waypoint".into(),
            description: "Record the bot's current position under a name for later recall (use \"base\" for a home base).".into(),
            input_schema: json!({ "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let name = arg(&input, "name");
        let p = pos(ctx);
        ctx.memory.set_waypoint(&ctx.scope(), &name, p);
        format!("saved waypoint \"{name}\" at ({}, {}, {})", p.x, p.y, p.z)
    }
}

struct GotoWaypoint;
#[async_trait]
impl Skill for GotoWaypoint {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "goto_waypoint".into(),
            description: "Pathfind to a previously saved waypoint by name.".into(),
            input_schema: json!({ "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let name = arg(&input, "name");
        let scope = ctx.scope();
        let wp = match ctx.memory.get_waypoint(&scope, &name) {
            Some(wp) => wp,
            None => {
                let names: Vec<String> =
                    ctx.memory.list_waypoints(&scope).into_iter().map(|(n, _)| n).collect();
                let known = if names.is_empty() { "none".into() } else { names.join(", ") };
                return format!("no waypoint \"{name}\" (known: {known})");
            }
        };
        // TODO(verify): RadiusGoal(Vec3, radius) + PathfinderClientExt::goto; goto has no Result (GoalNear(_,1) = radius 1).
        let goal = azalea::pathfinder::goals::RadiusGoal::new(azalea::Vec3::new(wp.x, wp.y, wp.z), 1.0);
        ctx.bot.goto(goal).await;
        format!("arrived at \"{name}\" ({}, {}, {})", wp.x, wp.y, wp.z)
    }
}

struct ListWaypoints;
#[async_trait]
impl Skill for ListWaypoints {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "list_waypoints".into(),
            description: "List saved waypoints with coordinates and distance from the bot.".into(),
            input_schema: json!({ "type": "object", "properties": {}, "required": [] }),
        }
    }
    async fn run(&self, ctx: &SkillContext, _input: Value) -> String {
        let wps = ctx.memory.list_waypoints(&ctx.scope());
        if wps.is_empty() {
            return "none saved".into();
        }
        wps.iter()
            .map(|(n, p)| format!("{n}: ({}, {}, {}) {}m", p.x, p.y, p.z, dist(ctx, p.x, p.y, p.z)))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

struct RememberNote;
#[async_trait]
impl Skill for RememberNote {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "remember_note".into(),
            description: "Store a freeform note or learning under a key for later recall.".into(),
            input_schema: json!({ "type": "object", "properties": { "key": { "type": "string" }, "text": { "type": "string" } }, "required": ["key", "text"] }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let key = arg(&input, "key");
        ctx.memory.set_note(&ctx.scope(), &key, &arg(&input, "text"));
        format!("noted \"{key}\"")
    }
}

struct RecallNotes;
#[async_trait]
impl Skill for RecallNotes {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "recall_notes".into(),
            description: "Recall stored notes, optionally filtered by a query (empty string returns all).".into(),
            input_schema: json!({ "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let q = arg(&input, "query");
        let query = if q.is_empty() { None } else { Some(q.as_str()) };
        let notes = ctx.memory.list_notes(&ctx.scope(), query);
        if notes.is_empty() {
            return "no matching notes".into();
        }
        notes.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n")
    }
}

struct UpdateLedger;
#[async_trait]
impl Skill for UpdateLedger {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "update_ledger".into(),
            description: "Set the status (todo|doing|done) of a ledger item, adding it if new.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "item": { "type": "string" },
                    "status": { "type": "string", "enum": ["todo", "doing", "done"] }
                },
                "required": ["item", "status"]
            }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let status = match arg(&input, "status").as_str() {
            "doing" => LedgerStatus::Doing,
            "done" => LedgerStatus::Done,
            _ => LedgerStatus::Todo,
        };
        let ledger = ctx.memory.set_ledger_item(&ctx.scope(), &arg(&input, "item"), status);
        if ledger.is_empty() {
            return "ledger empty".into();
        }
        ledger.iter().map(|i| ledger_line(i.status, &i.text)).collect::<Vec<_>>().join("; ")
    }
}

struct ReadLedger;
#[async_trait]
impl Skill for ReadLedger {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "read_ledger".into(),
            description: "Read the current task ledger.".into(),
            input_schema: json!({ "type": "object", "properties": {}, "required": [] }),
        }
    }
    async fn run(&self, ctx: &SkillContext, _input: Value) -> String {
        let ledger = ctx.memory.ledger(&ctx.scope());
        if ledger.is_empty() {
            return "ledger empty".into();
        }
        ledger.iter().map(|i| ledger_line(i.status, &i.text)).collect::<Vec<_>>().join("\n")
    }
}

pub fn skills() -> Vec<Arc<dyn Skill>> {
    vec![
        Arc::new(SaveWaypoint),
        Arc::new(GotoWaypoint),
        Arc::new(ListWaypoints),
        Arc::new(RememberNote),
        Arc::new(RecallNotes),
        Arc::new(UpdateLedger),
        Arc::new(ReadLedger),
    ]
}
