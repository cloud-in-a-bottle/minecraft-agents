//! Presence skill (port of skills/presence.ts): who_online from the tab list.
use crate::llm::ToolDef;
use crate::skills::{Skill, SkillContext};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};

pub fn skills() -> Vec<Arc<dyn Skill>> {
    vec![Arc::new(WhoOnline)]
}

fn agent_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^agent_\d+$").unwrap())
}

struct WhoOnline;
#[async_trait]
impl Skill for WhoOnline {
    fn tool(&self) -> ToolDef {
        ToolDef {
            name: "who_online".into(),
            description: "List the players currently online from the bot's tab list, with each one's ping.".into(),
            input_schema: json!({ "type": "object", "properties": {}, "required": [], "additionalProperties": false }),
        }
    }
    async fn run(&self, ctx: &SkillContext, _input: Value) -> String {
        let me = &ctx.self_.username;
        // TODO(verify): tab_list() -> HashMap<Uuid, PlayerInfo>; profile.name + latency(ms).
        let mut entries: Vec<(String, String)> = ctx
            .bot
            .tab_list()
            .values()
            .map(|p| (p.profile.name.clone(), p.latency))
            .filter(|(name, _)| !name.is_empty() && name != me)
            .map(|(name, ping)| {
                let tag = if agent_re().is_match(&name) { " [agent]" } else { "" };
                let text = format!("{name} ({ping} ms){tag}");
                (name, text)
            })
            .collect();
        if entries.is_empty() {
            return "no other players online".into();
        }
        entries.sort_by(|a, b| natural_cmp(&a.0, &b.0));
        format!("{} online: {}", entries.len(), entries.iter().map(|e| e.1.clone()).collect::<Vec<_>>().join(", "))
    }
}

/// Case-insensitive, numeric-aware compare (mirrors JS localeCompare {numeric,base}).
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let xn: String = take_digits(&mut ai);
                let yn: String = take_digits(&mut bi);
                let ord = xn.parse::<u128>().unwrap_or(0).cmp(&yn.parse::<u128>().unwrap_or(0));
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

fn take_digits(it: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut s = String::new();
    while let Some(c) = it.peek().copied() {
        if c.is_ascii_digit() {
            s.push(c);
            it.next();
        } else {
            break;
        }
    }
    s
}
