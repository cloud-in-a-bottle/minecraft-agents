//! azalea-layer helpers (port of deps.ts) + the `McData` registry facade.
//! Mineflayer's EventEmitter listeners become handler-driven: `Auth`, kick/reconnect,
//! chunk-prune, and reusable world/tool helpers the agent.rs Bevy handlers call.
//!
//! FINAL PUBLIC API (propagated to the other L4 agents):
//! ```ignore
//! // registry facade (crate::skills::McData impl)
//! pub struct McDataImpl;                    McDataImpl::new() -> Self
//! // enum <-> name helpers (azalea builtin registries)
//! pub fn block_kind_from_name(name: &str) -> Option<BlockKind>
//! pub fn item_kind_from_name(name: &str)  -> Option<ItemKind>
//! pub fn name_of_block_kind(k: BlockKind)  -> String    // bare, no "minecraft:"
//! pub fn name_of_item_kind(k: ItemKind)    -> String
//! pub fn is_hostile(kind: EntityKind) -> bool
//! // tool selection / combat / world scans (used by base.rs)
//! pub async fn equip_best_tool(bot: &Client, block: BlockState, require_harvest: bool) -> bool
//! pub fn nearest_hostile(bot: &Client, range: Option<f64>) -> Option<Entity>
//! pub fn find_blocks_near(bot: &Client, kind: BlockKind, max_distance: i32, count: usize) -> Vec<BlockPos>
//! pub fn nearest_block(bot: &Client, kind: BlockKind, max_distance: i32) -> Option<BlockPos>
//! pub async fn with_timeout<T>(fut, ms: u64, label: &str) -> anyhow::Result<T>
//! // connection lifecycle
//! pub struct Auth;      Auth::new(get_message, note); .on_login(&bot); .on_chat(&bot, line)
//! pub struct Reconnector;  new(max, on_reconnect, on_give_up); mark_connected(ms);
//!                          schedule_reconnect(should) -> u64; cancel_pending(); reset()
//! pub fn kick_reason(reason: &FormattedText) -> String
//! pub fn start_chunk_prune(bot: Client, keep_radius: i32, interval_ms: u64) -> tokio::task::JoinHandle<()>
//! pub fn socket_bytes(bot: &Client) -> (u64, u64)   // (0,0) — not exposed by azalea
//! pub fn log_line(log: &mut Vec<String>, msg: &str)
//! ```

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use azalea::block::{BlockState, BlockStates, BlockTrait};
use azalea::core::tier::get_item_tier;
use azalea::ecs::entity::Entity;
use azalea::ecs::query::Without;
use azalea::entity::inventory::Inventory;
use azalea::entity::{EntityKindComponent, LocalEntity, Position};
use azalea::inventory::ItemStack;
use azalea::registry::builtin::{BlockKind, EntityKind, ItemKind};
use azalea::registry::Registry;
use azalea::{BlockPos, Client, FormattedText, Vec3};
use parking_lot::Mutex;
use regex::Regex;

/// Sleep `ms` milliseconds (mineflayer `sleep`).
pub async fn sleep(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// Drive an azalea client on its own thread. Its `start` future holds a `LocalSet`
/// (`!Send`), so it can't ride a task on the shared multi-thread runtime.
pub fn spawn_client<Fut>(build: impl FnOnce() -> Fut + Send + 'static)
where
    Fut: Future + 'static,
{
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("azalea client runtime");
        rt.block_on(build());
    });
}

// --- registry name <-> enum helpers (replaces minecraft-data name lookups) ---

/// Parse a bare or `minecraft:`-prefixed name into a builtin registry enum.
pub fn block_kind_from_name(name: &str) -> Option<BlockKind> {
    name.parse::<BlockKind>().ok()
}
pub fn item_kind_from_name(name: &str) -> Option<ItemKind> {
    name.parse::<ItemKind>().ok()
}

/// Display renders `"minecraft:<id>"`; strip the namespace for the bare name.
pub fn name_of_block_kind(kind: BlockKind) -> String {
    strip_ns(&kind.to_string())
}
pub fn name_of_item_kind(kind: ItemKind) -> String {
    strip_ns(&kind.to_string())
}
fn strip_ns(s: &str) -> String {
    s.strip_prefix("minecraft:").unwrap_or(s).to_string()
}

/// Enumerate every valid id of a builtin registry (variants are contiguous from 0).
fn all_names<R: Registry + std::fmt::Display>() -> Vec<String> {
    let mut out = Vec::new();
    let mut id = 0u32;
    while let Some(v) = R::from_u32(id) {
        out.push(strip_ns(&v.to_string()));
        id += 1;
    }
    out
}

