# LLM Agents in Minecraft — State of the Art (2023–2026)

Research on embedding LLM-driven agents into a Minecraft server: prior implementations,
how they connect and act, measured effectiveness, limitations, and build guidance.

> Method: multi-source web research with adversarial (3-vote) claim verification.
> 22 primary/secondary sources, 25 claims triple-verified. Benchmark numbers below are
> almost all **self-reported by each paper against baselines it chose** — not independently
> reproduced. Cross-system comparisons are unreliable (different Minecraft versions, task
> setups, iteration budgets). Treat headline multipliers as directional, not exact.

## 1. The dominant architecture

Every serious 2023–2026 system converges on the same shape, established by **Voyager**:

- **High-level planner (LLM)** emits *code or skill calls*, **not** low-level motor commands.
  Programs naturally express temporally-extended, compositional, interpretable actions.
- **Ever-growing skill library** — verified executable snippets, embedded as vectors and
  retrieved by similarity search. New skills are added as the agent succeeds.
- **Iterative feedback loop** — environment state + execution errors + a separate LLM
  "self-verification" critic drive refinement until a task passes.
- **Action interface** — the LLM's code is bridged to the game through **Mineflayer**
  (a JS/Node bot API). This is near-universal across the field.

The consistent lesson: **put the LLM at the planning layer, let a deterministic API handle
low-level control.** End-to-end learned motor control (VPT/MineRL style) is a separate,
harder paradigm that the code-as-action approach largely sidesteps.

## 2. Major implementations

