//! Fleet manager (port of manager.ts). Owns the dispatcher + every worker and exposes the
//! facade api.rs consumes. Shared as `Arc<BotManager>`; all methods take `&self`.
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock, Weak};

use anyhow::Result;
use parking_lot::Mutex;

use crate::agent::Agent; // TODO(verify): concurrent module; methods assumed &self w/ interior mutability
use crate::config::{normalize_model, MODELS};
use crate::dispatcher::{DispatchHandlers, Dispatcher}; // TODO(verify): concurrent module; trait shape assumed
use crate::library::{FileRoutineStore, FileRuleStore};
use crate::skill::{Memory, OwnerLookup, PeerApi, RoutineStore, RuleStore};
use crate::store::Store;
use crate::types::{
    AgentStatus, AppConfig, AssignOutcome, BatchResult, BotSpec, CreateResult, DispatcherStatus,
    LlmConfig, McConfig, Pos, RejectReason, Settings, SettingsPatch, Skipped,
};

/// Planner keys shared by the fleet. // TODO(verify): agent.rs consumes this shape.
#[derive(Clone)]
pub struct PlannerKeys {
    pub anthropic: String,
    pub openai: String,
}

/// The skill environment handed to each worker (mirrors TS `SkillEnv`).
/// TODO(verify): agent.rs constructs its SkillContext from these Arcs.
#[derive(Clone)]
pub struct SkillEnv {
    pub memory: Arc<dyn Memory>,
    pub peers: Arc<dyn PeerApi>,
    pub routines: Arc<dyn RoutineStore>,
    pub rules: Arc<dyn RuleStore>,
}

/// Op-teleport callback (dispatcher brings a worker to a player). TODO(verify): agent.rs param type.
pub type TeleportFn = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// Per-spawn outcome (TS `SpawnResult`).
enum SpawnResult {
    Ok(String),
    Rejected(RejectReason),
}

/// Fleet-mutable state behind one lock (single-threaded in TS; interior-mutable here).
struct Fleet {
    agents: HashMap<String, Arc<Agent>>,
    next_number: usize,
    max_per_user: usize,
}

/// All shared manager state. Impls `PeerApi` + `DispatchHandlers`; wrapped in an Arc cycle
/// (env.peers / dispatcher handlers point back here) that lives for the process.
struct Inner {
    max_bots: usize,
    bots: Vec<BotSpec>,
    // live, shared by reference with every worker so settings edits reach them on reconnect.
    mc: Arc<Mutex<McConfig>>,
    llm: Arc<Mutex<LlmConfig>>,
    keys: PlannerKeys,
    store: Arc<Store>,
    routines: Arc<FileRoutineStore>,
    rules: Arc<FileRuleStore>,
    state: Mutex<Fleet>,
    dispatcher: OnceLock<Arc<Dispatcher>>,
    weak: OnceLock<Weak<Inner>>,
}

fn online_count(agents: &HashMap<String, Arc<Agent>>) -> usize {
    agents.values().filter(|a| a.is_online()).count()
}

fn owned_online_count(agents: &HashMap<String, Arc<Agent>>, owner: &str) -> usize {
    agents
        .values()
        .filter(|a| a.owner().as_deref() == Some(owner) && a.is_online())
        .count()
}

/// `agent_N` → N. Empty suffix reads as 0 (TS `Number("")`), non-numeric skips.
fn parse_agent_number(name: &str) -> Option<usize> {
    let rest = name.strip_prefix("agent_")?;
    if rest.is_empty() {
        Some(0)
    } else {
        rest.parse().ok()
    }
}

/// Durable settings override the env-var seed (TS `loadSettings`).
fn load_settings(config: &mut AppConfig, store: &Store) {
    if let Some(h) = store.get_setting("mcHost").filter(|s| !s.is_empty()) {
        config.mc.host = h;
    }
    if let Some(p) = store.get_setting("mcPort").filter(|s| !s.is_empty()) {
        if let Ok(n) = p.parse() {
            config.mc.port = n;
        }
    }
    if let Some(l) = store.get_setting("loginMessage") {
        config.mc.login_message = l;
    }
    if let Some(c) = store.get_setting("maxPerUser") {
        if let Ok(n) = c.parse() {
            config.max_per_user = n;
        }
    }
    if let Some(m) = store.get_setting("model").filter(|s| !s.is_empty()) {
        if let Ok(m) = normalize_model(&m) {
            config.llm.model = m;
        }
    }
    if let Some(s) = store.get_setting("maxSteps") {
        if let Ok(n) = s.parse() {
            config.llm.max_steps = n;
        }
    }
}