// --- McData: block/item registries, foods, hardness, harvest tools ---

pub struct McDataImpl {
    blocks: Vec<String>,
    items: Vec<String>,
}

impl Default for McDataImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl McDataImpl {
    pub fn new() -> Self {
        Self { blocks: all_names::<BlockKind>(), items: all_names::<ItemKind>() }
    }
}

impl crate::skills::McData for McDataImpl {
    fn block_id(&self, name: &str) -> Option<u32> {
        block_kind_from_name(name).map(|b| b.to_u32())
    }
    fn block_name(&self, id: u32) -> Option<String> {
        BlockKind::from_u32(id).map(name_of_block_kind)
    }
    fn item_id(&self, name: &str) -> Option<u32> {
        item_kind_from_name(name).map(|i| i.to_u32())
    }
    fn item_name(&self, id: u32) -> Option<String> {
        ItemKind::from_u32(id).map(name_of_item_kind)
    }
    fn is_food(&self, name: &str) -> bool {
        foods().contains(&strip_ns(name).as_str())
    }
    fn block_names(&self) -> Vec<String> {
        self.blocks.clone()
    }
    fn item_names(&self) -> Vec<String> {
        self.items.clone()
    }
    /// (hardness = `destroy_time`, harvest tool names). Empty tools = harvestable by hand.
    fn block_harvest(&self, name: &str) -> Option<(f32, Vec<String>)> {
        let kind = block_kind_from_name(name)?;
        let behavior = Box::<dyn BlockTrait>::from(BlockState::from(kind)).behavior();
        let tools =
            if behavior.requires_correct_tool_for_drops { harvest_tool_names(kind) } else { vec![] };
        Some((behavior.destroy_time, tools))
    }
}

/// Tools that yield drops. azalea has no per-block tool *class* data, so we assume a
/// pickaxe of the required tier and up. TODO(verify): class (axe/shovel) unmodeled.
fn harvest_tool_names(block: BlockKind) -> Vec<String> {
    use azalea::registry::tags::blocks as bl;
    let all = ["wooden_pickaxe", "stone_pickaxe", "iron_pickaxe", "diamond_pickaxe", "netherite_pickaxe"];
    let from = if bl::NEEDS_DIAMOND_TOOL.contains(&block) {
        3
    } else if bl::NEEDS_IRON_TOOL.contains(&block) {
        2
    } else if bl::NEEDS_STONE_TOOL.contains(&block) {
        1
    } else {
        0
    };
    all[from..].iter().map(|s| s.to_string()).collect()
}

/// Static edible-item set (azalea 0.15 exposes no default Food component). TODO(verify).
fn foods() -> &'static std::collections::HashSet<&'static str> {
    static FOODS: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new();
    FOODS.get_or_init(|| {
        [
            "apple", "golden_apple", "enchanted_golden_apple", "bread", "carrot", "golden_carrot",
            "potato", "baked_potato", "poisonous_potato", "beetroot", "melon_slice", "sweet_berries",
            "glow_berries", "cooked_beef", "beef", "cooked_porkchop", "porkchop", "cooked_chicken",
            "chicken", "cooked_mutton", "mutton", "cooked_rabbit", "rabbit", "cooked_cod", "cod",
            "cooked_salmon", "salmon", "tropical_fish", "pufferfish", "dried_kelp", "cookie",
            "pumpkin_pie", "mushroom_stew", "rabbit_stew", "beetroot_soup", "suspicious_stew",
            "honey_bottle", "chorus_fruit", "rotten_flesh", "spider_eye",
        ]
        .into_iter()
        .collect()
    })
}

// --- hostile classification (mineflayer "Hostile mobs" kind) ---

pub fn is_hostile(kind: EntityKind) -> bool {
    use azalea::registry::tags::entities as tags;
    if tags::UNDEAD.contains(&kind) || tags::RAIDERS.contains(&kind) {
        return true;
    }
    matches!(
        kind,
        EntityKind::Creeper
            | EntityKind::Spider
            | EntityKind::CaveSpider
            | EntityKind::Silverfish
            | EntityKind::Endermite
            | EntityKind::Enderman
            | EntityKind::Blaze
            | EntityKind::Breeze
            | EntityKind::Ghast
            | EntityKind::Slime
            | EntityKind::MagmaCube
            | EntityKind::Witch
            | EntityKind::Phantom
            | EntityKind::Guardian
            | EntityKind::ElderGuardian
            | EntityKind::Hoglin
            | EntityKind::Zoglin
            | EntityKind::Piglin
            | EntityKind::PiglinBrute
            | EntityKind::Warden
            | EntityKind::Shulker
            | EntityKind::Vex
            | EntityKind::Ravager
    )
}

