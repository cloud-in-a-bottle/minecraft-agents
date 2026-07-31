# minecraft-agents → Rust/azalea port — porting contract

Authoritative brief for every porting subagent. The source of truth for behavior is
the existing TypeScript in `../src/`. Port behavior faithfully; do **not** redesign.
This file defines the shared Rust contract so modules built in parallel fit together.

## What this app is

One process runs a persistent **dispatcher** bot (players tag it in-game) plus ephemeral
LLM-planned **worker** bots. Each worker connects, pursues one natural-language goal via a
Claude/OpenAI planning loop over a fixed skill library, then logs out. There is an HTTP
control API + live dashboard. Read `../README.md` and `../TOOLS.md` for the full picture.

The Node app runs N mineflayer bots (one JS event loop). The Rust app runs the same fleet
as an **azalea swarm** (`azalea::swarm`) — many bots, one process, one Bevy ECS + tokio runtime.

## Target: azalea 0.15.1+mc1.21.11

```toml
azalea = "=0.15.1"   # +mc1.21.11 build metadata; do NOT bump to 0.16 (moves to mc26.1)
```

## Module map (TS → Rust). All under `rust/src/`.

| TS source | Rust file | Layer | azalea? |
|---|---|---|---|
| `types.ts` | `types.rs` | foundation | no |
| `config.ts` | `config.rs` | 1 config | no |
| `secrets.ts` | `secrets.rs` | 1 config | no |
| `store.ts` | `store.rs` | 1 persistence (SQLite) | no |
| `filestore.ts`,`routinestore.ts`,`rulestore.ts` | `library.rs` | 1 persistence (JSON dir) | no |
| `llm.ts` | `llm.rs` | 2 planner | no |
| `routines.ts` | `routines.rs` | 3 interpreter | no (via `BotView`) |
| `rules.ts` | `rules.rs` | 3 rule engine | no (via `BotView`) |
| `api.ts` | `api.rs` | HTTP + dashboard | no |
| `skillkit.ts` | `skill.rs` | 4 contract | trait defs only |
| `skills.ts` (base + execute + observe + auto-behaviors) | `skills/base.rs` | 4 | yes |
| `skills_iron.ts` | `skills/iron.rs` | 4 | yes |
| `skills_memory.ts` | `skills/memory.rs` | 4 | yes |
| `skills_survival.ts` | `skills/survival.rs` | 4 | yes |
| `skills_multiagent.ts` | `skills/multiagent.rs` | 4 | yes |
| `skills/presence.ts`,`messaging.ts`,`rules.ts` | `skills/{presence,messaging,rules}.rs` | 4 | yes |
| `registry.ts` | `skills/mod.rs` | 4 | aggregation |
| `deps.ts` | `mc.rs` | 4 helpers (auth, reconnect, chunk prune, kick reasons) | yes |
| `agent.ts` | `agent.rs` | 4 worker lifecycle + planning loop | yes |
| `dispatcher.ts` | `dispatcher.rs` | 4 command bot | yes |
| `manager.ts` | `manager.rs` | 4 fleet | yes |
| `index.ts` | `main.rs` | entry | yes |

## Conventions

- **Runtime:** tokio (multi-thread). azalea drives a Bevy ECS internally; our per-bot loop is async.
- **Errors:** skill executors return `String` (natural-language result for the planner), exactly like
  the TS `execute()` — a caught error becomes `format!("error: {e}")`. Do not use `Result` at the skill
  boundary; keep the "return a sentence" contract. Internal helpers may use `anyhow::Result`.
- **Shared mutable state:** `Arc<Mutex<…>>` (parking_lot). The fleet map, per-agent status, and logs are shared.
- **Naming:** snake_case fns/fields; keep tool NAMES as the exact wire strings (`find_blocks`, `go_to`, …) —
  the planner calls them by name. Keep JSON field names identical (serde `rename_all` where needed) — the
  dashboard JS and the persisted DB/library files depend on them.
- **Comments:** terse. Two-line max on doc comments. No "differs from TS" notes.
- **String results:** match the TS wording of results closely; the planner is prompted around these phrasings.

## Decomplecting seams (so layers 1–3 need no azalea)

These traits are defined in `skill.rs` (layer 4 owns the trait *definitions*, but they name no azalea
types except behind `BotView`). Layers 1–3 depend only on the traits.