impl Inner {
    fn arc(&self) -> Arc<Inner> {
        self.weak.get().expect("weak self set").upgrade().expect("inner alive")
    }

    fn dispatcher(&self) -> &Arc<Dispatcher> {
        self.dispatcher.get().expect("dispatcher initialized")
    }

    /// Per-user cap applies to real players, not the admin HTTP channel.
    fn over_user_cap(&self, fleet: &Fleet, owner: Option<&str>) -> bool {
        match owner {
            Some(o) if o != "api" => {
                fleet.max_per_user > 0 && owned_online_count(&fleet.agents, o) >= fleet.max_per_user
            }
            _ => false,
        }
    }

    /// Construct a worker (shared live config + env) and register it. Does not start it.
    fn create(self: &Arc<Self>, fleet: &mut Fleet, spec: BotSpec, owner: Option<String>) -> Arc<Agent> {
        let username = spec.username.clone();
        let env = SkillEnv {
            memory: self.store.clone(),
            peers: Arc::clone(self) as Arc<dyn PeerApi>,
            routines: self.routines.clone(),
            rules: self.rules.clone(),
        };
        let disp = self.dispatcher().clone();
        let teleport: TeleportFn = Arc::new(move |name: &str, target: &str| disp.teleport(name, target));
        // TODO(verify): Agent::new signature/return type (assumed value, wrapped in Arc here).
        let agent = Arc::new(Agent::new(
            spec,
            self.mc.clone(),
            self.llm.clone(),
            owner,
            env,
            self.keys.clone(),
            teleport,
        ));
        fleet.agents.insert(username, agent.clone());
        agent
    }

    /// Set an agent's owner and persist it (ownership is written on any change).
    fn set_owner_persist(&self, agent: &Arc<Agent>, name: &str, owner: Option<&str>) {
        agent.set_owner(owner.map(str::to_string)); // TODO(verify): Agent::set_owner
        self.store.set_owner(name, owner);
    }

    fn spawn(self: &Arc<Self>, fleet: &mut Fleet, goal: Option<&str>, owner: Option<&str>) -> SpawnResult {
        if self.over_user_cap(fleet, owner) {
            return SpawnResult::Rejected(RejectReason::UserLimit);
        }
        if online_count(&fleet.agents) >= self.max_bots {
            return SpawnResult::Rejected(RejectReason::AtCapacity);
        }
        let mut username = format!("agent_{}", fleet.next_number);
        while fleet.agents.contains_key(&username) {
            fleet.next_number += 1;
            username = format!("agent_{}", fleet.next_number);
        }
        let spec = BotSpec { username: username.clone(), goal: goal.map(str::to_string), model: None };
        let agent = self.create(fleet, spec, owner.map(str::to_string));
        agent.start();
        self.store.set_owner(&username, owner);
        fleet.next_number += 1;
        SpawnResult::Ok(username)
    }

    /// `new [n] <task>` — n fresh workers on one goal, owned by the caller.
    fn create_new(self: &Arc<Self>, count: usize, goal: &str, owner: Option<&str>) -> CreateResult {
        let mut fleet = self.state.lock();
        let mut created = Vec::new();
        let mut rejected = 0usize;
        let mut reason = None;
        for _ in 0..count {
            match self.spawn(&mut fleet, Some(goal), owner) {
                SpawnResult::Ok(u) => created.push(u),
                SpawnResult::Rejected(r) => {
                    rejected += 1;
                    reason = Some(r);
                }
            }
        }
        CreateResult { created, rejected, reason }
    }

    /// `x[, y] <task>` — retask existing workers the caller owns.
    fn assign_existing(self: &Arc<Self>, numbers: &[u32], goal: &str, owner: &str) -> BatchResult {
        let mut done = Vec::new();
        let mut skipped = Vec::new();
        let fleet = self.state.lock();
        for n in numbers {
            let name = format!("agent_{n}");
            let a = fleet.agents.get(&name).cloned();
            match a {
                None => skipped.push(Skipped { name, reason: "unknown".into() }),
                Some(a) if a.owner().as_deref() != Some(owner) => {
                    skipped.push(Skipped { name, reason: "not_owner".into() })
                }
                Some(a) if !a.is_online() && self.over_user_cap(&fleet, Some(owner)) => {
                    skipped.push(Skipped { name, reason: "user_limit".into() })
                }
                Some(a) if !a.is_online() && online_count(&fleet.agents) >= self.max_bots => {
                    skipped.push(Skipped { name, reason: "at_capacity".into() })
                }
                Some(a) if !a.assign(goal) => skipped.push(Skipped { name, reason: "busy".into() }),
                Some(_) => done.push(name),
            }
        }
        BatchResult { done, skipped }
    }

