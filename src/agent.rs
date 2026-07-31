//! Worker lifecycle + LLM planning loop (port of agent.ts). Connects, pursues one goal via a
//! Claude/OpenAI planning loop over the fixed skill library, then logs out. Reusable via assign().

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use azalea::entity::metadata::Health;
use azalea::entity::{EntityKindComponent, Position};
use azalea::local_player::Hunger;
use azalea::ecs as bevy_ecs; // satisfies #[derive(Component)]'s bevy_ecs path
use azalea::prelude::Component;
use azalea::registry::builtin::EntityKind;
use azalea::{Account, Client, ClientBuilder, ClientInformation, Event, Vec3};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use crate::llm::{planner_for, ContentBlock, Message, PlanRequest, PlanResponse, Planner};
use crate::manager::{PlannerKeys, SkillEnv, TeleportFn};
use crate::mc::{
    kick_reason, log_line, nearest_hostile, socket_bytes, start_chunk_prune, Auth, McDataImpl,
    Reconnector,
};
use crate::skills::{base, McData, SelfInfo, SkillContext};
use crate::types::{AgentState, AgentStatus, AuthMode, BotSpec, Effort, LlmConfig, McConfig, Pos};

const SYSTEM: &str = r#"You control a single Minecraft bot through a fixed set of skills (tools).
Pursue the assigned GOAL by calling one skill at a time and reading the result and the CURRENT STATE that follows each result.
Rules:
- Decompose the goal into short, concrete steps. Long-horizon plans fail; act, observe, adjust.
- Before each tool call, output one short sentence saying what you're doing and why.
- Never invent coordinates — use find_blocks to locate things before moving or mining.
- go_to and collect_block only path reliably within ~32 blocks. For anything farther, close the gap in stages with go_toward (a cardinal direction or a block type), then act.
- If a skill returns an error, try a different concrete approach rather than repeating it.
- You can only talk to your owner and to fellow agents owned by them: use "message" to reach one, "message_team" to reach all your teammates. There is no public chat.
- Owner messages appear as OWNER:, teammate messages as AGENT <name>:, and damage as a "took N damage" note — respond to these.
- Routines are how your work gets reused — players mostly want a saved, replayable procedure, not a one-off. Whenever a goal is repeatable, author one instead of doing ad-hoc steps.
- To build one: check list_routines first (the library is shared across all agents and owners — reuse or extend an existing routine), otherwise save_routine then run_routine. Parameterize everything variable with {param} placeholders (block, count, direction, coordinates) and give it a clear name + description — that is how players find and rerun it. Cover the whole job in one routine (locate, gather, craft, deposit), and make it robust with until/when/repeat loops and stop_on_error (grammar is in save_routine).
- To react automatically to conditions (low food/health, etc.), create a setting once with create_setting (e.g. food<14 -> collect and eat food); it runs on its own until you delete it.
- Call task_complete as soon as the goal is met or is clearly impossible."#;

const KEEP_FULL: usize = 4; // recent steps keep full results; older collapse to summaries
const STABLE_MS: u64 = 20_000;

fn agent_re() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^agent_\d+$").unwrap())
}

fn effort_str(e: Effort) -> String {
    match e {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::Xhigh => "xhigh",
        Effort::Max => "max",
    }
    .to_string()
}

/// Client-info view distance from the config string ("tiny".."far" or a chunk count).
fn view_chunks(v: &Option<String>) -> u8 {
    match v.as_deref() {
        Some("tiny") => 2,
        Some("short") => 4,
        Some("far") => 12,
        Some(s) => s.parse().unwrap_or(8),
        None => 8,
    }
}

// --- on-wire byte accounting (uncompressed JSON, mirroring JSON.stringify().length) ---

fn block_json(b: &ContentBlock) -> Value {
    match b {
        ContentBlock::Text { text } => json!({ "type": "text", "text": text }),
        ContentBlock::ToolUse { id, name, input } => {
            json!({ "type": "tool_use", "id": id, "name": name, "input": input })
        }
        ContentBlock::ToolResult { tool_use_id, content } => {
            json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": content })
        }
    }
}

