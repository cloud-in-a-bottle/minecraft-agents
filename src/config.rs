//! App configuration (port of config.ts). Env parsing with the same defaults/fallbacks.
use crate::types::{AppConfig, AuthMode, BotSpec, Effort, LlmConfig, McConfig};
use anyhow::{anyhow, Result};
use std::path::Path;

/// Required env with optional fallback; empty string counts as unset (mirrors TS `env`).
fn env(name: &str, fallback: Option<&str>) -> Result<String> {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => fallback
            .map(str::to_string)
            .ok_or_else(|| anyhow!("missing required env var {name}")),
    }
}

/// Non-empty env value, else None (mirrors TS `||` fallthrough).
fn env_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

const HAIKU: &str = "claude-haiku-4-5";
const SONNET: &str = "claude-sonnet-5";
const LUNA: &str = "gpt-5.6-luna";

/// Canonical selectable planner models (for the dashboard dropdown).
pub const MODELS: &[&str] = &[HAIKU, SONNET, LUNA];

/// Resolve a model alias to its canonical id. Accepts haiku/sonnet/luna/5.6-luna and full ids.
pub fn normalize_model(m: &str) -> Result<String> {
    let v = match m.trim().to_lowercase().as_str() {
        "haiku" | "claude-haiku-4-5" => HAIKU,
        "sonnet" | "claude-sonnet-5" => SONNET,
        "luna" | "5.6-luna" | "gpt-5.6-luna" => LUNA,
        _ => return Err(anyhow!(
            "model \"{m}\" not allowed; use claude-haiku-4-5 (haiku), claude-sonnet-5 (sonnet), or gpt-5.6-luna (luna)"
        )),
    };
    Ok(v.to_string())
}

fn parse_effort(s: &str) -> Effort {
    match s {
        "medium" => Effort::Medium,
        "high" => Effort::High,
        "xhigh" => Effort::Xhigh,
        "max" => Effort::Max,
        _ => Effort::Low,
    }
}

#[derive(serde::Deserialize)]
struct BotSpecJson {
    goal: Option<String>,
    model: Option<String>,
}

/// Resolve the roster. Every agent is numbered agent_1..agent_N — the only naming scheme.
fn resolve_bots() -> Result<Vec<BotSpec>> {
    if let Some(path) = env_opt("BOTS_CONFIG") {
        let text = std::fs::read_to_string(&path)?;
        let specs: Vec<BotSpecJson> = serde_json::from_str(&text)?;
        if specs.is_empty() {
            return Err(anyhow!("BOTS_CONFIG must be a non-empty JSON array of {{ goal?, model? }}"));
        }
        return specs
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                let model = match s.model {
                    Some(m) => Some(normalize_model(&m)?),
                    None => None,
                };
                Ok(BotSpec { username: format!("agent_{}", i + 1), goal: s.goal, model })
            })
            .collect();
    }
    let count: usize = env("BOT_COUNT", Some("0"))?.parse()?;
    Ok((0..count)
        .map(|i| BotSpec { username: format!("agent_{}", i + 1), goal: None, model: None })
        .collect())
}

pub fn load_config() -> Result<AppConfig> {
    let view_distance = env_opt("MC_VIEW_DISTANCE").map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let mc = McConfig {
        host: env("MC_HOST", Some("localhost"))?,
        port: env("MC_PORT", Some("25565"))?.parse()?,
        version: env_opt("MC_VERSION"),
        auth: if env("MC_AUTH", Some("offline"))? == "microsoft" { AuthMode::Microsoft } else { AuthMode::Offline },
        login_message: std::env::var("LOGIN_MESSAGE").unwrap_or_default(),
        view_distance,
        chunk_keep_radius: env("CHUNK_KEEP_RADIUS", Some("4"))?.parse()?,
    };
    let llm = LlmConfig {
        api_key: std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
        openai_api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
        model: normalize_model(&env("LLM_MODEL", Some(HAIKU))?)?,
        effort: parse_effort(&env("LLM_EFFORT", Some("low"))?),
        max_steps: env("LLM_MAX_STEPS", Some("40"))?.parse()?,
    };
    let command_allowlist = std::env::var("COMMAND_ALLOWLIST")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let data_dir = env_opt("OPENHOST_APP_DATA_DIR")
        .or_else(|| env_opt("DATA_DIR"))
        .unwrap_or_else(|| "./data".to_string());
    let join = |name: &str| Path::new(&data_dir).join(name).to_string_lossy().into_owned();
    Ok(AppConfig {
        port: env("PORT", Some("8080"))?.parse()?,
        db_path: env_opt("DB_PATH").unwrap_or_else(|| join("minecraft-agents.db")),
        library_dir: env_opt("LIBRARY_DIR").unwrap_or_else(|| join("library")),
        mc,
        llm,
        bots: resolve_bots()?,
        dispatcher_name: env("DISPATCHER_NAME", Some("agents"))?,
        command_allowlist,
        max_bots: env("MAX_BOTS", Some("20"))?.parse()?,
        max_per_user: env("MAX_PER_USER", Some("5"))?.parse()?,
        dispatcher_recycle_ms: env("DISPATCHER_RECYCLE_MIN", Some("45"))?.parse::<u64>()? * 60_000,
    })
}