    /// `free x[, y]` — the owner relinquishes ownership (becomes claimable).
    fn free(self: &Arc<Self>, numbers: &[u32], owner: &str) -> BatchResult {
        let mut done = Vec::new();
        let mut skipped = Vec::new();
        let fleet = self.state.lock();
        for n in numbers {
            let name = format!("agent_{n}");
            match fleet.agents.get(&name).cloned() {
                None => skipped.push(Skipped { name, reason: "unknown".into() }),
                Some(a) if a.owner().as_deref() != Some(owner) => {
                    skipped.push(Skipped { name, reason: "not_owner".into() })
                }
                Some(a) => {
                    self.set_owner_persist(&a, &name, None);
                    done.push(name);
                }
            }
        }
        BatchResult { done, skipped }
    }

    /// `claim x[, y]` — take an unowned number (creating it offline if new).
    fn claim(self: &Arc<Self>, numbers: &[u32], owner: &str) -> BatchResult {
        let mut done = Vec::new();
        let mut skipped = Vec::new();
        let mut fleet = self.state.lock();
        for n in numbers {
            let name = format!("agent_{n}");
            match fleet.agents.get(&name).cloned() {
                None => {
                    let spec = BotSpec { username: name.clone(), goal: None, model: None };
                    let agent = self.create(&mut fleet, spec, Some(owner.to_string()));
                    agent.mark_offline();
                    self.store.set_owner(&name, Some(owner));
                    fleet.next_number = fleet.next_number.max(*n as usize + 1);
                    done.push(name);
                }
                Some(a) => {
                    let o = a.owner();
                    if o.is_none() || o.as_deref() == Some(owner) {
                        self.set_owner_persist(&a, &name, Some(owner));
                        done.push(name);
                    } else {
                        skipped.push(Skipped { name, reason: "owned_by_other".into() });
                    }
                }
            }
        }
        BatchResult { done, skipped }
    }

    /// `quit x[, y]` — immediately disconnect workers the caller owns, even mid-task.
    fn quit(self: &Arc<Self>, numbers: &[u32], owner: &str) -> BatchResult {
        let mut done = Vec::new();
        let mut skipped = Vec::new();
        let fleet = self.state.lock();
        for n in numbers {
            let name = format!("agent_{n}");
            match fleet.agents.get(&name).cloned() {
                None => skipped.push(Skipped { name, reason: "unknown".into() }),
                Some(a) if a.owner().as_deref() != Some(owner) => {
                    skipped.push(Skipped { name, reason: "not_owner".into() })
                }
                Some(a) => {
                    a.stop();
                    done.push(name);
                }
            }
        }
        BatchResult { done, skipped }
    }

    /// `give x[, y] <player>` — transfer ownership of your workers to another player.
    fn give(self: &Arc<Self>, numbers: &[u32], owner: &str, target: &str) -> BatchResult {
        let mut done = Vec::new();
        let mut skipped = Vec::new();
        let fleet = self.state.lock();
        for n in numbers {
            let name = format!("agent_{n}");
            match fleet.agents.get(&name).cloned() {
                None => skipped.push(Skipped { name, reason: "unknown".into() }),
                Some(a) if a.owner().as_deref() != Some(owner) => {
                    skipped.push(Skipped { name, reason: "not_owner".into() })
                }
                Some(a) => {
                    self.set_owner_persist(&a, &name, Some(target));
                    done.push(name);
                }
            }
        }
        BatchResult { done, skipped }
    }

    /// Recreate persisted owned numbers as offline placeholders so ownership survives restarts.
    fn restore_ownership(self: &Arc<Self>) {
        let rows = self.store.all_agents();
        let mut fleet = self.state.lock();
        for (username, owner) in rows {
            if let Some(existing) = fleet.agents.get(&username) {
                existing.set_owner(owner); // TODO(verify): Agent::set_owner (no persist; already from store)
                continue;
            }
            let spec = BotSpec { username: username.clone(), goal: None, model: None };
            let agent = self.create(&mut fleet, spec, owner);
            agent.mark_offline();
            if let Some(n) = parse_agent_number(&username) {
                fleet.next_number = fleet.next_number.max(n + 1);
            }
        }
    }