fn request_bytes(req: &PlanRequest) -> u64 {
    let v = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "system": req.system,
        "tools": req.tools.iter().map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.input_schema })).collect::<Vec<_>>(),
        "messages": req.messages.iter().map(|m| json!({ "role": m.role, "content": m.content.iter().map(block_json).collect::<Vec<_>>() })).collect::<Vec<_>>(),
        "effort": req.effort,
        "thinking_disabled": req.thinking_disabled,
    });
    v.to_string().len() as u64
}

fn response_bytes(res: &PlanResponse) -> u64 {
    let v = json!({
        "content": res.content.iter().map(block_json).collect::<Vec<_>>(),
        "usage": {
            "input_tokens": res.usage.input_tokens,
            "output_tokens": res.usage.output_tokens,
            "cache_read_input_tokens": res.usage.cache_read_input_tokens,
        },
    });
    v.to_string().len() as u64
}

// --- per-step planning history ---

struct ResultRec {
    id: String,
    name: String,
    content: String,
}
struct Step {
    assistant: Vec<ContentBlock>,
    results: Vec<ResultRec>,
    collapsed: bool,
}

// --- shared interior-mutable state (azalea handler task + planning-loop task) ---

struct AgentShared {
    spec: BotSpec,
    mc: Arc<Mutex<McConfig>>,
    llm: Arc<Mutex<LlmConfig>>,
    keys: PlannerKeys,
    teleport: TeleportFn,
    env: SkillEnv,
    mc_data: Arc<dyn McData>,

    state: Mutex<AgentState>,
    goal: Mutex<Option<String>>,
    owner: Mutex<Option<String>>,
    planner: Mutex<Option<Arc<dyn Planner>>>,
    bot: Mutex<Option<Client>>,
    auth: Mutex<Option<Auth>>,
    behaviors: Arc<Mutex<HashSet<String>>>,
    injected: Mutex<Vec<String>>,
    log: Mutex<Vec<String>>,
    note_sink: Mutex<Option<Arc<dyn Fn(&str) + Send + Sync>>>,

    prune: Mutex<Option<JoinHandle<()>>>,
    behavior_handle: Mutex<Option<JoinHandle<()>>>,
    reconnector: Reconnector,

    step: AtomicU32,
    conv_steps: AtomicUsize,
    tokens_in: AtomicU64,
    tokens_out: AtomicU64,
    cache_read: AtomicU64,
    api_in: AtomicU64,
    api_out: AtomicU64,
    mc_in_base: AtomicU64,
    mc_out_base: AtomicU64,
    last_health: Mutex<f32>,

    stopped: AtomicBool,
    looping: AtomicBool,
    teleport_on_spawn: AtomicBool,
    effort_ok: AtomicBool,
    thinking_ok: AtomicBool,
    generation: AtomicU64,
    weak: Mutex<Weak<AgentShared>>,
}

impl AgentShared {
    fn note(&self, msg: &str) {
        log_line(&mut self.log.lock(), msg);
    }
    fn note_fn(&self) -> Arc<dyn Fn(&str) + Send + Sync> {
        self.note_sink.lock().clone().expect("note sink set")
    }
    fn set_state(&self, s: AgentState) {
        *self.state.lock() = s;
    }
    fn owner(&self) -> Option<String> {
        self.owner.lock().clone()
    }
}

/// azalea handler state carried through the Event handler (supersession via generation).
#[derive(Clone, Default, Component)]
pub struct WorkerState {
    shared: Option<Arc<AgentShared>>,
    generation: u64,
}

/// A worker bot: connects, pursues one goal, then logs out. Reusable via assign().
pub struct Agent {
    shared: Arc<AgentShared>,
}

