//! Messaging skills (port of skills/messaging.ts): private message + team broadcast.
use crate::llm::ToolDef;
use crate::skills::{Skill, SkillContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

pub fn skills() -> Vec<Arc<dyn Skill>> {
    vec![Arc::new(Message), Arc::new(MessageTeam)]
}

struct Message;
#[async_trait]
impl Skill for Message {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "message".into(),
            description: "Privately message your owner (in-game /msg) or a teammate agent owned by the same player. The ONLY way this bot can talk to anyone.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "to": { "type": "string" }, "message": { "type": "string" } },
                "required": ["to", "message"], "additionalProperties": false,
            }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let to = input.get("to").and_then(Value::as_str).unwrap_or("");
        let msg = input.get("message").and_then(Value::as_str).unwrap_or("");
        let owner = ctx.self_.owner.as_deref();
        if let Some(owner) = owner {
            if to == owner {
                ctx.bot.chat(format!("/msg {owner} {msg}")); // TODO(verify): whisper via /msg command
                return format!("messaged owner {owner}");
            }
            if ctx.peers.owner_of(to) == Some(Some(owner.to_string())) {
                let ok = ctx.peers.send(to, &ctx.self_.username, msg);
                return if ok { format!("delivered to {to}") } else { format!("{to} is offline") };
            }
        }
        format!("can only message your owner or a teammate agent (owned by {})", owner.unwrap_or("nobody"))
    }
}

struct MessageTeam;
#[async_trait]
impl Skill for MessageTeam {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "message_team".into(),
            description: "Broadcast an in-process message to every fellow agent owned by the same player.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "message": { "type": "string" } },
                "required": ["message"], "additionalProperties": false,
            }),
        }
    }
    async fn run(&self, ctx: &SkillContext, input: Value) -> String {
        let msg = input.get("message").and_then(Value::as_str).unwrap_or("");
        let me = &ctx.self_.username;
        let mates: Vec<String> = ctx
            .peers
            .teammates(ctx.self_.owner.as_deref())
            .into_iter()
            .filter(|n| n != me)
            .collect();
        if mates.is_empty() {
            return "no teammates online".into();
        }
        let sent: Vec<String> = mates.into_iter().filter(|m| ctx.peers.send(m, me, msg)).collect();
        format!("sent to {} teammate(s): {}", sent.len(), sent.join(", "))
    }
}