    fn wipe_agents(&self) -> usize {
        let mut fleet = self.state.lock();
        let removed = fleet.agents.len();
        for a in fleet.agents.values() {
            a.stop();
        }
        fleet.agents.clear();
        self.store.wipe_agent_data();
        fleet.next_number = 1;
        removed
    }
}

impl PeerApi for Inner {
    fn position(&self, name: &str) -> Option<Pos> {
        self.state.lock().agents.get(name).and_then(|a| a.position())
    }
    fn online(&self, name: &str) -> bool {
        self.state.lock().agents.get(name).map_or(false, |a| a.is_online())
    }
    fn send(&self, to: &str, from: &str, message: &str) -> bool {
        let a = self.state.lock().agents.get(to).cloned();
        match a {
            Some(a) if a.is_online() => {
                a.inject(&format!("AGENT {from}: {message}"));
                true
            }
            _ => false,
        }
    }
    fn owner_of(&self, name: &str) -> OwnerLookup {
        self.state.lock().agents.get(name).map(|a| a.owner())
    }
    fn teammates(&self, owner: Option<&str>) -> Vec<String> {
        match owner {
            None => Vec::new(),
            Some(o) => self
                .state
                .lock()
                .agents
                .iter()
                .filter(|(_, a)| a.owner().as_deref() == Some(o) && a.is_online())
                .map(|(n, _)| n.clone())
                .collect(),
        }
    }
    fn summon(&self, count: usize, goal: &str, owner: Option<&str>) -> CreateResult {
        self.arc().create_new(count, goal, owner)
    }
}

// Dispatcher command grammar handlers. Owner is always the commanding player here.
impl DispatchHandlers for Inner {
    fn create_new(&self, count: usize, goal: &str, owner: &str) -> CreateResult {
        self.arc().create_new(count, goal, Some(owner))
    }
    fn assign_existing(&self, numbers: &[u32], goal: &str, owner: &str) -> BatchResult {
        self.arc().assign_existing(numbers, goal, owner)
    }
    fn free(&self, numbers: &[u32], owner: &str) -> BatchResult {
        self.arc().free(numbers, owner)
    }
    fn claim(&self, numbers: &[u32], owner: &str) -> BatchResult {
        self.arc().claim(numbers, owner)
    }
    fn quit(&self, numbers: &[u32], owner: &str) -> BatchResult {
        self.arc().quit(numbers, owner)
    }
    fn give(&self, numbers: &[u32], owner: &str, target: &str) -> BatchResult {
        self.arc().give(numbers, owner, target)
    }
}

/// Owns the dispatcher and every worker; handles creation, ownership, and reuse.
pub struct BotManager {
    inner: Arc<Inner>,
}

impl BotManager {
    pub fn new(config: AppConfig, store: Arc<Store>) -> BotManager {
        let mut config = config;
        load_settings(&mut config, &store); // durable settings override the env-var seed
        let keys = PlannerKeys { anthropic: config.llm.api_key.clone(), openai: config.llm.openai_api_key.clone() };
        // One shared library for the whole fleet: routines/ and settings/ as JSON files (not the DB).
        let lib = Path::new(&config.library_dir);
        let routines = Arc::new(FileRoutineStore::new(lib.join("routines")));
        let rules = Arc::new(FileRuleStore::new(lib.join("settings")));
        let inner = Arc::new(Inner {
            max_bots: config.max_bots,
            bots: config.bots.clone(),
            mc: Arc::new(Mutex::new(config.mc.clone())),
            llm: Arc::new(Mutex::new(config.llm.clone())),
            keys,
            store,
            routines,
            rules,
            state: Mutex::new(Fleet { agents: HashMap::new(), next_number: 0, max_per_user: config.max_per_user }),
            dispatcher: OnceLock::new(),
            weak: OnceLock::new(),
        });
        inner.weak.set(Arc::downgrade(&inner)).ok();
        // Dispatcher holds the handlers (this manager); teleport callbacks reference it.
        // TODO(verify): Dispatcher::new signature (shared live mc; handlers as Arc<dyn DispatchHandlers>).
        let dispatcher = Arc::new(Dispatcher::new(
            config.dispatcher_name.clone(),
            inner.mc.clone(),
            config.command_allowlist.clone(),
            inner.clone() as Arc<dyn DispatchHandlers>,
        ));
        inner.dispatcher.set(dispatcher).ok();
        {
            let mut fleet = inner.state.lock();
            for spec in inner.bots.iter() {
                inner.create(&mut fleet, spec.clone(), None);
            }
            fleet.next_number = inner.bots.len() + 1;
        }
        inner.restore_ownership();
        BotManager { inner }
    }