impl Agent {
    pub fn new(
        spec: BotSpec,
        mc: Arc<Mutex<McConfig>>,
        llm: Arc<Mutex<LlmConfig>>,
        owner: Option<String>,
        env: SkillEnv,
        keys: PlannerKeys,
        teleport: TeleportFn,
    ) -> Agent {
        let goal = spec.goal.clone();
        let model = spec.model.clone().unwrap_or_else(|| llm.lock().model.clone());
        let planner = planner_for(&model, &keys.anthropic, &keys.openai).ok();
        let behaviors: HashSet<String> =
            ["defend".to_string(), "auto_eat".to_string()].into_iter().collect();

        let shared = Arc::new_cyclic(|weak: &Weak<AgentShared>| {
            let w = weak.clone();
            let on_reconnect: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                if let Some(s) = w.upgrade() {
                    connect(&s);
                }
            });
            let w = weak.clone();
            let on_give_up: Arc<dyn Fn(u32) + Send + Sync> = Arc::new(move |n| {
                if let Some(s) = w.upgrade() {
                    s.set_state(AgentState::Error);
                    s.note(&format!("gave up reconnecting after {n} attempts (check host/login)"));
                }
            });
            AgentShared {
                spec,
                mc,
                llm,
                keys,
                teleport,
                env,
                mc_data: Arc::new(McDataImpl::new()),
                state: Mutex::new(AgentState::Connecting),
                goal: Mutex::new(goal.clone()),
                owner: Mutex::new(owner),
                planner: Mutex::new(planner),
                bot: Mutex::new(None),
                auth: Mutex::new(None),
                behaviors: Arc::new(Mutex::new(behaviors)),
                injected: Mutex::new(Vec::new()),
                log: Mutex::new(Vec::new()),
                note_sink: Mutex::new(None),
                prune: Mutex::new(None),
                behavior_handle: Mutex::new(None),
                reconnector: Reconnector::new(4, on_reconnect, on_give_up),
                step: AtomicU32::new(0),
                conv_steps: AtomicUsize::new(0),
                tokens_in: AtomicU64::new(0),
                tokens_out: AtomicU64::new(0),
                cache_read: AtomicU64::new(0),
                api_in: AtomicU64::new(0),
                api_out: AtomicU64::new(0),
                mc_in_base: AtomicU64::new(0),
                mc_out_base: AtomicU64::new(0),
                last_health: Mutex::new(20.0),
                stopped: AtomicBool::new(false),
                looping: AtomicBool::new(false),
                teleport_on_spawn: AtomicBool::new(goal.is_some()),
                effort_ok: AtomicBool::new(true),
                thinking_ok: AtomicBool::new(true),
                generation: AtomicU64::new(0),
                weak: Mutex::new(Weak::new()),
            }
        });
        *shared.weak.lock() = Arc::downgrade(&shared);
        // note sink logs into this agent's ring buffer.
        let w = Arc::downgrade(&shared);
        *shared.note_sink.lock() = Some(Arc::new(move |m: &str| {
            if let Some(s) = w.upgrade() {
                s.note(m);
            }
        }));
        Agent { shared }
    }

    pub fn start(&self) {
        connect(&self.shared);
    }

    /// Position for the peer API.
    pub fn position(&self) -> Option<Pos> {
        let bot = self.shared.bot.lock().clone()?;
        let p = bot.get_component::<Position>()?;
        let v: Vec3 = *p;
        Some(Pos { x: v.x, y: v.y, z: v.z })
    }

    /// Deliver an external message (from another agent) into the planning loop.
    pub fn inject(&self, message: &str) {
        self.shared.injected.lock().push(message.to_string());
        self.shared.note(&format!("inbox: {message}"));
    }

    /// (Re)assign a goal. Reconnects if logged out. Rejected (false) while busy.
    pub fn assign(&self, goal: &str) -> bool {
        let s = &self.shared;
        if s.looping.load(Ordering::Relaxed) {
            s.note(&format!("assign rejected (busy): {goal}"));
            return false;
        }
        *s.goal.lock() = Some(goal.to_string());
        s.note(&format!("goal: {goal}"));
        let state = *s.state.lock();
        if state == AgentState::Idle {
            spawn_loop(s.clone());
        } else if state == AgentState::Stopped || state == AgentState::Error {
            s.stopped.store(false, Ordering::Relaxed);
            s.teleport_on_spawn.store(true, Ordering::Relaxed);
            reset_reconnector(s);
            connect(s);
        }
        true
    }

    pub fn chat(&self, message: &str) {
        if let Some(bot) = self.shared.bot.lock().as_ref() {
            bot.chat(message.to_string());
        }
    }

    pub fn stop(&self) {
        let s = &self.shared;
        s.stopped.store(true, Ordering::Relaxed);
        s.set_state(AgentState::Stopped);
        reset_reconnector(s);
        if let Some(bot) = s.bot.lock().as_ref() {
            bot.disconnect();
        }
        s.note("stopped");
    }

    /// Reserve this identity without connecting (used to claim an unspawned number).
    pub fn mark_offline(&self) {
        let s = &self.shared;
        s.stopped.store(true, Ordering::Relaxed);
        s.set_state(AgentState::Stopped);
        reset_reconnector(s);
    }

    pub fn is_online(&self) -> bool {
        *self.shared.state.lock() != AgentState::Stopped
    }

    /// Reconnect an idle bot (to pick up a new host/login). Skips busy/stopped ones.
    pub fn reconnect(&self) {
        let s = &self.shared;
        if *s.state.lock() == AgentState::Stopped || s.looping.load(Ordering::Relaxed) {
            return;
        }
        reset_reconnector(s);
        connect(s);
    }

    pub fn owner(&self) -> Option<String> {
        self.shared.owner()
    }

    pub fn set_owner(&self, owner: Option<String>) {
        *self.shared.owner.lock() = owner;
    }

    pub fn status(&self) -> AgentStatus {
        let s = &self.shared;
        let bot = s.bot.lock().clone();
        let (si, so) = bot.as_ref().map(socket_bytes).unwrap_or((0, 0));
        let health = bot.as_ref().and_then(|b| b.get_component::<Health>()).map(|h| *h);
        let food = bot.as_ref().and_then(|b| b.get_component::<Hunger>()).map(|h| h.food as f32);
        let position = bot
            .as_ref()
            .and_then(|b| b.get_component::<Position>())
            .map(|p| {
                let v: Vec3 = *p;
                Pos { x: v.x.round(), y: v.y.round(), z: v.z.round() }
            });
        AgentStatus {
            username: s.spec.username.clone(),
            owner: s.owner(),
            state: *s.state.lock(),
            goal: s.goal.lock().clone(),
            step: s.step.load(Ordering::Relaxed),
            conv_steps: s.conv_steps.load(Ordering::Relaxed),
            tokens_in: s.tokens_in.load(Ordering::Relaxed),
            tokens_out: s.tokens_out.load(Ordering::Relaxed),
            cache_read_tokens: s.cache_read.load(Ordering::Relaxed),
            net_in: s.mc_in_base.load(Ordering::Relaxed) + si + s.api_in.load(Ordering::Relaxed),
            net_out: s.mc_out_base.load(Ordering::Relaxed) + so + s.api_out.load(Ordering::Relaxed),
            health,
            food,
            position,
            log: s.log.lock().clone(),
        }
    }
}

