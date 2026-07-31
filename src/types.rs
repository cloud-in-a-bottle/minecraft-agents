//! Shared types (port of types.ts). JSON field names must match the dashboard + persisted files.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    Offline,
    Microsoft,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Connecting,
    Idle,
    Working,
    Error,
    Stopped,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Pos {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Clone, Debug)]
pub struct McConfig {
    pub host: String,
    pub port: u16,
    pub version: Option<String>,
    pub auth: AuthMode,
    /// Sent in chat on spawn if non-empty (e.g. "/login <pw>"). Live-editable.
    pub login_message: String,
    /// Client view-distance ("tiny".."far" or a chunk count).
    pub view_distance: Option<String>,
    /// Drop loaded columns beyond this many chunks to cap the roaming world-copy leak.
    pub chunk_keep_radius: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

#[derive(Clone, Debug)]
pub struct LlmConfig {
    pub api_key: String,
    pub openai_api_key: String,
    pub model: String,
    pub effort: Effort,
    pub max_steps: u32,
}

/// One bot. `goal`/`model` override the shared defaults when present.
#[derive(Clone, Debug)]
pub struct BotSpec {
    pub username: String,
    pub goal: Option<String>,
    pub model: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub port: u16,
    pub db_path: String,
    pub library_dir: String,
    pub mc: McConfig,
    pub llm: LlmConfig,
    pub bots: Vec<BotSpec>,
    pub dispatcher_name: String,
    pub command_allowlist: Vec<String>,
    pub max_bots: usize,
    pub max_per_user: usize,
    pub dispatcher_recycle_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    AtCapacity,
    UserLimit,
}

#[derive(Clone, Debug, Default)]
pub struct CreateResult {
    pub created: Vec<String>,
    pub rejected: usize,
    pub reason: Option<RejectReason>,
}

#[derive(Clone, Debug)]
pub struct Skipped {
    pub name: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub struct BatchResult {
    pub done: Vec<String>,
    pub skipped: Vec<Skipped>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub username: String,
    pub owner: Option<String>,
    pub state: AgentState,
    pub goal: Option<String>,
    pub step: u32,
    pub conv_steps: usize,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read_tokens: u64,
    pub net_in: u64,
    pub net_out: u64,
    pub health: Option<f32>,
    pub food: Option<f32>,
    pub position: Option<Pos>,
    pub log: Vec<String>,
}

/// Dispatcher status row (GET /dispatcher). camelCase for the dashboard.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatcherStatus {
    pub username: String,
    pub online: bool,
    pub net_in: u64,
    pub net_out: u64,
    pub log: Vec<String>,
}

/// Live settings shown/edited in the dashboard (GET/POST /config). camelCase.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub max_bots: usize,
    pub max_per_user: usize,
    pub mc_host: String,
    pub mc_port: u16,
    pub login_message: String,
    pub model: String,
    pub models: Vec<String>,
    pub max_steps: u32,
}

/// Partial settings update (POST /config). Any field may be absent.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SettingsPatch {
    pub max_per_user: Option<usize>,
    pub mc_host: Option<String>,
    pub mc_port: Option<u16>,
    pub login_message: Option<String>,
    pub model: Option<String>,
    pub max_steps: Option<u32>,
}

/// Outcome of retasking an existing agent (POST /bots/:name/goal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOutcome {
    NotFound,
    Busy,
    Ok,
}