// --- tool selection (port of equipBestTool + mineflayer-tool) ---

/// Select the fastest hotbar tool for `block`; with `require_harvest`, return false
/// (without committing to dig) when no carried tool would yield drops.
pub async fn equip_best_tool(bot: &Client, block: BlockState, require_harvest: bool) -> bool {
    use azalea::auto_tool::AutoToolClientExt;
    let best = bot.best_tool_in_hotbar_for_block(block);
    bot.set_selected_hotbar_slot(best.index as u8);
    if !require_harvest {
        return true;
    }
    let inv = bot.component::<Inventory>();
    let menu = &inv.inventory_menu;
    let slots = menu.slots();
    let hotbar = &slots[menu.hotbar_slots_range()];
    let held = hotbar.get(best.index).map(ItemStack::kind).unwrap_or(ItemKind::Air);
    has_correct_tool_for_drops(block, held)
}

/// Reimplements azalea's private harvest check (azalea_entity::mining) for require_harvest.
fn has_correct_tool_for_drops(block: BlockState, tool: ItemKind) -> bool {
    let bt = Box::<dyn BlockTrait>::from(block);
    if !bt.behavior().requires_correct_tool_for_drops {
        return true;
    }
    use azalea::registry::tags::{blocks as bl, items as it};
    let rb = bt.as_registry_block();
    if tool == ItemKind::Shears {
        matches!(rb, BlockKind::Cobweb | BlockKind::RedstoneWire | BlockKind::Tripwire)
    } else if it::SWORDS.contains(&tool) {
        rb == BlockKind::Cobweb
    } else if it::PICKAXES.contains(&tool)
        || it::SHOVELS.contains(&tool)
        || it::HOES.contains(&tool)
        || it::AXES.contains(&tool)
    {
        let level = get_item_tier(tool).map(|t| t.level()).unwrap_or(0);
        !((level < 3 && bl::NEEDS_DIAMOND_TOOL.contains(&rb))
            || (level < 2 && bl::NEEDS_IRON_TOOL.contains(&rb))
            || (level < 1 && bl::NEEDS_STONE_TOOL.contains(&rb)))
    } else {
        false
    }
}

// --- entity + world queries ---

/// Nearest hostile mob (optionally within `range`), same instance as the bot.
pub fn nearest_hostile(bot: &Client, range: Option<f64>) -> Option<Entity> {
    let found = bot.nearest_entity_by::<&EntityKindComponent, Without<LocalEntity>>(
        |k: &EntityKindComponent| is_hostile(**k),
    )?;
    if let Some(r) = range {
        let here = bot.position();
        let there: Vec3 = *bot.entity_component::<Position>(found);
        if here.distance_to(there) > r {
            return None;
        }
    }
    Some(found)
}

/// Bounded world scan for a block type (replaces mineflayer findBlocks): sorted-nearest
/// positions within `max_distance`, capped at `count`.
pub fn find_blocks_near(bot: &Client, kind: BlockKind, max_distance: i32, count: usize) -> Vec<BlockPos> {
    let from: BlockPos = match bot.get_component::<Position>() {
        Some(p) => (*p).into(),
        None => return vec![],
    };
    let states = BlockStates::from(kind);
    let world = bot.world();
    let instance = world.read();
    let md = max_distance as f64;
    instance
        .find_blocks(from, &states)
        .filter(|p| block_dist(from, *p) <= md)
        .take(count)
        .collect()
}

pub fn nearest_block(bot: &Client, kind: BlockKind, max_distance: i32) -> Option<BlockPos> {
    find_blocks_near(bot, kind, max_distance, 1).into_iter().next()
}

fn block_dist(a: BlockPos, b: BlockPos) -> f64 {
    let (dx, dy, dz) = ((a.x - b.x) as f64, (a.y - b.y) as f64, (a.z - b.z) as f64);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Time-box any world action (every skill action is time-boxed, per deps.ts).
pub async fn with_timeout<T>(fut: impl Future<Output = T>, ms: u64, label: &str) -> anyhow::Result<T> {
    match tokio::time::timeout(Duration::from_millis(ms), fut).await {
        Ok(v) => Ok(v),
        Err(_) => Err(anyhow::anyhow!("{label} timed out after {ms}ms")),
    }
}

// --- auth (port of installAuth; driven by agent.rs on Login/Chat events) ---

fn auth_prompt() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)not authenticated|please (log ?in|login)|use /login|/l to auth|you have to (login|register)|register (first|to)|not logged in").unwrap())
}
fn auth_ok() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)logged in|login successful|authentication successful|successfully (logged|registered|authenticated)|welcome back|session restored").unwrap())
}
fn auth_fail() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)wrong password|incorrect|invalid password|not registered|must register|password.*(short|long|weak)|too many|max.*accounts").unwrap())
}