// --- reconnector helpers (Reconnector is not Clone; keep it behind the Mutex) ---

fn reset_reconnector(s: &Arc<AgentShared>) {
    s.reconnector.reset();
}

fn make_ctx(s: &Arc<AgentShared>, bot: Client) -> SkillContext {
    SkillContext {
        bot,
        mc_data: s.mc_data.clone(),
        memory: s.env.memory.clone(),
        peers: s.env.peers.clone(),
        routines: s.env.routines.clone(),
        rules: s.env.rules.clone(),
        self_: SelfInfo { username: s.spec.username.clone(), owner: s.owner() },
        behaviors: s.behaviors.clone(),
        note: s.note_fn(),
    }
}

/// Perception + durable memory summary, fed to the planner each step.
fn state_text(s: &Arc<AgentShared>, ctx: &SkillContext) -> String {
    let ob = base::observe(ctx);
    let mem = s.env.memory.summary(&ctx.scope());
    if mem.is_empty() {
        ob
    } else {
        format!("{ob}\n{mem}")
    }
}

// --- (re)connect: fold prior socket bytes, spawn the azalea client, drive from the handler ---

fn connect(shared: &Arc<AgentShared>) {
    shared.stopped.store(false, Ordering::Relaxed);
    shared.set_state(AgentState::Connecting);
    shared.reconnector.cancel_pending();

    let old = shared.bot.lock().take();
    let (pi, po) = old.as_ref().map(socket_bytes).unwrap_or((0, 0));
    shared.mc_in_base.fetch_add(pi, Ordering::Relaxed);
    shared.mc_out_base.fetch_add(po, Ordering::Relaxed);

    let generation = shared.generation.fetch_add(1, Ordering::Relaxed) + 1;
    if let Some(h) = shared.prune.lock().take() {
        h.abort();
    }
    if let Some(h) = shared.behavior_handle.lock().take() {
        h.abort();
    }

    // Fresh auth state per connection; login line read live from the config.
    let get_message = {
        let mc = shared.mc.clone();
        Arc::new(move || mc.lock().login_message.clone()) as Arc<dyn Fn() -> String + Send + Sync>
    };
    *shared.auth.lock() = Some(Auth::new(get_message, shared.note_fn()));

    let (host, port, auth_mode, version) = {
        let mc = shared.mc.lock();
        (mc.host.clone(), mc.port, mc.auth, mc.version.clone())
    };
    let _ = version; // TODO(verify): azalea negotiates the version; no explicit override here.
    let username = shared.spec.username.clone();
    let state = WorkerState { shared: Some(shared.clone()), generation };
    crate::mc::spawn_client(move || async move {
        let account = match auth_mode {
            AuthMode::Offline => Account::offline(&username),
            // TODO(verify): Microsoft session auth is async + cached; falls back to offline on failure.
            AuthMode::Microsoft => match Account::microsoft(&username).await {
                Ok(a) => a,
                Err(_) => Account::offline(&username),
            },
        };
        let addr = format!("{host}:{port}");
        // reconnect_after(None): our Reconnector drives reconnection, not azalea's built-in.
        let _ = ClientBuilder::new()
            .set_handler(handle)
            .set_state(state)
            .reconnect_after(None)
            .start(account, addr.as_str())
            .await;
    });

    if let Some(old) = old {
        old.disconnect(); // its Disconnect carries a stale generation and is ignored
    }
}

