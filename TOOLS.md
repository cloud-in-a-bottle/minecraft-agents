# Agent tools

The skills the planner (Claude) can call, one per step. These are the **worker's
internal action space** — distinct from the chat commands in
[COMMANDS.md](COMMANDS.md), which are how *people* drive the bots.

Each call returns a short text result. Every action is **time-boxed** so a stuck
pathfind can't wedge the loop, and each step the planner is given a **perception
snapshot** (position, health/food, held item, inventory, nearby players, hostile
mobs within 16 m) plus a **durable-memory summary** (open ledger items + saved
waypoint names). Navigation uses `mineflayer-pathfinder` (A\*), not Baritone;
sustained combat uses `mineflayer-pvp`.

Base tools live in `src/skills.ts`; the rest are pluggable modules
(`src/skills_iron.ts`, `_memory.ts`, `_survival.ts`, `_multiagent.ts`, plus the
`src/skills/` subdirectory — `presence.ts`, `messaging.ts`, `rules.ts`)
aggregated in `src/registry.ts`.

## Perceive

| Tool | Inputs | Returns |
|---|---|---|
| `list_inventory` | — | Everything the bot is carrying |
| `find_blocks` | `name, count, max_distance` | Coordinates of the nearest N blocks of a type |
| `scan_area` | `radius` (max 8) | Count of every solid block within the radius, by type |
| `top_down` | — | 5×5 heightmap: first block down per column, height vs. ground (eye `+2`, waist `+1`, ground `0`, below negative) |
| `match_block_names` | `pattern, limit` | Block names matching a regex |
| `match_item_names` | `pattern, limit` | Item names matching a regex (for craft/equip/give) |
| `who_online` | — | Players currently in the tab list, with each one's ping |

## Recipes & knowledge

| Tool | Inputs | Returns |
|---|---|---|
| `get_recipe` | `item` | Ingredients + counts, output count, whether a crafting table is required |
| `get_block_info` | `block` | Hardness, tool tier needed to drop it, and whether the held item can harvest it |
| `inventory_gap` | `item, count` | Recursively expands the recipe tree, subtracts inventory → flat "still need" list |

## Move · mine · build

| Tool | Inputs | Does |
|---|---|---|
| `go_to` | `x, y, z` | Pathfind to a coordinate |
| `go_to_player` | `username` | Pathfind to within 2 blocks of a player |
| `follow_player` | `username, seconds` (max 300) | Follow continuously, ~2 blocks away |
| `collect_block` | `name, count` | Find + mine + pick up N of a block |
| `mine_block` | `x, y, z` | Dig the block at an exact coordinate |
| `place_block` | `name, x, y, z` | Place a carried block (needs a solid block below) |
| `craft_item` | `name, count` | Craft, using a nearby crafting table if required |
| `equip_item` | `name, destination` | Equip to `hand`/`head`/`torso`/`legs`/`feet`/`off-hand` |

## Tech tree · mining

| Tool | Inputs | Does |
|---|---|---|
| `smelt` | `input, fuel, count` | Operate the nearest furnace: load fuel+input, wait, collect output |
| `craft_station` | `station` (`crafting_table`\|`furnace`\|`blast_furnace`) | Ensure a station is nearby, crafting + placing one if absent |
| `dig_staircase` | `target_y` | Dig a descending 2-high staircase to a depth, torching, lava-guarded (bounded) |
| `strip_mine` | `direction` (n/s/e/w), `length` (max 64) | Dig a 1×2 branch tunnel, torching at intervals, stopping at lava |
| `dig_down_safe` | `depth` | Mine straight down, stopping if lava/water/void is 2 below |

## Logistics

| Tool | Inputs | Does |
|---|---|---|
| `deposit` | `item, count, x, y, z` | Put items into the chest at a coordinate |
| `withdraw` | `item, count, x, y, z` | Take items from the chest at a coordinate |

## Combat · survival

| Tool | Inputs | Does |
|---|---|---|
| `attack_nearest` | — | Approach + hit the nearest hostile mob once |
| `attack_player` | `username` | Approach + hit a player once |
| `fight` | `target` | Sustained melee until dead/fled/~30s. `target` = `nearest`, a mob name, or a username |
| `flee` | `distance` (max 32) | Run away from the nearest hostile mob |

## Durable memory (host-side, scoped per owner)

Persists across a worker logging out. The ledger + waypoints are also injected
into perception each step, so plans survive the step budget.

