# minecraft-agents

The bot-side component: one OpenHost app running a persistent **dispatcher**
player that players tag in-game to summon LLM-controlled Mineflayer **worker**
bots on a Minecraft Java server. Each worker connects as a normal client,
pursues one natural-language goal via a Claude planning loop over a fixed skill
library, then **logs out when the task finishes**.

Architecture follows the Voyager/Mindcraft pattern (see `RESEARCH.md`): the LLM
plans at the **skill level, not motor commands**; a deterministic Mineflayer layer
executes; fresh perception is fed back every step.

## Topology

```
                          +--> worker agent_1 (task -> logout)
[ this app ]              |--> worker agent_2 (task -> logout)
  dispatcher "agents" ----+--> worker agent_3 ...
        |                       (each: Minecraft protocol --> Java server)
        +-- @agents chat commands in-game
        +-- HTTP control API (OpenHost, owner-gated)
        +-- Anthropic API (planner, per worker)
```

The dispatcher stays online and non-interactable; workers are ephemeral —
summoned for a goal, gone when done, reusable by number. All connect **out** to
the server (offline backend behind a Velocity proxy, or any reachable host).

## Configuration (env vars)

One process, one config → the whole roster. Set these on the OpenHost app.

| Var | Default | Purpose |
|---|---|---|
| `ANTHROPIC_API_KEY` | — | Claude planner key. **Local dev only** — in production it's pulled from the OpenHost **secrets** service at boot (grant the `ANTHROPIC_API_KEY` secret); see `openhost.toml`. |
| `OPENAI_API_KEY` | — | OpenAI planner key, needed only when a bot uses the `gpt-5.6-luna` model. Resolved like `ANTHROPIC_API_KEY` (env var locally, else OpenHost secrets). |
| `MC_HOST` | `localhost` | Server host — **also editable live in the dashboard** |
| `MC_PORT` | `25565` | Server port — **also editable live in the dashboard** |
| `MC_VERSION` | auto | Pin if auto-detect fails |
| `MC_AUTH` | `offline` | `offline` or `microsoft` |
| `LOGIN_MESSAGE` | — | Sent on join to authenticate (e.g. `/login <pw>`); two-step via `&&` or newline (e.g. `/register <pw> <pw> && /login <pw>`) — **editable live in the dashboard** |
| `DISPATCHER_NAME` | `agents` | Username of the always-on dispatcher players tag |
| `BOT_COUNT` | `0` | Optional pre-spawned workers `agent_1`..`agent_N` (usually 0 — summon on demand) |
| `BOTS_CONFIG` | — | Path to a JSON array of `{goal?, model?}` for pre-spawned workers; numbered `agent_1`.. by array order |
| `LLM_MODEL` | `claude-haiku-4-5` | Default planner model — `claude-haiku-4-5` (`haiku`), `claude-sonnet-5` (`sonnet`), or `gpt-5.6-luna` (`luna`, OpenAI). Provider is picked from the model id; thinking is always off. **Also selectable live in the dashboard** (applies to each worker's next task) |
| `LLM_EFFORT` | `low` | `low`..`max` (Sonnet only; ignored by Haiku) |
| `LLM_MAX_STEPS` | `40` | Skill calls per goal before the loop stops. **Also editable live in the dashboard** (1–1000; applies to in-flight and future tasks) |
| `COMMAND_ALLOWLIST` | — | Comma-separated usernames allowed to command `@<dispatcher>`; empty = anyone |
| `MAX_BOTS` | `20` | Cap on concurrent **online** workers (logged-out ones don't count) |
| `MAX_PER_USER` | `5` | Cap on online workers one player may own (0 = unlimited). Live-editable in the dashboard |
| `MC_VIEW_DISTANCE` | server default | Worker view-distance (`tiny`..`far` or chunk count); cuts per-bot RAM **only** on servers with per-player view-distance (Paper/Folia). The dispatcher always uses `tiny` |
| `CHUNK_KEEP_RADIUS` | `12` | Drop each bot's loaded chunks beyond this radius to cap the roaming world-copy leak. **Must be ≥ the server's view-distance** or blocks near the bot go missing |
| `DISPATCHER_RECYCLE_MIN` | `45` | Minutes between dispatcher reconnects to reset its accumulated chunk memory (it never logs out); `0` disables |
| `PORT` | `8080` | HTTP port (matches `openhost.toml`) |
| `DB_PATH` | `$OPENHOST_APP_DATA_DIR/minecraft-agents.db` | SQLite file for persisted state; falls back to `$DATA_DIR` then `./data` locally |
| `LIBRARY_DIR` | `$OPENHOST_APP_DATA_DIR/library` | Shared library of bot-authored **routines** (`routines/`) and **settings** (`settings/`), as JSON files (not the DB); one collection every agent reads and writes |

Env vars **seed** the config; live settings (host, port, login, per-user cap)
saved in the DB **override** them on the next boot (see Persistence below).

Workers are normally summoned in-game (below), so `BOT_COUNT` defaults to `0` —
only the dispatcher runs at boot. Pre-spawning via `BOT_COUNT`/`BOTS_CONFIG` is
optional; those workers have no owner and so aren't reusable through `@agents`.

## Control API

All routes are login-gated by the OpenHost router (nothing is public).

| Method | Path | Body | Action |
|---|---|---|---|
| GET | `/health` | — | Liveness |
| GET | `/dispatcher` | — | Dispatcher name, online state, recent log |
| GET | `/bots` | — | Status of every worker (incl. logged-out) |
| GET | `/bots/:name` | — | One worker's status + recent log |
| POST | `/summon` | `{"count":N,"goal":"..."}` | Summon N fresh workers on one goal → `{spawned:[...],rejected}` |
| POST | `/bots/:name/goal` | `{"goal":"..."}` | Retask a worker (admin; reconnects if logged out); **409** if busy |
| POST | `/bots/:name/chat` | `{"message":"..."}` | Say something in-game |
| POST | `/bots/:name/stop` | — | Disconnect a worker |
| POST | `/dev/reset` | `{"confirm":true}` | **Dev:** disconnect + forget every agent and its memory; **keeps** live settings and the shared routine/settings library → `{removed:N}` |

The HTTP channel is admin (owner-gated by the OpenHost router) and bypasses the
in-game ownership check.

`GET /` serves a **live dashboard** — a scrolling, auto-refreshing (1.5s) summary
line per agent: state (active/idle/connecting/stopped, color-coded), owner, goal,
step + conversation length, tokens in/out, cache-read tokens, **network in/out**,
and health/food, with dispatcher status, fleet token + **traffic** totals in the
header. Traffic = real Minecraft socket bytes (on-wire) per bot + approximate API
request/response bytes; summed across all workers and the dispatcher. The header
also has **live-editable controls** — the Minecraft server host/port, login
message, per-user cap, **planner model** (dropdown), and **step budget** (`max
steps`). All are **staged and applied together** with the **apply** button (no
restart); the fleet reconnects only if host/port/login changed. The model applies
to each worker's next task, the step budget to in-flight and future tasks, and the
per-user cap to the next summon. A colored dot shows whether the dispatcher is
connected. **Click any row (or the dispatcher)** to open its log — spawn/kick
reasons, server auth replies (`srv:`), and the step-by-step tool calls and results.
The per-user cap is enforced for in-game players (owner `api`/HTTP admin is exempt);
over-cap `new` requests are truncated and the dispatcher says so in chat.

## In-game chat commands

Management runs through the **dispatcher** (`@agents`), commanded publicly with
`@agents <cmd>` or privately with `/msg agents <cmd>`; it replies **privately**
either way. The owner steers a running worker with `@agent_N` / `/msg`. The
dispatcher holds **op** and teleports each freshly summoned worker to its owner on
login. Full grammar, ownership rules, responses, and the HTTP equivalents are in
**[COMMANDS.md](COMMANDS.md)**.

```
@agents new 3 mine iron ore          # create 3 workers (shorthand: @a n 3 …)
@agents 1 2 build a wall            # task workers you own
@agents quit 1                       # disconnect agent_1 now
@agent_1 focus on oak                # steer a running worker (owner only)
```

## Skills the planner can call

The worker's action space — perceive, move/mine/build, logistics, combat,
background behaviors, talk — is documented in **[TOOLS.md](TOOLS.md)**.
Navigation uses `mineflayer-pathfinder` (A\*); every action is time-boxed and a
perception snapshot is injected each step.

## Deploy

```bash
oh app deploy https://github.com/<you>/minecraft-agents --name minecraft-agents --wait
oh app logs minecraft-agents
```

Then:
1. Store the planner key in the OpenHost **secrets** service under `ANTHROPIC_API_KEY`
   and approve this app's grant for it (the app fetches it at boot).
2. Open the app dashboard (`GET /`) and set the **server host/port** and **login
   message** (e.g. `/login <pw>`) — live, no restart.

Summon workers (or just tag `@agents` in-game):

```bash
oh curl -X POST https://minecraft-agents.<zone>/summon \
  -H 'content-type: application/json' -d '{"count":3,"goal":"collect 5 oak_log"}'
```

## Scaling

Per-worker cost is dominated by (a) Claude calls and (b) the server-side chunks
each bot keeps loaded — not the bot process (see `RESEARCH.md` for measured
figures, ~100 MB/online worker). `MAX_BOTS` caps concurrent *online* workers;
raise `memory_mb`/`cpu_millicores` in `openhost.toml` alongside it. Workers
logging out on task completion naturally frees memory and capacity. For many bots
on one IP, raise the server's per-IP join/registration limit for the auth plugin.

**Bot-side lag** (jerky movement while the server is fine) is single-thread
contention: Node runs *all* bots on one event loop, so pathfinding (synchronous
A\*), 20 Hz physics, and chunk/entity packets compete for one thread — extra CPU
cores don't help JS. Mitigations already applied: pathfinding is capped per bot
(`tickTimeout` 10 ms/tick, bounded `searchRadius`) so concurrent A\* can't starve
other bots, the dispatcher runs with physics off + `tiny` view, and each bot prunes
its chunk copy (`CHUNK_KEEP_RADIUS`). To cut it further, lower `MC_VIEW_DISTANCE`
(on Paper/Folia) and keep fewer workers online at once. Past ~8–12 active workers
the fix is **horizontal**: run additional app instances, each driving a subset —
one event loop can only do so much.

## Persistence

State lives in a SQLite DB (`node:sqlite`, no native build) on the OpenHost
`app_data` volume (`$OPENHOST_APP_DATA_DIR`), written on every change:

- **Dashboard settings** — server host/port, login message, per-user cap, planner
  model, step budget. Override the env seed at boot, so dashboard edits survive a restart/redeploy.
- **Ownership** — each `agent_N`'s owner, so owned numbers persist across
  restarts (offline placeholders are recreated at boot).
- **Memory** — per-owner waypoints, notes, and the task ledger.

Bot-authored **routines** (`save_routine`) and **reactive settings**
(`create_setting`) live **outside** the DB, as JSON files in a single shared
library (`LIBRARY_DIR` → `library/routines/` and `library/settings/`) that every
agent — across all owners — reads and writes. One file per procedure/rule.

Agent logs, token/traffic counters, and live bot connections stay in-memory
(diagnostic, high-churn). Delete the DB file to reset all persisted state.

## Local dev

```bash
npm install
npm run build
ANTHROPIC_API_KEY=sk-... MC_HOST=localhost npm start   # dispatcher only; summon via HTTP or @agents
```

## Notes

- TypeScript (strict) because Mineflayer is Node-native; the CommonJS game
  libraries are loaded via `createRequire` to avoid ESM named-export pitfalls.
- Server connection is outbound and offline-mode-friendly; see `RESEARCH.md`
  for the auth/proxy trade-offs.
- `LLM_EFFORT` is sent as `output_config.effort`, which Haiku rejects. The planner
  detects that 400 once, drops the field (and `thinking`), and continues.
- Cost efficiency: the tools+system prefix is prompt-cached (a stable ~4k-token
  prefix → ~0.1× on cache reads; engages on Sonnet now, on Haiku once the tool set
  clears its 4096-token minimum). History is compacted **deterministically** — old
  tool results collapse to per-tool summaries (`summarizeResult`) and only the
  latest step keeps full output + fresh perception; durable state rides in the
  ledger/waypoints, not the transcript. No model calls are used to compact.