async fn handle(bot: Client, event: Event, state: WorkerState) -> anyhow::Result<()> {
    let shared = match &state.shared {
        Some(s) => s,
        None => return Ok(()),
    };
    let gen = state.generation;
    let current = || gen == shared.generation.load(Ordering::Relaxed);

    match event {
        Event::Init => {
            let vd = view_chunks(&shared.mc.lock().view_distance);
            bot.set_client_information(ClientInformation { view_distance: vd, ..Default::default() });
        }
        // Login fires pre-spawn; login plugins may hold us here until authenticated.
        Event::Login => {
            let auth = shared.auth.lock().clone();
            if let Some(a) = auth {
                a.on_login(&bot);
            }
        }
        Event::Spawn => {
            if !current() {
                return Ok(());
            }
            *shared.bot.lock() = Some(bot.clone());
            *shared.last_health.lock() = bot.get_component::<Health>().map(|h| *h).unwrap_or(20.0);
            shared.behaviors.lock().insert("defend".into());
            shared.behaviors.lock().insert("auto_eat".into());
            shared.set_state(AgentState::Idle);
            shared.reconnector.mark_connected(STABLE_MS);
            shared.note(&format!("spawned as {}", shared.spec.username));

            let keep = shared.mc.lock().chunk_keep_radius;
            *shared.prune.lock() = Some(start_chunk_prune(bot.clone(), keep, 45000));

            // eat-when-hungry + defend-when-attacked, registered behaviors, and bot-authored rules.
            let sh = shared.clone();
            let is_friendly: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(move |name: &str| {
                sh.owner().as_deref() == Some(name) || agent_re().is_match(name)
            });
            let ctx = make_ctx(shared, bot.clone());
            *shared.behavior_handle.lock() = Some(base::install_auto_behaviors(ctx, is_friendly));

            // Authenticate first, then act — starting the loop before login lands gets the bot kicked.
            let after_auth =
                if shared.mc.lock().login_message.is_empty() { 0 } else { 3000 };
            let owner = shared.owner();
            let tp = shared.teleport_on_spawn.swap(false, Ordering::Relaxed);
            // Freshly summoned/tasked: the dispatcher (op) brings it to its owner. Skipped on reconnects.
            if tp {
                if let Some(o) = owner.clone() {
                    if o != "api" {
                        let s = shared.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(after_auth)).await;
                            if let Some(o) = s.owner() {
                                (s.teleport)(&s.spec.username, &o);
                            }
                        });
                    }
                }
            }
            if shared.goal.lock().is_some() {
                let s = shared.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(after_auth)).await;
                    run_loop(s).await;
                });
            }
        }
        Event::Chat(packet) => {
            let full = packet.message().to_string();
            let s = full.trim();
            if !s.is_empty() {
                let trunc: String = s.chars().take(180).collect();
                shared.note(&format!("srv: {trunc}"));
            }
            let auth = shared.auth.lock().clone();
            if let Some(a) = auth {
                a.on_chat(&bot, s);
            }
            if let Some(user) = packet.sender() {
                let content = packet.content();
                if packet.is_whisper() {
                    on_whisper(shared, &user, content.trim());
                } else {
                    on_owner_chat(shared, &user, content.trim());
                }
            }
        }
        // Poll for a health drop each tick (mineflayer's "health" event has no azalea analog).
        Event::Tick => {
            if current() {
                on_health_change(shared, &bot);
            }
        }
        Event::Disconnect(reason) => {
            if let Some(r) = &reason {
                shared.note(&format!("kicked: {}", kick_reason(r)));
            }
            if !current() {
                return Ok(()); // superseded by a newer connection; ignore
            }
            if let Some(h) = shared.prune.lock().take() {
                h.abort();
            }
            if let Some(h) = shared.behavior_handle.lock().take() {
                h.abort();
            }
            if shared.stopped.load(Ordering::Relaxed) {
                return Ok(());
            }
            shared.set_state(AgentState::Connecting);
            let should = {
                let w = shared.weak.lock().clone();
                Arc::new(move || {
                    w.upgrade()
                        .map(|s| {
                            !s.stopped.load(Ordering::Relaxed)
                                && s.generation.load(Ordering::Relaxed) == gen
                        })
                        .unwrap_or(false)
                }) as Arc<dyn Fn() -> bool + Send + Sync>
            };
            let delay = shared.reconnector.schedule_reconnect(should);
            if delay > 0 {
                shared.note(&format!("disconnected; reconnecting in {}s", delay / 1000));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Notify the planner when the bot loses health, naming a nearby hostile if any.
fn on_health_change(shared: &Arc<AgentShared>, bot: &Client) {
    let h = match bot.get_component::<Health>() {
        Some(h) => *h,
        None => return,
    };
    let mut last = shared.last_health.lock();
    if h < *last {
        let dmg = (*last - h).round() as i64;
        let from = match nearest_hostile(bot, Some(8.0)) {
            Some(e) => format!(" (hostile nearby: {})", hostile_name(bot, e)),
            None => String::new(),
        };
        drop(last);
        let msg = format!("took {dmg} damage, health now {}/20{from}", h.round() as i64);
        shared.injected.lock().push(msg.clone());
        shared.note(&format!("inbox: {msg}"));
        *shared.last_health.lock() = h;
    } else {
        *last = h;
    }
}

fn hostile_name(bot: &Client, entity: azalea::ecs::entity::Entity) -> String {
    // TODO(verify): entity_component::<EntityKindComponent> + deref to EntityKind.
    let comp = bot.entity_component::<EntityKindComponent>(entity);
    let kind: EntityKind = *comp;
    let s = kind.to_string();
    s.strip_prefix("minecraft:").unwrap_or(&s).to_string()
}

/// Owner-only in-game prompt: `@agent_N <msg>` while it's online.
fn on_owner_chat(shared: &Arc<AgentShared>, username: &str, message: &str) {
    let owner = shared.owner();
    if owner.as_deref() != Some(username) {
        return;
    }
    let escaped = regex::escape(&shared.spec.username);
    let re = Regex::new(&format!(r"(?i)^@{escaped}\b[\s:,-]*(.*)$")).unwrap();
    if let Some(c) = re.captures(message.trim()) {
        let msg = c[1].trim();
        if !msg.is_empty() {
            steer(shared, msg);
        }
    }
}

/// Owner-only whisper (`/msg agent_N <msg>`).
fn on_whisper(shared: &Arc<AgentShared>, username: &str, message: &str) {
    if shared.owner().as_deref() != Some(username) {
        return;
    }
    let msg = message.trim();
    if !msg.is_empty() {
        steer(shared, msg);
    }
}

/// Steer a running task (inject a prompt) or, if idle, start it as a new goal.
fn steer(shared: &Arc<AgentShared>, message: &str) {
    if shared.looping.load(Ordering::Relaxed) {
        shared.injected.lock().push(format!("OWNER: {message}"));
        shared.note(&format!("owner prompt queued: {message}"));
    } else if let Some(agent) = upgrade_agent(shared) {
        agent.assign(message);
    }
}

fn upgrade_agent(shared: &Arc<AgentShared>) -> Option<Agent> {
    shared.weak.lock().upgrade().map(|shared| Agent { shared })
}

fn spawn_loop(shared: Arc<AgentShared>) {
    tokio::spawn(run_loop(shared));
}

// --- planning loop (faithful to agent.ts runLoop) ---

async fn run_loop(shared: Arc<AgentShared>) {
    if shared.looping.load(Ordering::Relaxed) || shared.bot.lock().is_none() {
        return;
    }
    let goal = match shared.goal.lock().clone() {
        Some(g) => g,
        None => return,
    };
    shared.looping.store(true, Ordering::Relaxed);
    shared.set_state(AgentState::Working);
    shared.step.store(0, Ordering::Relaxed);
    shared.conv_steps.store(0, Ordering::Relaxed);

    let model = shared.spec.model.clone().unwrap_or_else(|| shared.llm.lock().model.clone());
    // re-pick so a live model change applies next task.
    match planner_for(&model, &shared.keys.anthropic, &shared.keys.openai) {
        Ok(p) => *shared.planner.lock() = Some(p),
        Err(e) => {
            shared.note(&format!("loop error: {e}"));
            shared.looping.store(false, Ordering::Relaxed);
            shared.set_state(AgentState::Idle);
            return;
        }
    }

    let result = step_loop(&shared, &model, &goal).await;
    if let Err(e) = result {
        shared.note(&format!("loop error: {e}"));
    }
    shared.looping.store(false, Ordering::Relaxed);
    logout(&shared);
}

async fn step_loop(shared: &Arc<AgentShared>, model: &str, goal: &str) -> anyhow::Result<()> {
    let mut history: Vec<Step> = Vec::new();

    loop {
        if shared.stopped.load(Ordering::Relaxed) {
            break;
        }
        let max_steps = shared.llm.lock().max_steps;
        if shared.step.load(Ordering::Relaxed) >= max_steps {
            break;
        }
        shared.step.fetch_add(1, Ordering::Relaxed);

        let bot = match shared.bot.lock().clone() {
            Some(b) => b,
            None => break,
        };
        let ctx = make_ctx(shared, bot);
        let messages = build_messages(shared, goal, &history, &ctx);

        let res = plan(shared, model, messages).await?;
        shared.tokens_in.fetch_add(res.usage.input_tokens, Ordering::Relaxed);
        shared.tokens_out.fetch_add(res.usage.output_tokens, Ordering::Relaxed);
        shared.cache_read.fetch_add(res.usage.cache_read_input_tokens, Ordering::Relaxed);

        for b in &res.content {
            if let ContentBlock::Text { text } = b {
                let t = text.trim();
                if !t.is_empty() {
                    shared.note(&format!("thinks: {t}"));
                }
            }
        }
        let calls: Vec<(String, String, Value)> = res
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .collect();
        if calls.is_empty() {
            break;
        }

        if let Some((_, _, input)) = calls.iter().find(|c| c.1 == "task_complete") {
            let summary = input.get("summary").and_then(Value::as_str).unwrap_or("");
            shared.note(&format!("done: {summary}"));
            break;
        }

        let mut results = Vec::new();
        for (id, name, input) in &calls {
            let out = base::execute(&ctx, name, input.clone()).await;
            shared.note(&format!("{name} -> {out}"));
            results.push(ResultRec { id: id.clone(), name: name.clone(), content: out });
        }
        history.push(Step { assistant: res.content.clone(), results, collapsed: false });

        // Collapse steps that fell out of the full-keep window into their summaries.
        let older = history.len().saturating_sub(KEEP_FULL);
        for rec in history.iter_mut().take(older) {
            if rec.collapsed {
                continue;
            }
            for r in &mut rec.results {
                r.content = base::summarize_result(&r.name, &r.content);
            }
            rec.collapsed = true;
        }
        shared.conv_steps.store(history.len(), Ordering::Relaxed);
    }

    let max_steps = shared.llm.lock().max_steps;
    if shared.step.load(Ordering::Relaxed) >= max_steps {
        shared.note("stopped: step budget exhausted");
    }
    Ok(())
}

/// Rebuild the message thread each step: stable goal, compacted old results, fresh perception last.
fn build_messages(
    shared: &Arc<AgentShared>,
    goal: &str,
    history: &[Step],
    ctx: &SkillContext,
) -> Vec<Message> {
    let mut msgs = Vec::new();
    if history.is_empty() {
        msgs.push(user_text(format!("GOAL: {goal}\n\nCURRENT STATE:\n{}", state_text(shared, ctx))));
    } else {
        msgs.push(user_text(format!("GOAL: {goal}")));
        let last = history.len() - 1;
        for (i, rec) in history.iter().enumerate() {
            msgs.push(Message { role: "assistant".into(), content: rec.assistant.clone() });
            let mut content: Vec<ContentBlock> = rec
                .results
                .iter()
                .map(|r| ContentBlock::ToolResult {
                    tool_use_id: r.id.clone(),
                    content: r.content.clone(),
                })
                .collect();
            if i == last {
                content.push(ContentBlock::Text {
                    text: format!("CURRENT STATE:\n{}", state_text(shared, ctx)),
                });
            }
            msgs.push(Message { role: "user".into(), content });
        }
    }
    let drained: Vec<String> = shared.injected.lock().drain(..).collect();
    if !drained.is_empty() {
        msgs.push(user_text(drained.join("\n")));
    }
    msgs
}

fn user_text(text: String) -> Message {
    Message { role: "user".into(), content: vec![ContentBlock::Text { text }] }
}

// --- request build + self-heal (drop rejected effort/thinking params, retry once) ---

fn plan_request(shared: &Arc<AgentShared>, model: &str, messages: Vec<Message>) -> PlanRequest {
    let effort = if shared.effort_ok.load(Ordering::Relaxed) {
        Some(effort_str(shared.llm.lock().effort))
    } else {
        None
    };
    PlanRequest {
        model: model.to_string(),
        system: SYSTEM.to_string(),
        tools: base::tools(),
        messages,
        max_tokens: 1024,
        effort,
        thinking_disabled: !shared.thinking_ok.load(Ordering::Relaxed),
    }
}

async fn send(shared: &Arc<AgentShared>, req: PlanRequest) -> anyhow::Result<PlanResponse> {
    shared.api_out.fetch_add(request_bytes(&req), Ordering::Relaxed);
    let planner = shared
        .planner
        .lock()
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no planner configured"))?;
    let res = planner.create(&req).await?;
    shared.api_in.fetch_add(response_bytes(&res), Ordering::Relaxed);
    Ok(res)
}

async fn plan(
    shared: &Arc<AgentShared>,
    model: &str,
    messages: Vec<Message>,
) -> anyhow::Result<PlanResponse> {
    let req = plan_request(shared, model, messages.clone());
    match send(shared, req).await {
        Ok(r) => Ok(r),
        Err(err) => {
            let m = err.to_string().to_lowercase();
            let mut changed = false;
            if shared.effort_ok.load(Ordering::Relaxed)
                && (m.contains("effort") || m.contains("output_config"))
            {
                shared.effort_ok.store(false, Ordering::Relaxed);
                changed = true;
            }
            if shared.thinking_ok.load(Ordering::Relaxed) && m.contains("thinking") {
                shared.thinking_ok.store(false, Ordering::Relaxed);
                changed = true;
            }
            if changed {
                shared.note("dropping rejected params; retrying");
                let req = plan_request(shared, model, messages);
                return send(shared, req).await;
            }
            Err(err)
        }
    }
}

/// Disconnect on task completion/failure; the identity stays for owner reuse.
fn logout(shared: &Arc<AgentShared>) {
    shared.stopped.store(true, Ordering::Relaxed);
    shared.set_state(AgentState::Stopped);
    reset_reconnector(shared);
    if let Some(bot) = shared.bot.lock().as_ref() {
        bot.disconnect();
    }
    shared.note("task finished; logged out");
}