type Note = Arc<dyn Fn(&str) + Send + Sync>;

/// Authenticate after join: send the (live-read) login line(s) on login and on any auth
/// prompt, stopping once the server confirms success. Newline / `&&`-separated lines stagger.
#[derive(Clone)]
pub struct Auth {
    get_message: Arc<dyn Fn() -> String + Send + Sync>,
    note: Note,
    sent: Arc<AtomicU32>,
    done: Arc<AtomicBool>,
}

impl Auth {
    pub fn new(get_message: Arc<dyn Fn() -> String + Send + Sync>, note: Note) -> Self {
        Self { get_message, note, sent: Arc::new(AtomicU32::new(0)), done: Arc::new(AtomicBool::new(false)) }
    }

    /// Call on the Login event (fires pre-spawn); sends after a short settle.
    pub fn on_login(&self, bot: &Client) {
        let this = self.clone();
        let bot = bot.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            this.send(&bot, false);
        });
    }

    /// Call on each server/system chat line.
    pub fn on_chat(&self, bot: &Client, line: &str) {
        if !self.done.load(Ordering::Relaxed) && auth_ok().is_match(line) {
            self.done.store(true, Ordering::Relaxed);
            (self.note)("authenticated ✓");
            return;
        }
        if auth_fail().is_match(line) {
            let snippet: String = line.chars().take(120).collect();
            (self.note)(&format!("auth rejected: {snippet}"));
            return;
        }
        if !self.done.load(Ordering::Relaxed)
            && self.sent.load(Ordering::Relaxed) < 6
            && auth_prompt().is_match(line)
        {
            self.send(bot, true);
        }
    }

    fn send(&self, bot: &Client, prompted: bool) {
        if self.done.load(Ordering::Relaxed) {
            return;
        }
        let lines: Vec<String> = split_login((self.get_message)());
        if lines.is_empty() {
            (self.note)(if prompted {
                "server requires login but none is configured — set it in the dashboard"
            } else {
                "no login message configured"
            });
            return;
        }
        self.sent.fetch_add(1, Ordering::Relaxed);
        for (i, line) in lines.into_iter().enumerate() {
            let (bot, note, done) = (bot.clone(), self.note.clone(), self.done.clone());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(i as u64 * 700)).await;
                if done.load(Ordering::Relaxed) {
                    return;
                }
                bot.chat(line.clone());
                note(&format!("sent login: {line}"));
            });
        }
    }
}

/// Split a login message into lines on newlines or ` && `.
fn split_login(msg: String) -> Vec<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"\n|\s+&&\s+").unwrap());
    re.split(&msg).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

// --- kick / disconnect reasons (port of kickReason) ---

fn kick_friendly(key: &str) -> Option<&'static str> {
    Some(match key {
        "multiplayer.disconnect.duplicate_login" => "duplicate login (another connection with this name)",
        "multiplayer.disconnect.idling" => "kicked for idling (AFK)",
        "multiplayer.disconnect.kicked" => "kicked by an operator",
        "multiplayer.disconnect.server_shutdown" => "server shut down",
        "multiplayer.disconnect.flying" => "flying is not enabled on this server",
        "multiplayer.disconnect.slow_login" => "login timed out",
        _ => return None,
    })
}

/// Readable text from a disconnect reason (chat component / translation key).
pub fn kick_reason(reason: &FormattedText) -> String {
    let s = flatten_component(reason);
    let s = s.trim();
    kick_friendly(s).unwrap_or(s).to_string()
}

fn flatten_component(c: &FormattedText) -> String {
    match c {
        FormattedText::Text(t) => {
            let mut s = t.text.clone();
            for sib in &t.base.siblings {
                s.push_str(&flatten_component(sib));
            }
            s
        }
        FormattedText::Translatable(t) => {
            if let Some(f) = kick_friendly(&t.key) {
                return f.to_string();
            }
            let mut s = t.key.clone();
            for sib in &t.base.siblings {
                s.push_str(&flatten_component(sib));
            }
            s
        }
    }
}