```rust
// A read/act view over one bot, implemented by the azalea layer (agent.rs).
// Lets routines.rs / rules.rs evaluate conditions and run tools without importing azalea.
pub trait BotView {
    fn inv_count(&self, item: &str) -> i64;          // have:<item>
    fn nearby_count(&self, block: &str) -> i64;      // find:<block> (32m, cap 64)
    fn health(&self) -> f32;
    fn food(&self) -> f32;
}
// Tool executor seam: async fn(tool, json args) -> String. Boxed so the interpreter is azalea-free.
pub type Exec = Arc<dyn Fn(String, serde_json::Value) -> BoxFuture<'static, String> + Send + Sync>;
```

`routines.rs` `RunCtx` and `rules.rs` `RuleEngine` take `&dyn BotView` + `Exec` + budget/deadline/log —
mirroring the TS `RunCtx`. No azalea imports in layers 1–3.

Persistence traits (defined in `skill.rs`, implemented in `store.rs`/`library.rs`):
`Memory` (waypoints/notes/ledger/summary), `RoutineStore`, `RuleStore`, `PeerApi`. Signatures mirror
`skillkit.ts` exactly.

Planner trait (`llm.rs`): `async fn create(&self, req: PlanRequest) -> anyhow::Result<PlanResponse>`.
`PlanRequest`/`PlanResponse` are our own structs (model, system, tools, messages / content blocks + usage) —
do NOT depend on a vendor SDK type. Anthropic + OpenAI are two impls; OpenAI translates to the Responses API
exactly as `OpenAiPlanner` does in `llm.ts` (messages→input items, tool_use→function_call, cached-token math).

## azalea API cheat-sheet (VERIFY against the compiling workspace — signatures approximate)

> These are from docs.rs 0.15.1. Where a call doesn't exist as named, find the nearest equivalent in the
> compiled crate and leave a `// TODO(verify)` note. The azalea layer is expected to need a compile pass.

- Build/connect: `ClientBuilder::new().set_handler(handle).start(account, addr)`, `Account::offline(name)`.
  Swarm: `SwarmBuilder::new().set_handler(...).set_swarm_handler(...).add_account(...).start(addr)`.
- Handler: `async fn handle(bot: Client, event: Event, state: State) -> anyhow::Result<()>`.
  Events: `Event::Init`, `Event::Login`, `Event::Chat(ChatPacket)`, `Event::Tick`, `Event::Death(..)`, `Event::Disconnect(..)`.
- Position/vitals: `bot.position() -> Vec3`, `bot.health() -> f32`, `bot.hunger()`/hunger component for food,
  `bot.component::<T>()` / `bot.entity_component::<T>()` for other state (e.g. held item via inventory menu).
- Movement: pathfinder via `PathfinderClientExt`: `bot.goto(goal)` (async, resolves when arrived/failed).
  Goals in `azalea::pathfinder::goals`: `BlockPosGoal`, `RadiusGoal(pos, radius)`, `ReachBlockPosGoal`,
  `XZGoal`, `YGoal`. Stop via `bot.stop_pathfinding()` / `StopPathfindingEvent`. Low-level: `bot.walk`, `bot.sprint`,
  `bot.set_direction(yaw,pitch)`, `bot.look_at(vec3)`, `bot.set_jumping(bool)`.
- Mining: `bot.mine(BlockPos)` (async, mines until broken) or `start_mining`. Auto tool-selection is NOT built in
  like mineflayer-tool — port the "equip best/harvest-capable tool" logic manually against inventory + block data.
- Place/interact: `bot.block_interact(BlockPos)`; item use `bot.start_use_item()`. Placement semantics differ from
  mineflayer `placeBlock(refBlock, faceVec)` — port carefully; may need to face + use item on a block face.
- Attack: `bot.attack(entity_id)`; cooldown via `has_attack_cooldown()`.
- Chat/commands: `bot.chat("msg")` (a leading `/` sends a command). `/tp`, `/login`, `/msg <user> <text>` all go via chat.
- World queries: `bot.world()` → read blocks by `BlockPos`. There is no mineflayer `findBlocks`; implement a bounded
  spiral/box scan over loaded chunks (mirror `find_blocks`/`scan_area` radius logic). `minecraft-data` equivalent:
  use `azalea::registry` / block+item enums for names↔ids, hardness, harvest tools.
- Entities: iterate ECS entities near the bot for players/mobs (nearest hostile, nearest player). Tab list / players
  map for `who_online`.