| System | Year | Planner | Key idea | Control |
|---|---|---|---|---|
| **Voyager** | 2023 | GPT-4 (blackbox, no fine-tune) | Automatic curriculum + skill library + self-verification; **code as action space** | Mineflayer JS |
| **GITM** (Ghost in the Minecraft) | 2023 | LLM decomposer + structured actions | Beats RL on the tech tree's hardest task | Mineflayer-style |
| **STEVE** (See and Think) | 2024 (ECCV) | LLaMA-2-7B/13B → STEVE-7B/13B | Adds **vision** (EfficientFormerV2 encoder) + skill DB | MineDojo env, Mineflayer |
| **Odyssey** | 2024 (IJCAI'25) | **Fine-tuned open LLaMA-3 8B/70B (MineMA)** | Open-weight core; 40 primitive + 183 compositional skills | Mineflayer |
| **PPA / parallelized planning-acting** | 2025 (AAMAS'26) | LLM multi-agent | Dual-thread (planning + acting), server never pauses, real-time multi-agent | Mineflayer skill lib |
| **Mindcraft** | ongoing | Pluggable (GPT-4o, Claude, Gemini, Ollama, HF…) | Ready-made **multi-agent** framework, JSON agent profiles | Mineflayer, MC ≤1.21.x |

**Architectural evolution:** Voyager (closed GPT-4, text-only, pauses server) →
STEVE (adds vision, fine-tuned open model) → Odyssey (fully open-weight planner, larger
skill library) → PPA (real-time, parallel multi-agent). The trajectory is toward
**open-weight models, visual perception, and concurrent multi-agent execution.**

> Note: the brief named STEVE-1, JARVIS-1, Plan4MC, and MineRL. No claims about these
> survived verification in this pass, so they're not characterized here — a known gap.
> (STEVE-1 and Plan4MC are the low-level/hybrid-control lineage; worth a follow-up.)

## 3. How agents connect and act

- **Mineflayer** is the de-facto control layer: a stable JS/Node bot API (usable from
  Python via JSPyBridge), supporting Minecraft Java **1.8–1.21.x**. Exposes movement/physics,
  block query/dig/build, entity tracking, inventory/crafting, and combat. LLM-generated code
  calls these functions. Voyager, STEVE, Odyssey, Mindcraft, and PPA all bridge through it.
- **MineDojo** is the standard *simulation/benchmark/knowledge* substrate (not the runtime
  control layer): 1,581 programmatic + ~1,560 creative + 64 curated core tasks, plus an
  internet-scale knowledge base (730K+ YouTube videos / 2.2B transcript words, 6,735 wiki
  pages, 340K+ Reddit posts). It exposes structured spatial grounding — a **3×3×3 voxel**
  observation with per-block collidability, tool requirement, liquidity, solidity,
  flammability, light-blocking.
- **Mindcraft** is the fastest on-ramp for a new build: connects LLM agents to a real Java
  server via Mineflayer, model chosen in a JSON profile (`"model": "gpt-4o"`), and
  `agent_count` ≥ 1 for multi-agent coordination.

**Server-pause distinction (important design choice):** Voyager *pauses the server* during
LLM planning to freeze the world (fine for single-agent, breaks multi-agent). PPA runs the
server **continuously** so all agents act in real time. If you want multi-agent or a live
shared server with human players, you cannot pause — plan asynchronously.

## 4. Effectiveness — what works

- **Tech tree / crafting** is the strongest result. Voyager: 63 unique items in 160
  iterations (3.3× baselines), tech milestones up to **15.3× faster** (wood; 8.5× stone,
  6.4× iron), and the only baseline-era agent to reach **diamond**. STEVE claims ~1.3–1.5×
  over Voyager on tool tiers and ~2.5× faster block/diamond search.
- **Beating RL on the hardest task.** GITM improved `ObtainDiamond` success by **+47.5
  percentage points** over the ~20% RL SOTA (OpenAI VPT), i.e. ~67.5% absolute —
  LLM planning outperforming learned RL control on the canonical benchmark.
- **Exploration** — Voyager travels 2.3× further and discovers more of the map than
  AutoGPT/ReAct/Reflexion.
- **Multi-agent combat scales sharply with team size.** PPA vs the **Ender Dragon**:
  0% (3 agents) → 41.7% (5) → 91.7% (10) → **100% (20)**; the **Wither**: 41.7% (3) →
  100% (10+). Tasks impossible for individuals become reliable with coordinated teams.

## 5. Limitations — what breaks

These come from primary sources but were extracted rather than triple-verified in this pass;
treat as strong signal, slightly lower certainty than §4.

- **Long-horizon planning collapses.** A 2025 spatial-reasoning benchmark reports a
  **uniform 0.00 pass rate on long-horizon interactive spatial tasks across all tested
  LLMs**; even GPT-5 hits only ~0.75 on *short*-horizon tasks. Chained goals (e.g. "mine
  iron ore" end-to-end) drop a zero-shot vision agent to ~10% on hard tasks and **0% on
  chained tasks**, despite 100% on simple ones.
- **Spatial hallucination.** Long-horizon path planning fails from "spatial hallucination"
  and "context inconsistency hallucination" — the model invents geometry/state it can't
  actually perceive.
- **Self-correction doesn't fix wrong knowledge.** LLMs "stubbornly adhere to erroneous
  parametric knowledge and fail to self-correct even when given failure feedback" —
  motivating *algorithmic* (non-LLM) knowledge correction rather than trusting the reflection
  loop to converge.
- **Vision can hurt.** Several models perform **worse** with image observations than with
  structured text — VLM grounding in Minecraft is still weak; don't assume adding vision helps.
- **Multi-agent coordination degrades with scale (outside scripted combat).** Mindcraft is
  reported to lose ~15pp when agents share plans over chat — open-ended collaboration
  (building, division of labor) is far less solid than the boss-fight numbers suggest.
- **Latency/cost (structural, under-quantified in sources).** Closed GPT-4-class planners
  mean per-action API latency and cost; Voyager pausing the server is partly a symptom of
  this. The field's move to fine-tuned open models (Odyssey/MineMA) is largely a cost/latency
  and self-hosting response, though whether open models *match* closed planners on reliability
  is still open.

## 6. Build guidance for a new system (2026)

**Recommended baseline stack:**

1. **Control:** Mineflayer against a real Java server. Non-negotiable — it's what everything
   uses and where the skill ecosystem lives.
2. **Action space:** code/skill calls, not motor commands. Maintain a vector-indexed,
   verified **skill library** that grows as the agent succeeds (the Voyager pattern).
3. **Planner:** start with a strong closed model (GPT-4o / Claude) for reliability; consider
   a **fine-tuned open model** (Odyssey/MineMA-style LLaMA-3) once you need cost control,
   self-hosting, or lower latency. Validate that the open model actually holds success rate —
   this is not yet settled.
4. **Framework:** **Mindcraft** for the fastest path to a working (multi-)agent bot — JSON
   profiles, pluggable backends, multi-agent out of the box. Study Voyager's curriculum +
   self-verification loop and port what you need.
5. **Grounding:** feed structured observations (MineDojo-style voxel/block metadata), not
   just screenshots. Vision is optional and can *hurt*.

**Design decisions that matter most:**

- **Don't pause the server** if you want multi-agent or live human players — plan async
  (PPA's dual-thread pattern).
- **Keep tasks short-horizon.** Decompose aggressively; the 0.00 long-horizon pass rate is
  the field's hard wall. Reliability lives in recursive decomposition + a deterministic skill
  library, not in trusting the LLM to plan 50 steps ahead.
- **Don't rely on LLM self-correction** to fix factual/knowledge errors — add deterministic
  validation and an external knowledge source (wiki/recipe data).
- **Scale multi-agent for hard objectives** (combat, gathering) — team size buys success —
  but budget for coordination overhead and expect open-ended collaboration to be shakier.

## Bot memory usage (rough estimates)

To be confirmed against a real server — these are pre-flight estimates from
community reports plus this app's measured floor.

| Component | Rough RAM | Source |
|---|---|---|
| Process floor — Node + deps, bots not yet in world | ~130 MB, flat 1→20 bots | measured on this app |
| `minecraft-data` (loads once per server version, shared) | tens of MB | prismarine |
| **Per connected bot, steady state** | **~100 MB** | mineflayer #2251: ~50 bots ≈ 5 GB node RSS |
| Long-running / roaming bot | grows unbounded | mineflayer #1123: chunks don't unload; one data-collection bot ~2 GiB after ~30 min |

**Rough sizing for this app** (steady-state, near spawn): 1 bot ≈ 230 MB · 5 ≈
600 MB · 10 ≈ 1.1 GB · 20 ≈ 2 GB. So the `openhost.toml` default of 512 MB
comfortably holds ~2–3 bots; raise `memory_mb` (and expect to split across
processes/`worker_threads`) as `MAX_BOTS` climbs.

**What actually drives it:**
- Each bot keeps its **own** copy of the world — memory scales ~linearly with bot
  count (the mineflayer maintainer: "they all have to keep track of the world
  individually").
- The real lever is the **server's `view-distance`/`simulation-distance`** (fewer
  chunks streamed = less per-bot RAM). Setting the *bot's* `viewDistance` to
  `"tiny"` does **nothing** — the maintainer confirmed this; the server decides
  what chunks it sends.
- **Chunks don't unload** as a bot travels (a known leak, #1123), so a long-lived
  roaming bot drifts upward for its whole session. Mitigations: keep bots near a
  small area, recycle them periodically (this app already auto-reconnects on
  disconnect — a scheduled reconnect would cap growth), or split bots across
  processes.
- Beyond a handful of bots, one Node process is the bottleneck; the ecosystem
  recommends `worker_threads` or separate processes/containers. Our current
  single-process design is fine for small fleets but will strain at high
  `MAX_BOTS` — a known scaling limit to revisit.

Sources: [mineflayer #2251](https://github.com/PrismarineJS/mineflayer/discussions/2251),
[mineflayer #1123](https://github.com/PrismarineJS/mineflayer/issues/1123),
[mineflayer #1950](https://github.com/PrismarineJS/mineflayer/discussions/1950),
[prismarine-viewer](https://github.com/PrismarineJS/prismarine-viewer).

## Network usage (rough estimates)

Two internet channels per agent (the Minecraft server is **remote**, so its
traffic is not LAN-local):

| Channel | Direction | Rough rate | Applies to |
|---|---|---|---|
| Minecraft protocol (bot ↔ server) | inbound-heavy (chunks, entities) | idle ~1–5 KB/s; moving/mining tens–hundreds KB/s, bursty | **every connected bot** (idle too) |
| Anthropic API (app ↔ Anthropic) | upload-heavy (prompt) | ~2–10 KB/s while working; ~20–30 KB uploaded/step | **working bots only** |

On a remote server the Minecraft side usually dominates and scales with all
online bots; the API side scales with concurrently-working bots. Levers:
log-out-on-completion (drops the MC connection to zero), `MAX_BOTS`/per-user cap
(caps concurrent MC connections), bounded movement + low server view-distance
(chunk streaming), history compaction (API upload). Prompt caching cuts API
*cost* but not *bytes*.

Measured live via the dashboard: real MC socket bytes (`net.Socket.bytesRead/
bytesWritten`, on-wire/compressed) per bot + approximate API JSON bytes, summed
fleet-wide. Estimates above are unverified; confirm with the dashboard totals.

## Existing skill libraries — reuse vs. build (2026 check)

No polished, typed, `npm install`-able skill library exists for a Mineflayer/TS app today.
Closest reusable catalogs:

- **Mindcraft** (kolbytn/mindcraft, MIT) — ~47 hand-written `fn(bot, …)` JS skills + a `world.js`
  query lib. Fixed library; optional insecure coding mode runs generated JS in an SES compartment
  (off by default). Maps almost 1:1 to our ~46-skill set — the best coverage checklist.
- **Voyager** (MineDojo, MIT) — not a library but a *generation pipeline*: the LLM writes async JS
  Mineflayer functions, self-verifies, and stores them in an embedding-indexed store for retrieval.
  Runs generated code **unsandboxed** (`eval`) — a code-injection surface, unacceptable here since
  our bots read untrusted public chat.
- **Odyssey** (zju-vipa, code MIT / dataset CC-BY-NC-SA) — largest curated set (40 primitive + 183
  compositional skills) but entangled with Python + a fine-tuned LLaMA-3; port piecemeal at best.
- **Project Sid** (Altera) — report/paper only, no code released.

**Decision:** keep our fixed, hand-authored skill set (reliability, Mindcraft model) and add safe
runtime **composition** instead of Voyager-style code-gen. Implemented as *routines* — saved,
parameterized procedures (sequence + `repeat`/`until`/`when`) interpreted over the whitelisted
skills, persisted per owner in SQLite. No `eval`, bounded by step/time budgets. See `TOOLS.md` →
Routines and `src/routines.ts`.

## Open questions

1. Quantified limits — end-to-end latency, API $/task, hallucination/grounding failure rates,
   exact long-horizon breakdown point — the evidence base covers capability far better than cost.
2. Do fine-tuned open-weight planners (MineMA-style) really match GPT-4-class reliability at
   materially lower cost, or is a closed model still required?
3. How do the low-level learned-control systems (STEVE-1, Plan4MC, JARVIS-1, VPT/MineRL)
   compare to the code/skill high-level paradigm — and is a hybrid better?
4. Multi-agent coordination beyond scripted boss fights — open-ended building and division of
   labor, given Mindcraft's reported degradation with more agents.

## Key sources

- Voyager — arxiv.org/abs/2305.16291 · voyager.minedojo.org
- GITM (Ghost in the Minecraft) — arxiv.org/abs/2305.17144
- STEVE (ECCV 2024) — ecva.net (papers_ECCV/papers/01280.pdf)
- Odyssey / MineMA (IJCAI 2025) — arxiv.org/abs/2407.15325
- Parallelized planning-acting multi-agent (AAMAS 2026) — arxiv.org/abs/2503.03505
- MineDojo (NeurIPS 2022, Outstanding Paper) — arxiv.org/abs/2206.08853 · docs.minedojo.org
- Mineflayer — github.com/PrismarineJS/mineflayer
- Mindcraft — github.com/kolbytn/mindcraft
- Limitations: arxiv.org/abs/2512.23328 (long-horizon spatial), 2411.13543 (game-agent survey),
  2505.24157 (knowledge self-correction), 2509.06235 (zero-shot vision agent)
