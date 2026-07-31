//! Always-on command bot (port of dispatcher.ts). Players tag it in-game to manage
//! workers; it never moves. Auth/reconnect/chunk-prune/anti-afk run off the azalea Event handler.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use azalea::entity::Position;
use azalea::ecs as bevy_ecs; // satisfies #[derive(Component)]'s bevy_ecs path
use azalea::prelude::{BotClientExt, Component};
use azalea::{Account, Client, ClientBuilder, ClientInformation, Event, Vec3};
use parking_lot::Mutex;
use regex::Regex;
use tokio::task::JoinHandle;

use crate::mc::{kick_reason, log_line, socket_bytes, start_chunk_prune, Auth, Reconnector};
use crate::types::{
    AgentState, AgentStatus, AuthMode, BatchResult, CreateResult, DispatcherStatus, McConfig,
    RejectReason,
};

/// Manager-provided callbacks (port of the TS `DispatchHandlers` interface). manager.rs impls this.
pub trait DispatchHandlers: Send + Sync {
    fn create_new(&self, count: usize, goal: &str, owner: &str) -> CreateResult;
    fn assign_existing(&self, numbers: &[u32], goal: &str, owner: &str) -> BatchResult;
    fn free(&self, numbers: &[u32], owner: &str) -> BatchResult;
    fn claim(&self, numbers: &[u32], owner: &str) -> BatchResult;
    fn quit(&self, numbers: &[u32], owner: &str) -> BatchResult;
    fn give(&self, numbers: &[u32], owner: &str, target: &str) -> BatchResult;
    /// Agents the caller owns (online and offline), for the `list` command.
    fn list(&self, owner: &str) -> Vec<AgentStatus>;
}

/// Parse "1 2 agent_3" into numbers; commas tolerated.
fn parse_numbers(s: &str) -> Vec<u32> {
    let strip = agent_prefix();
    Regex::new(r"[\s,]+")
        .unwrap()
        .split(s)
        .map(|t| strip.replace(t, "").to_string())
        .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|t| t.parse().ok())
        .collect()
}

/// Parse a leading run of "1 2 agent_3" into numbers + the trailing text.
fn parse_leading_numbers(s: &str) -> (Vec<u32>, String) {
    let strip = agent_prefix();
    let toks: Vec<&str> = s.split_whitespace().collect();
    let mut numbers = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = strip.replace(toks[i].strip_suffix(',').unwrap_or(toks[i]), "").to_string();
        if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(n) = t.parse() {
                numbers.push(n);
            }
        } else {
            break;
        }
        i += 1;
    }
    (numbers, toks[i..].join(" "))
}

fn agent_prefix() -> Regex {
    Regex::new(r"^(?i)agent_").unwrap()
}

// --- azalea client state carried through the Event handler ---

type Shared = Arc<DispShared>;

#[derive(Clone, Default, Component)]
pub struct BotState {
    shared: Option<Shared>,
    generation: u64,
}

struct DispShared {
    username: String,
    allowlist: Vec<String>,
    handlers: Arc<dyn DispatchHandlers>,
    mc: Arc<Mutex<McConfig>>, // login_message read live on (re)connect; shared with manager
    log: Mutex<Vec<String>>,
    bot: Mutex<Option<Client>>,
    auth: Mutex<Option<Auth>>,
    afk: Mutex<Option<JoinHandle<()>>>,
    prune: Mutex<Option<JoinHandle<()>>>,
    reconnector: Reconnector,
    stopped: AtomicBool,
    online: AtomicBool,
    generation: AtomicU64,
    mc_in_base: AtomicU64,
    mc_out_base: AtomicU64,
}

impl DispShared {
    fn note(&self, msg: &str) {
        log_line(&mut self.log.lock(), msg);
    }

    /// All replies go back privately via `/msg <user> …` (never public chat).
    fn reply(&self, username: &str, msg: &str) {
        if let Some(bot) = self.bot.lock().as_ref() {
            bot.chat(format!("/msg {username} {msg}"));
        }
        self.note(&format!("{username}: {msg}"));
    }
}

/// Always-on, non-interactable (spectator, physics off, tiny view distance) player.
pub struct Dispatcher {
    shared: Shared,
}

