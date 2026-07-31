# Lean planner thread — contract for `agent.rs`

A deliberate improvement over the faithful TS port. Behavior is identical (act → observe →
adjust); only the **encoding of context** changes. The TS loop re-sent the whole transcript
(all past `tool_use`/`tool_result`/assistant text) every step, so per-request tokens grew
O(steps). The Rust loop is **stateless per step**: each request carries only what the next
decision needs. This is the top lever for scaling concurrent bots — fewer tokens/step → lower
cost and lower TPM per bot → more bots planning within the same API rate limits.

## The rule: drop old tool calls; keep current surroundings

Each planning step sends **exactly one user message**. No assistant / `tool_use` /
`tool_result` blocks are ever carried forward. State that must persist across steps lives in
**durable memory** (ledger, waypoints, notes) — that's what makes dropping the transcript safe.

### Request shape (every step)

- `system` (unchanged, **prompt-cached**): rules + the full tool set. Keep this stable — it's the
  cached prefix; don't make it per-step.
- `messages`: a single `user` message, built fresh each step:

```
GOAL: <goal>

<observe() — current surroundings: vitals, inventory, nearby players/mobs when present>
<durable-memory summary — open ledger items + waypoint names>

LAST: <tool_name> -> <result>        # outcome of the immediately preceding action; omit on step 1
<OWNER: ...> / <AGENT <name>: ...> / <took N damage ...>   # injected messages, if any
```

The assistant replies with **one** `tool_use`. Execute it, capture the result string, then build
the *next* single user message with that result as the new `LAST:` line. Discard the previous
turn entirely.

### Why this is correct, not lossy

- **Continuity comes from durable memory, not the transcript.** The planner tracks multi-step
  progress via `update_ledger`/`remember_note`; perception is fresh every step. The system prompt
  should nudge the planner to use the ledger (it already does).
- **`LAST:` gives the immediate feedback loop** — the model sees the consequence of its last
  action, which is the only history that materially affects the next choice.
- **No tool-call API pairing issues.** You cannot keep a `tool_result` without its owning
  assistant `tool_use` turn, so the last result is folded in as **plain text** (`LAST:`), not a
  `tool_result` block. Every request is a clean `[system][one user] -> [assistant tool_use]`.

## Encoding details

- **`LAST:` result** — cap it with `summarize_result(name, full)` (already in `base.rs`) so a
  verbose `find_blocks`/`scan_area`/`top_down` result doesn't bloat the one line it occupies.
  With no transcript, `summarize_result` is now used *only* for this single last result.
- **`observe()`** is already lean (`base.rs`): vitals + inventory always; `players nearby` /
  `mobs within 16m` only when non-empty (no constant "none" lines).
- **Multiple tool calls in one turn** — if the model ever returns >1 `tool_use`, execute them in
  order and concatenate their results into `LAST:` (`t1 -> r1 | t2 -> r2`).
- **`task_complete`** ends the loop as before; **step budget** (`LLM_MAX_STEPS`) still caps it.

## What NOT to do

- Don't accumulate a `Vec<Message>` history across steps. Rebuild the single user message each
  step from (goal, observe, memory, last-result, injected).
- Don't put the goal or perception into the cached `system` block — only tools+rules are stable
  and cacheable; goal/perception change per task/step.
- Don't keep compacted old `tool_use` blocks "just in case" — the ledger is the memory. (If a
  future need arises for a short window, keep at most the last 1 step and still as `LAST:` text,
  never as tool blocks.)

## Rough effect

Late in a task the TS loop's *uncached* tail was ~1.5–2k tokens (accumulated transcript +
narration + full perception). This makes it ~O(a few hundred) and flat across the whole task,
with the tools+system prefix still cached. No change to which actions the bot takes.