- Block/item metadata: azalea has generated block & item registries (`azalea_block`, `azalea::registry::Item`).
  Port `mcData.blocksByName`/`itemsByName`, hardness, `harvestTools`, `foodsByName` onto these.

## Manager facade (api.rs consumes it; manager.rs must expose exactly this)

`api.rs` holds `Arc<manager::BotManager>` and calls only these (no direct `Agent` refs across locks):

```rust
fn list(&self) -> Vec<AgentStatus>;
fn status(&self, name: &str) -> Option<AgentStatus>;
fn dispatcher_status(&self) -> DispatcherStatus;
fn get_settings(&self) -> Settings;
fn update_settings(&self, patch: SettingsPatch) -> anyhow::Result<()>;   // Err on bad model string
fn create_new(&self, count: usize, goal: &str, owner: Option<&str>) -> CreateResult;  // HTTP owner = Some("api")
fn assign(&self, name: &str, goal: &str) -> AssignOutcome;               // NotFound | Busy | Ok
fn chat(&self, name: &str, message: &str) -> bool;                        // false = no such bot
fn stop(&self, name: &str) -> bool;                                       // false = no such bot
fn wipe_agents(&self) -> usize;                                           // returns removed count
```

`Settings`, `SettingsPatch`, `DispatcherStatus`, `AssignOutcome`, `CreateResult` live in `types.rs`.
`config::MODELS` (slice) and `config::normalize_model(&str) -> anyhow::Result<String>` back the model dropdown/validation.

## Layer 4 fan-out plan (azalea — gated on the compiling workspace)

The contract for this layer is `src/skills/mod.rs` (SkillContext, Skill, BehaviorHandler, McData) + `src/skill.rs`.
Base tools are a `match` in `base.rs::execute`, NOT `Skill` objects (mirrors `execute()` in skills.ts). Pluggable
modules each expose `pub fn skills() -> Vec<Arc<dyn Skill>>` (and survival also `behaviors()`).

Shared azalea-layer pieces one agent must build FIRST (others depend on them):
- `mc.rs`: `McData` impl over azalea registries (block/item name↔id, hardness, harvest tools, foods);
  plus the `deps.ts` helpers — `install_auth`, `Reconnector`, `start_chunk_prune`, `kick_reason`, `anti_afk`,
  `socket_bytes`, `equip_best_tool(bot, block, require_harvest)`, `nearest_hostile(bot, range)`, `with_timeout`.
- `base.rs`: `observe(ctx) -> String` (perception snapshot), `execute(ctx, name, input) -> String` (the big match:
  find_blocks/get_position/go_to/collect_block/go_toward/mine_block/place_block/craft_item/equip_item/attack*/
  fight/flee/follow_player/deposit/withdraw/scan_area/top_down/set_behavior/save_routine/run_routine/list_routines/
  list_inventory/match_block_names — and dispatch to `all_skills()` for the rest), `summarize_result`, `TOOLS` list,
  `install_auto_behaviors`, `auto_eat`, `defend`. `find_blocks`/`scan_area`/`top_down` need a bounded world scan
  (no mineflayer findBlocks) — implement over `bot.world()`.

Assignments (each writes only its file(s); all depend on mc.rs + skills/mod.rs + skill.rs + llm.rs):
| Agent | Files | Source |
|---|---|---|
| L4-core | `mc.rs`, `skills/base.rs` | deps.ts, skills.ts |
| L4-iron | `skills/iron.rs` | skills_iron.ts (recipes, smelt, craft_station, dig_staircase, strip_mine) |
| L4-mem | `skills/memory.rs` | skills_memory.ts (waypoints/notes/ledger) |
| L4-surv | `skills/survival.rs` | skills_survival.ts (dig_down_safe + 4 behaviors) |
| L4-multi | `skills/multiagent.rs`, `skills/presence.rs`, `skills/messaging.rs`, `skills/rules.rs` | the small skill modules |
| L4-agent | `agent.rs` | agent.ts — worker lifecycle + planning loop; builds PlanRequest, self-heals 400 by clearing effort then thinking, KEEP_FULL=4 history compaction, token + byte accounting (bytes = serialized request/response len + azalea socket bytes), impls `BotView`, builds the `Exec` closure wrapping `base::execute` |
| L4-disp | `dispatcher.rs` | dispatcher.ts — command bot, `@agents`/whisper grammar, teleport |
| L4-mgr | `manager.rs` | manager.ts — fleet map, ownership, the manager facade in this doc, `PeerApi` impl, settings load/apply |