impl Dispatcher {
    pub fn new(
        username: String,
        mc: Arc<Mutex<McConfig>>,
        allowlist: Vec<String>,
        handlers: Arc<dyn DispatchHandlers>,
    ) -> Self {
        let shared = Arc::new_cyclic(|weak: &Weak<DispShared>| {
            let w1 = weak.clone();
            let on_reconnect: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                if let Some(s) = w1.upgrade() {
                    connect(&s);
                }
            });
            let w2 = weak.clone();
            let on_give_up: Arc<dyn Fn(u32) + Send + Sync> = Arc::new(move |n| {
                if let Some(s) = w2.upgrade() {
                    s.note(&format!("gave up reconnecting after {n} attempts (check host/login)"));
                }
            });
            DispShared {
                username,
                allowlist,
                handlers,
                mc,
                log: Mutex::new(Vec::new()),
                bot: Mutex::new(None),
                auth: Mutex::new(None),
                afk: Mutex::new(None),
                prune: Mutex::new(None),
                reconnector: Reconnector::new(8, on_reconnect, on_give_up),
                stopped: AtomicBool::new(false),
                online: AtomicBool::new(false),
                generation: AtomicU64::new(0),
                mc_in_base: AtomicU64::new(0),
                mc_out_base: AtomicU64::new(0),
            }
        });
        Self { shared }
    }

    pub fn start(&self) {
        connect(&self.shared);
    }

    /// Op teleport (the dispatcher has op): bring a worker to a player.
    pub fn teleport(&self, agent: &str, target: &str) {
        if let Some(bot) = self.shared.bot.lock().as_ref() {
            bot.chat(format!("/tp {agent} {target}"));
        }
        self.shared.note(&format!("tp {agent} -> {target}"));
    }

    pub fn stop(&self) {
        self.shared.stopped.store(true, Ordering::Relaxed);
        self.shared.reconnector.reset();
        if let Some(bot) = self.shared.bot.lock().take() {
            bot.disconnect();
        }
    }

    /// Reconnect to pick up a new host/login. connect() tears down any existing connection.
    pub fn reconnect(&self) {
        self.shared.reconnector.reset();
        connect(&self.shared);
    }

    pub fn status(&self) -> DispatcherStatus {
        let (i, o) = self.shared.bot.lock().as_ref().map(socket_bytes).unwrap_or((0, 0));
        DispatcherStatus {
            username: self.shared.username.clone(),
            online: self.shared.online.load(Ordering::Relaxed),
            net_in: self.shared.mc_in_base.load(Ordering::Relaxed) + i,
            net_out: self.shared.mc_out_base.load(Ordering::Relaxed) + o,
            log: self.shared.log.lock().clone(),
        }
    }
}

/// (Re)connect the always-on bot. Bumps the generation so a superseded connection's events are ignored.
fn connect(shared: &Shared) {
    shared.stopped.store(false, Ordering::Relaxed);
    shared.reconnector.cancel_pending();

    let old = shared.bot.lock().take();
    let (pi, po) = old.as_ref().map(socket_bytes).unwrap_or((0, 0));
    shared.mc_in_base.fetch_add(pi, Ordering::Relaxed);
    shared.mc_out_base.fetch_add(po, Ordering::Relaxed);

    let generation = shared.generation.fetch_add(1, Ordering::Relaxed) + 1;
    if let Some(h) = shared.afk.lock().take() {
        h.abort();
    }
    if let Some(h) = shared.prune.lock().take() {
        h.abort();
    }
    shared.online.store(false, Ordering::Relaxed);

    // Fresh auth state per connection; login line read live from the config.
    let get_message = {
        let w = Arc::downgrade(shared);
        Arc::new(move || w.upgrade().map(|s| s.mc.lock().login_message.clone()).unwrap_or_default())
            as Arc<dyn Fn() -> String + Send + Sync>
    };
    let note = {
        let w = Arc::downgrade(shared);
        Arc::new(move |m: &str| {
            if let Some(s) = w.upgrade() {
                s.note(m);
            }
        }) as Arc<dyn Fn(&str) + Send + Sync>
    };
    *shared.auth.lock() = Some(Auth::new(get_message, note));

    let (host, port, auth_mode) = {
        let mc = shared.mc.lock();
        (mc.host.clone(), mc.port, mc.auth)
    };
    let username = shared.username.clone();
    let state = BotState { shared: Some(shared.clone()), generation };
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

/// Keep the idle spectator from being AFK-kicked: nudge its view periodically. TODO(verify).
fn anti_afk(bot: Client) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut t = 0f64;
        loop {
            tokio::time::sleep(Duration::from_millis(15000)).await;
            if bot.get_component::<Position>().is_none() {
                break; // disconnected
            }
            t += 0.1;
            let p = bot.position();
            bot.look_at(Vec3 { x: p.x + t.cos(), y: p.y, z: p.z + t.sin() });
        }
    })
}