| Tool | Inputs | Does |
|---|---|---|
| `save_waypoint` | `name` | Record the current position under a name (use `base` for home) |
| `goto_waypoint` | `name` | Pathfind to a saved waypoint |
| `list_waypoints` | — | Saved waypoints with coords + distance |
| `remember_note` | `key, text` | Store a durable learning/fact |
| `recall_notes` | `query` (empty = all) | Retrieve notes matching a query |
| `update_ledger` | `item, status` (`todo`\|`doing`\|`done`) | Add/update a checklist item |
| `read_ledger` | — | The current checklist |

## Interaction · multi-agent

| Tool | Inputs | Does |
|---|---|---|
| `summon_agents` | `count, goal` | Summon helper agents on a goal — owned by your owner, counted against that player's cap |
| `activate_block` | `x, y, z` | Right-click a block (doors, levers, buttons, plates) |
| `collect_drops` | `radius` | Walk to and pick up dropped items nearby |
| `give_item` | `target, item, count` | Toss items to another agent or player |
| `go_to_agent` | `agent, range` | Pathfind to another worker's live position |

## Background behaviors

`set_behavior {behavior, enabled}` toggles a behavior that runs on its own until
turned off. Persists across a worker's reconnects. **`defend` and `auto_eat` are
on by default**; the rest are opt-in.

| Behavior | Does |
|---|---|
| `defend` | *(default on)* On damage, hit back the nearest attacker (mob or non-friendly player; never owner/other agents) |
| `auto_eat` | *(default on)* Eat any food when hunger drops to ≤14 |
| `maintain_light` | Place a torch when standing in low light and carrying one |
| `retreat_if_low_health` | At health ≤7, disengage and flee the nearest hostile |
| `lava_guard` | Stop and step back when lava or a big drop is imminent ahead |
| `anti_stuck` | Detect a stalled pathfind and nudge (jump / dig ahead) to recover |

## Talk (locked to owner + teammates)

Bots have **no public chat**. A worker can only message its **owner** and **fellow
agents owned by the same player**. Incoming owner/teammate messages — and a
"took N damage" note whenever the bot is hurt — are injected into the planning loop.

| Tool | Inputs | Does |
|---|---|---|
| `message` | `to, message` | Private message to your owner (in-game `/msg`) or a same-owner teammate agent (in-process). Any other target is refused |
| `message_team` | `message` | Broadcast an in-process message to every online same-owner teammate agent |

## Settings (self-authored reactive rules)

Condition→action rules the bot writes for itself; evaluated every second and fired
when the condition holds (10 s per-rule cooldown). Persisted as JSON **files** under
the app's `settings/` directory (one file per rule, scoped per owner) — **not the DB**.

| Tool | Inputs | Does |
|---|---|---|
| `create_setting` | `name, condition, steps` | Save a rule: when `condition` holds, run `steps` (same grammar as `save_routine`). E.g. `food<14` → collect + eat food |
| `list_settings` | — | List rules (name, on/off, condition) |
| `toggle_setting` | `name, enabled` | Enable or disable a rule |
| `delete_setting` | `name` | Remove a rule |

Conditions use the routine grammar: `have:<item><op>N`, `find:<block><op>N`,
`health<op>N`, `food<op>N` (op ∈ `>= <= > < == !=`).

## Routines (self-authored procedures)

Reusable, saved procedures the planner composes from the skills above — for
repetitive work (gathering, crafting chains) it authors one, then replays it with
**no per-step LLM calls**. Steps are interpreted data, not code (no `eval`), and
may only call whitelisted skills. Saved per owner in SQLite, shared across that
owner's agents. Execution is bounded by a step budget (300) and a 5-minute deadline.

| Tool | Inputs | Does |
|---|---|---|
| `save_routine` | `name, description, steps` | Store a procedure. A step is `{tool,args}`, `{repeat:N,do:[…]}`, `{until:"<cond>",max:N,do:[…]}`, or `{when:"<cond>",do:[…],else:[…]}`; `{param}` placeholders in args/conditions |
| `run_routine` | `name, args` | Run a saved routine, filling `{param}` from `args` (nesting ≤3) |
| `list_routines` | — | List saved routines to reuse |

Conditions: `have:<item><op>N`, `find:<block><op>N`, `health<op>N`, `food<op>N`
(op ∈ `>= <= > < == !=`), evaluated against live inventory/surroundings.

## Control

| Tool | Inputs | Does |
|---|---|---|
| `task_complete` | `summary` | End the task — goal achieved or impossible |
