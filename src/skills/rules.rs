//! Reactive-setting skills (port of skills/rules.ts): create/list/toggle/delete rules.
use crate::llm::ToolDef;
use crate::routines::referenced_tools;
use crate::skill::{Rule, SHARED_SCOPE};
use crate::skills::{Skill, SkillContext};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};

const FORBIDDEN: &[&str] =
    &["save_routine", "task_complete", "create_setting", "delete_setting", "list_settings"];

pub fn skills() -> Vec<Arc<dyn Skill>> {
    vec![Arc::new(CreateSetting), Arc::new(ListSettings), Arc::new(ToggleSetting), Arc::new(DeleteSetting)]
}

fn cond_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^(.+?)(>=|<=|==|!=|>|<)(-?\d+)$").unwrap())
}

/// Valid if it matches <lhs><op><int> and lhs is have:/find:/health/food.
fn valid_condition(cond: &str) -> bool {
    match cond_re().captures(cond) {
        Some(c) => {
            let left = &c[1];
            left.starts_with("have:") || left.starts_with("find:") || left == "health" || left == "food"
        }
        None => false,
    }
}

struct CreateSetting;
#[async_trait]
impl Skill for CreateSetting {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "create_setting".into(),
            description: "Create a reactive rule: whenever condition holds, steps run automatically. \
Condition: have:<item><op>N, find:<block><op>N, health<op>N, food<op>N (op is >=,<=,>,<,==,!=). \
Steps use the SAME grammar as save_routine. Example: condition food<14 -> steps that collect and eat food.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "condition": { "type": "string" },
                    "steps": { "type": "array", "items": { "type": "object" } },
                },
                "required": ["name", "condition", "steps"], "additionalProperties": false,
            }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let steps: Vec<Value> = input.get("steps").and_then(Value::as_array).cloned().unwrap_or_default();
        let name = input.get("name").and_then(Value::as_str).unwrap_or("");
        if name.is_empty() || steps.is_empty() {
            return "need a name and non-empty steps".into();
        }
        let condition: String =
            input.get("condition").and_then(Value::as_str).unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect();
        if !valid_condition(&condition) {
            return "condition must look like food<14, health<=6, have:cooked_beef>=1, or find:oak_log>0".into();
        }
        let refs = referenced_tools(&steps);
        let bad: Vec<String> = refs.iter().filter(|t| FORBIDDEN.contains(&t.as_str())).cloned().collect();
        if !bad.is_empty() {
            return format!("settings can't use: {}", bad.join(", "));
        }
        let rule = Rule { name: name.to_string(), condition: condition.clone(), steps, enabled: true };
        ctx.rules.save_rule(SHARED_SCOPE, rule.clone());
        format!("saved setting \"{}\": when {condition}, run {} skill(s)", rule.name, refs.len())
    }
}

struct ListSettings;
#[async_trait]
impl Skill for ListSettings {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "list_settings".into(),
            description: "List reactive settings (name, on/off, condition).".into(),
            input_schema: json!({ "type": "object", "properties": {}, "required": [], "additionalProperties": false }),
        }
    }
    async fn run(&self, ctx: &SkillContext, _input: Value) -> String {
        let rules = ctx.rules.list_rules(SHARED_SCOPE);
        if rules.is_empty() {
            return "no settings yet".into();
        }
        rules
            .iter()
            .map(|r| format!("{} [{}]: when {}", r.name, if r.enabled { "on" } else { "off" }, r.condition))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

struct ToggleSetting;
#[async_trait]
impl Skill for ToggleSetting {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "toggle_setting".into(),
            description: "Enable or disable a reactive setting by name.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "name": { "type": "string" }, "enabled": { "type": "boolean" } },
                "required": ["name", "enabled"], "additionalProperties": false,
            }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let name = input.get("name").and_then(Value::as_str).unwrap_or("");
        let rule = ctx.rules.list_rules(SHARED_SCOPE).into_iter().find(|r| r.name == name);
        let rule = match rule {
            Some(r) => r,
            None => return format!("no setting \"{name}\""),
        };
        let enabled = input.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        ctx.rules.save_rule(SHARED_SCOPE, Rule { enabled, ..rule });
        format!("setting \"{name}\" {}", if enabled { "on" } else { "off" })
    }
}

struct DeleteSetting;
#[async_trait]
impl Skill for DeleteSetting {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "delete_setting".into(),
            description: "Delete a reactive setting by name.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"], "additionalProperties": false,
            }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let name = input.get("name").and_then(Value::as_str).unwrap_or("");
        if ctx.rules.delete_rule(SHARED_SCOPE, name) {
            format!("deleted setting \"{name}\"")
        } else {
            format!("no setting \"{name}\"")
        }
    }
}