    pub fn start_all(&self) {
        let inner = &self.inner;
        inner.dispatcher().start();
        let agents: Vec<Arc<Agent>> = inner.state.lock().agents.values().cloned().collect();
        for a in agents {
            a.start();
        }
    }

    // ---- facade (api.rs consumes exactly these) ----

    pub fn list(&self) -> Vec<AgentStatus> {
        self.inner.state.lock().agents.values().map(|a| a.status()).collect()
    }

    pub fn status(&self, name: &str) -> Option<AgentStatus> {
        self.inner.state.lock().agents.get(name).map(|a| a.status())
    }

    pub fn dispatcher_status(&self) -> DispatcherStatus {
        self.inner.dispatcher().status()
    }

    /// Live settings shown/edited in the dashboard.
    pub fn get_settings(&self) -> Settings {
        let (mc_host, mc_port, login_message) = {
            let mc = self.inner.mc.lock();
            (mc.host.clone(), mc.port, mc.login_message.clone())
        };
        let (model, max_steps) = {
            let llm = self.inner.llm.lock();
            (llm.model.clone(), llm.max_steps)
        };
        Settings {
            max_bots: self.inner.max_bots,
            max_per_user: self.inner.state.lock().max_per_user,
            mc_host,
            mc_port,
            login_message,
            model,
            models: MODELS.iter().map(|s| s.to_string()).collect(),
            max_steps,
        }
    }

    /// Apply a live settings patch, persisting each change. Host/port/login reconnect the fleet;
    /// model/step-budget/cap apply to each worker's next (or in-flight) task.
    pub fn update_settings(&self, patch: SettingsPatch) -> Result<()> {
        let inner = &self.inner;
        if let Some(mpu) = patch.max_per_user {
            inner.state.lock().max_per_user = mpu;
            inner.store.set_setting("maxPerUser", &mpu.to_string());
        }
        if let Some(model) = patch.model.as_deref() {
            let model = normalize_model(model)?; // Err on an unknown model
            inner.llm.lock().model = model.clone();
            inner.store.set_setting("model", &model);
        }
        if let Some(ms) = patch.max_steps {
            let ms = ms.clamp(1, 1000);
            inner.llm.lock().max_steps = ms;
            inner.store.set_setting("maxSteps", &ms.to_string());
        }
        let mut reconnect = false;
        {
            let mut mc = inner.mc.lock();
            if let Some(h) = patch.mc_host.as_deref() {
                if h != mc.host {
                    mc.host = h.to_string();
                    inner.store.set_setting("mcHost", h);
                    reconnect = true;
                }
            }
            if let Some(p) = patch.mc_port {
                if p != mc.port {
                    mc.port = p;
                    inner.store.set_setting("mcPort", &p.to_string());
                    reconnect = true;
                }
            }
            if let Some(l) = patch.login_message.as_deref() {
                if l != mc.login_message {
                    mc.login_message = l.to_string();
                    inner.store.set_setting("loginMessage", l);
                    reconnect = true;
                }
            }
        }
        if reconnect {
            inner.dispatcher().reconnect();
            let agents: Vec<Arc<Agent>> = inner.state.lock().agents.values().cloned().collect();
            for a in agents {
                a.reconnect();
            }
        }
        Ok(())
    }

    pub fn create_new(&self, count: usize, goal: &str, owner: Option<&str>) -> CreateResult {
        self.inner.create_new(count, goal, owner)
    }

    pub fn assign(&self, name: &str, goal: &str) -> AssignOutcome {
        let a = self.inner.state.lock().agents.get(name).cloned();
        match a {
            None => AssignOutcome::NotFound,
            Some(a) => {
                if a.assign(goal) {
                    AssignOutcome::Ok
                } else {
                    AssignOutcome::Busy
                }
            }
        }
    }

    pub fn chat(&self, name: &str, message: &str) -> bool {
        let a = self.inner.state.lock().agents.get(name).cloned();
        match a {
            Some(a) => {
                a.chat(message);
                true
            }
            None => false,
        }
    }

    pub fn stop(&self, name: &str) -> bool {
        let a = self.inner.state.lock().agents.get(name).cloned();
        match a {
            Some(a) => {
                a.stop();
                true
            }
            None => false,
        }
    }

    /// Dev reset: disconnect and forget every worker + its memory. Keeps live settings and the
    /// shared library. Dispatcher stays online.
    pub fn wipe_agents(&self) -> usize {
        self.inner.wipe_agents()
    }
}