Sequencing: run L4-core alone first (or first in a wave). Once `mc.rs`/`base.rs` land, fan out the rest together.
`Cargo.toml` may need `azalea::swarm`, `futures-util`; add as needed. Expect a real `cargo build` fix-up pass after —
azalea signatures in the cheat-sheet are approximate.

## Pinned interfaces the wave calls (L4-core implements mc.rs/base.rs; everyone else CALLS these)

All 7 downstream agents code against these exact signatures. If L4-core's realized signature differs,
the final `cargo build` pass reconciles — do NOT redefine these yourself. azalea types marked `~` are
approximate (verify against the crate source); keep the shape.

`mc.rs` (helpers):
```rust
pub fn mc_data(version: &str) -> std::sync::Arc<dyn crate::skills::McData>;
pub async fn equip_best_tool(bot: &azalea::Client, block: &~Block, require_harvest: bool) -> bool;
pub fn nearest_hostile(bot: &azalea::Client, range: Option<f64>) -> Option<~Entity>;
pub async fn with_timeout<T>(fut: impl std::future::Future<Output = T>, ms: u64, label: &str) -> Result<T, anyhow::Error>;
pub async fn sleep(ms: u64);                                   // tokio sleep wrapper
pub fn socket_bytes(bot: &azalea::Client) -> (u64, u64);       // (in, out); (0,0) if unavailable
pub fn kick_reason(reason: &~DisconnectReason) -> String;
pub fn log_line(log: &mut Vec<String>, msg: &str);             // ISO ts + ring cap 100
pub struct Reconnector; // new(max, on_reconnect: impl Fn(), on_give_up: impl Fn(u32)); mark_connected(ms); schedule_reconnect(should:Fn()->bool)->u64; cancel_pending(); reset()
// auth + chunk-prune + anti-afk are driven from the Event handler; L4-core exposes the pieces the handler calls.
```

`base.rs` (agent.rs calls these):
```rust
pub fn observe(ctx: &SkillContext) -> String;                                  // perception snapshot
pub async fn execute(ctx: &SkillContext, name: &str, input: serde_json::Value) -> String;
pub fn tools() -> Vec<crate::llm::ToolDef>;                                     // base tools + all_skills schemas
pub fn summarize_result(name: &str, full: &str) -> String;
pub async fn auto_eat(ctx: &SkillContext);                                      // called from agent.rs on health
pub fn defend(ctx: &SkillContext, is_friendly: &dyn Fn(&str) -> bool);         // called from agent.rs on health
```

Auto-behavior orchestration lives in **agent.rs**, not a mineflayer-style installer: each Tick, agent.rs builds a
`SkillContext`, runs `skills::all_behaviors()` `on_tick` (for enabled names) + `RuleEngine::tick(...)`; on health
drop it runs `auto_eat`/`defend` (if enabled) + `all_behaviors` `on_health`.

## Shared azalea call patterns (assume these; confirm against crate source; mark drift `// TODO(verify)`)

- position `bot.position() -> Vec3`; health `bot.health() -> f32`; food `bot.hunger()`/hunger component.
- move: `bot.goto(goal).await` with `azalea::pathfinder::goals::{BlockPosGoal, RadiusGoal, XZGoal, YGoal, ReachBlockPosGoal}`; stop `bot.stop_pathfinding()`.
- dig: `bot.mine(BlockPos).await` (equip via `mc::equip_best_tool` first); place/interact: `bot.block_interact(BlockPos)` + `bot.start_use_item()`.
- attack: `bot.attack(entity_id)`. chat/commands: `bot.chat("…")` (leading `/` = command; `/tp`,`/login`,`/msg`).
- world reads: via `bot.world()` (RwLock) — get block state at a `BlockPos`. No `findBlocks`: use a bounded box scan.
- names↔ids/hardness/harvest/foods: `ctx.mc_data` (the `McData` trait), NOT minecraft-data.
- entities/players: iterate the ECS / tab list for nearest hostile, nearest player, who_online.
- `BlockPos` from ints; `Vec3` for exact positions; offsets are signed (`+x` east, `+y` up, `+z` south) — see `crate::skill::rel`.

## Definition of done per module

Compiles conceptually against the contract; mirrors the TS behavior and result strings; keeps wire/JSON/tool names
identical; terse comments. Leave `// TODO(verify)` where an azalea signature is uncertain. Do not invent new features.