// --- reconnect backoff (port of Reconnector) ---

type Handle = tokio::task::JoinHandle<()>;

struct ReconState {
    attempts: u32,
    stable: Option<Handle>,
    pending: Option<Handle>,
}

/// Backed-off reconnect with an attempt cap. A connection up long enough resets the backoff;
/// too many quick failures give up.
pub struct Reconnector {
    max_attempts: u32,
    on_reconnect: Arc<dyn Fn() + Send + Sync>,
    on_give_up: Arc<dyn Fn(u32) + Send + Sync>,
    state: Arc<Mutex<ReconState>>,
}

impl Reconnector {
    pub fn new(
        max_attempts: u32,
        on_reconnect: Arc<dyn Fn() + Send + Sync>,
        on_give_up: Arc<dyn Fn(u32) + Send + Sync>,
    ) -> Self {
        Self {
            max_attempts,
            on_reconnect,
            on_give_up,
            state: Arc::new(Mutex::new(ReconState { attempts: 0, stable: None, pending: None })),
        }
    }

    /// Call once a connection is live; if it survives `stable_ms`, the backoff resets.
    pub fn mark_connected(&self, stable_ms: u64) {
        let mut st = self.state.lock();
        if let Some(h) = st.stable.take() {
            h.abort();
        }
        let state = self.state.clone();
        st.stable = Some(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(stable_ms)).await;
            state.lock().attempts = 0;
        }));
    }

    /// Call on disconnect. Reconnects after a growing delay (capped 30s) if `should_reconnect`,
    /// else gives up. Returns the scheduled delay in ms (0 = gave up).
    pub fn schedule_reconnect(&self, should_reconnect: Arc<dyn Fn() -> bool + Send + Sync>) -> u64 {
        let mut st = self.state.lock();
        if let Some(h) = st.stable.take() {
            h.abort();
        }
        if let Some(h) = st.pending.take() {
            h.abort();
        }
        st.attempts += 1;
        if st.attempts > self.max_attempts {
            let attempts = st.attempts;
            let on_give_up = self.on_give_up.clone();
            drop(st);
            on_give_up(attempts);
            return 0;
        }
        let delay = 30_000u64.min(2000 * 2u64.pow(st.attempts - 1));
        let on_reconnect = self.on_reconnect.clone();
        st.pending = Some(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            if should_reconnect() {
                on_reconnect();
            }
        }));
        delay
    }

    /// Cancel a scheduled reconnect without touching the backoff count.
    pub fn cancel_pending(&self) {
        if let Some(h) = self.state.lock().pending.take() {
            h.abort();
        }
    }

    pub fn reset(&self) {
        let mut st = self.state.lock();
        st.attempts = 0;
        if let Some(h) = st.pending.take() {
            h.abort();
        }
        if let Some(h) = st.stable.take() {
            h.abort();
        }
    }
}

// --- chunk prune (port of startChunkPrune; adapts to azalea world API) ---

/// Periodically drop loaded columns beyond `keep_radius` chunks of the bot, reclaiming the
/// unbounded shared world-copy (#1123). Stops when the bot disconnects.
/// TODO(verify): also prunes the client PartialChunkStorage ring if the leak persists.
pub fn start_chunk_prune(bot: Client, keep_radius: i32, interval_ms: u64) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
            let pos: Vec3 = match bot.get_component::<Position>() {
                Some(p) => *p,
                None => break, // disconnected
            };
            let cx = (pos.x.floor() as i32) >> 4;
            let cz = (pos.z.floor() as i32) >> 4;
            let world = bot.world();
            let mut instance = world.write();
            instance
                .chunks
                .map
                .retain(|p, _| (p.x - cx).abs().max((p.z - cz).abs()) <= keep_radius);
        }
    })
}

// --- misc ---

/// Cumulative on-wire bytes. TODO(verify): azalea exposes no socket byte counters; agent.rs
/// accounts bytes from serialized request/response lengths instead.
pub fn socket_bytes(_bot: &Client) -> (u64, u64) {
    (0, 0)
}

/// Append to a capped ring-buffer log with an ISO-8601 timestamp.
pub fn log_line(log: &mut Vec<String>, msg: &str) {
    log.push(format!("{} {}", iso_now(), msg));
    if log.len() > 100 {
        log.remove(0);
    }
}

fn iso_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    let (h, mi, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let (y, mo, d) = civil_from_days((secs / 86400) as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}

/// Howard Hinnant's days-from-civil, inverted: days since epoch -> (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