async fn handle(bot: Client, event: Event, state: BotState) -> anyhow::Result<()> {
    let shared = match &state.shared {
        Some(s) => s,
        None => return Ok(()),
    };
    let gen = state.generation;
    let current = || gen == shared.generation.load(Ordering::Relaxed);

    match event {
        // Spectator needs only chat, not the world.
        Event::Init => {
            bot.set_client_information(ClientInformation { view_distance: 2, ..Default::default() });
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
            shared.online.store(true, Ordering::Relaxed);
            shared.reconnector.mark_connected(20000);
            shared.note("dispatcher online");
            *shared.afk.lock() = Some(anti_afk(bot.clone()));
            let keep = shared.mc.lock().chunk_keep_radius;
            *shared.prune.lock() = Some(start_chunk_prune(bot.clone(), keep, 45000));
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
                if packet.is_whisper() {
                    on_whisper(shared, &user, packet.content().trim());
                } else {
                    on_chat(shared, &user, &packet.content());
                }
            }
        }
        Event::Disconnect(reason) => {
            if let Some(r) = &reason {
                shared.note(&format!("kicked: {}", kick_reason(r)));
            }
            if !current() {
                return Ok(()); // superseded by a newer connection; ignore
            }
            shared.online.store(false, Ordering::Relaxed);
            if let Some(h) = shared.afk.lock().take() {
                h.abort();
            }
            if let Some(h) = shared.prune.lock().take() {
                h.abort();
            }
            if shared.stopped.load(Ordering::Relaxed) {
                return Ok(());
            }
            let should = {
                let w = Arc::downgrade(shared);
                Arc::new(move || {
                    w.upgrade()
                        .map(|s| !s.stopped.load(Ordering::Relaxed) && s.generation.load(Ordering::Relaxed) == gen)
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

/// Public `@agents <cmd>` (or the `@a` first-letter shorthand) in chat.
fn on_chat(shared: &Shared, username: &str, message: &str) {
    if username == shared.username {
        return;
    }
    let full = regex::escape(&shared.username);
    let first = regex::escape(&shared.username.chars().next().map(String::from).unwrap_or_default());
    let re = Regex::new(&format!(r"(?i)^@(?:{full}|{first})\b[\s:,-]*(.*)$")).unwrap();
    if let Some(c) = re.captures(message.trim()) {
        handle_command(shared, username, c[1].trim());
    }
}

/// Private `/msg agents <cmd>` — same grammar, no `@agents` prefix needed.
fn on_whisper(shared: &Shared, username: &str, message: &str) {
    if username == shared.username {
        return;
    }
    handle_command(shared, username, message.trim());
}

fn skipped_note(r: &BatchResult) -> String {
    if r.skipped.is_empty() {
        return String::new();
    }
    let items: Vec<String> = r.skipped.iter().map(|s| format!("{}: {}", s.name, s.reason)).collect();
    format!(" (skipped {})", items.join(", "))
}

/// Sort key from an `agent_N` username.
fn agent_number(username: &str) -> u32 {
    username.strip_prefix("agent_").and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// Human word for the list command (`Stopped` reads as offline/logged-out to players).
fn state_word(state: AgentState) -> &'static str {
    match state {
        AgentState::Connecting => "connecting",
        AgentState::Idle => "idle",
        AgentState::Working => "working",
        AgentState::Error => "error",
        AgentState::Stopped => "offline",
    }
}

fn done_or_nothing(done: &[String]) -> String {
    if done.is_empty() {
        "nothing".to_string()
    } else {
        done.join(", ")
    }
}

fn handle_command(shared: &Shared, username: &str, rest: &str) {
    if !shared.allowlist.is_empty() && !shared.allowlist.iter().any(|u| u == username) {
        shared.reply(username, "not allowed to command bots");
        return;
    }
    if rest.is_empty() {
        shared.reply(
            username,
            "usage: new [n] <task> | <n> [n…] <task> | list | free/claim/quit <n> [n…] | give <n> [n…] <player>",
        );
        return;
    }

    // Each keyword also accepts its first letter (n/l/f/c/q/g).
    let cmd = Regex::new(r"(?i)^(new|n|list|l|free|f|claim|c|quit|q|give|g)\b\s*(.*)$").unwrap().captures(rest);
    let keyword = cmd.as_ref().map(|c| match c[1].to_lowercase().as_str() {
        "n" => "new",
        "l" => "list",
        "f" => "free",
        "c" => "claim",
        "q" => "quit",
        "g" => "give",
        other => other,
    }
    .to_string());
    let args = cmd.as_ref().map(|c| c[2].trim().to_string()).unwrap_or_default();
    let u = &shared.username;

    match keyword.as_deref() {
        Some("new") => {
            let m = Regex::new(r"^(?:(\d+)\s+)?(.+)$").unwrap().captures(&args);
            let m = match m {
                Some(m) => m,
                None => {
                    shared.reply(username, &format!("usage: @{u} new [n] <task>"));
                    return;
                }
            };
            let count = m.get(1).and_then(|g| g.as_str().parse().ok()).unwrap_or(1usize);
            let goal = m[2].trim();
            let r = shared.handlers.create_new(count, goal, username);
            let limit =
                if r.reason == Some(RejectReason::UserLimit) { "your agent limit reached" } else { "agent limit reached" };
            let msg = if !r.created.is_empty() {
                let extra = if r.rejected > 0 { format!(" — {} not summoned ({limit})", r.rejected) } else { String::new() };
                format!("created {} on: {goal}{extra}", r.created.join(", "))
            } else {
                format!("cannot summon — {limit}")
            };
            shared.reply(username, &msg);
        }
        Some("list") => {
            let mut agents = shared.handlers.list(username);
            agents.sort_by_key(|a| agent_number(&a.username));
            if agents.is_empty() {
                shared.reply(username, "you don't own any agents");
                return;
            }
            let lines: Vec<String> = agents
                .iter()
                .map(|a| {
                    let n = a.username.strip_prefix("agent_").unwrap_or(&a.username);
                    match a.goal.as_deref().filter(|g| !g.is_empty()) {
                        Some(g) => format!("{n} [{}]: {g}", state_word(a.state)),
                        None => format!("{n} [{}]", state_word(a.state)),
                    }
                })
                .collect();
            shared.reply(username, &format!("your agents: {}", lines.join("; ")));
        }
        Some(k @ ("free" | "claim" | "quit")) => {
            let numbers = parse_numbers(&args);
            if numbers.is_empty() {
                shared.reply(username, &format!("usage: {k} <n> [n…]"));
                return;
            }
            let (verb, r) = match k {
                "free" => ("freed", shared.handlers.free(&numbers, username)),
                "quit" => ("quit", shared.handlers.quit(&numbers, username)),
                _ => ("claimed", shared.handlers.claim(&numbers, username)),
            };
            shared.reply(username, &format!("{verb} {}{}", done_or_nothing(&r.done), skipped_note(&r)));
        }
        Some("give") => {
            let m = Regex::new(r"^(.+?)\s+(\S+)$").unwrap().captures(&args);
            let m = match m {
                Some(m) => m,
                None => {
                    shared.reply(username, &format!("usage: @{u} give <n> [n…] <player>"));
                    return;
                }
            };
            let numbers = parse_numbers(&m[1]);
            let target = &m[2];
            if numbers.is_empty() {
                shared.reply(username, &format!("usage: @{u} give <n> [n…] <player>"));
                return;
            }
            let r = shared.handlers.give(&numbers, username, target);
            shared.reply(username, &format!("gave {} to {target}{}", done_or_nothing(&r.done), skipped_note(&r)));
        }
        _ => {
            // existing-agents task: "<n> [n…] <task>"
            let (numbers, goal) = parse_leading_numbers(rest);
            if numbers.is_empty() {
                shared.reply(username, &format!("unknown command \"{rest}\""));
                return;
            }
            if goal.is_empty() {
                shared.reply(username, "add a goal after the agent numbers");
                return;
            }
            let r = shared.handlers.assign_existing(&numbers, &goal, username);
            shared.reply(username, &format!("{} on: {goal}{}", done_or_nothing(&r.done), skipped_note(&r)));
        }
    }
}
