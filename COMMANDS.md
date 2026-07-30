# Command reference

All in-game interaction is text in Minecraft chat. Two surfaces:

- **`@agents`** — the dispatcher (a spectator player with no worker of its own).
  Manages workers: create, task, and transfer ownership. It replies to every
  command, addressed to the sender.
- **`@agent_N` / `/msg agent_N`** — an individual worker, for steering it mid-task.

`COMMAND_ALLOWLIST` (if set) limits who the dispatcher obeys. `DISPATCHER_NAME`
(default `agents`) is the dispatcher's username.

## Notation

- `x [y …]` — one or more agent numbers, **space-separated** (e.g. `1 2 3`). Each
  may be written bare (`1`) or prefixed (`agent_1`).
- `<task>` / `<msg>` — free text (the rest of the line).
- `[n]` — optional; omitted means `1`.

## Dispatcher commands (`@agents …`)

| Shape | Meaning |
|---|---|
| `@agents new [n] <task>` | Create `n` new workers (default 1) running `<task>`, owned by you. |
| `@agents x [y …] <task>` | Give `<task>` to existing workers you own. |
| `@agents release x [y …]` | Relinquish ownership of those workers (they become unowned / claimable). |
| `@agents claim x [y …]` | Take ownership of those numbers — unowned or previously released ones, or a number that doesn't exist yet. |
| `@agents reserve x [y …]` | Reserve arbitrary numbers as yours **without connecting them** (owner logged, offline). Task later with `@agents x <task>` to bring them online. |
| `@agents give x [y …] <player>` | Transfer ownership of your workers to `<player>`. |

`claim` and `reserve` are the same operation — both log you as owner of a number
without logging the bot in; `reserve` is the natural word when grabbing arbitrary
unused numbers ahead of time.

If a `new [n]` request exceeds the online-agent cap (`MAX_BOTS`), the dispatcher
creates as many as fit and says so in chat, e.g.
`Steve created agent_1, agent_2 on: mine iron — 1 not summoned (agent limit reached)`.

### Examples

```
@agents new collect 10 oak_log            # 1 new worker
@agents new 3 mine iron ore               # 3 new workers, same goal
@agents 1 2 build a dirt wall            # retask agent_1 and agent_2 (must be yours)
@agents agent_4 follow me                 # agent_ prefix also accepted
@agents release 1 2                      # give up agent_1, agent_2
@agents claim 5 7                        # claim those numbers
@agents give 1 2 Steve                   # hand agent_1, agent_2 to Steve
```

## Steering a running worker (owner only)

While a worker is online, its **current owner** can send it a prompt without
interrupting the task. Both forms are equivalent and both are owner-gated:

```
@agent_3 focus on oak, skip birch
/msg agent_3 come back once you have 10
```

The message is fed into the worker's live planning loop as an `OWNER:` note on its
next step. If the worker is idle (not running), the message is taken as a new goal.
Non-owners are ignored.

## Rules & responses

- **Ownership** — you may `task`, `release`, and `give` only workers you own.
  `claim` takes unowned workers (one owned by someone else is refused). Ownership
  is by player name.
- **Lifecycle** — a worker runs one task, then **logs out** on completion or
  failure. Its number and owner are kept, so `@agents x <task>` (or `claim`)
  brings it back online.
- **Busy** — a running worker rejects *new task* assignment until it finishes;
  steer it with `@agent_N` / `/msg` instead.
- **Capacity** — `MAX_BOTS` caps concurrent online workers; logged-out ones don't
  count.

The dispatcher replies to the sender and lists anything it skipped, e.g.:

```
Steve created agent_8, agent_9 on: mine iron ore
Steve agent_1 on: build a dirt wall (skipped agent_2: not_owner)
Steve claimed agent_5 (skipped agent_7: owned_by_other)
Steve gave agent_1 to Alex
```

Skip reasons: `unknown` (no such worker), `not_owner`, `busy`, `at_capacity`,
`owned_by_other`.

## HTTP equivalents (admin)

Owner-gated by the OpenHost router; these bypass the in-game ownership check.

| Method | Path | Body |
|---|---|---|
| `POST` | `/summon` | `{"count":N,"goal":"…"}` — like `new` |
| `POST` | `/bots/:name/goal` | `{"goal":"…"}` — retask (reconnects if logged out; 409 if busy) |
| `POST` | `/bots/:name/chat` | `{"message":"…"}` |
| `POST` | `/bots/:name/stop` | — disconnect |
| `GET` | `/bots` · `/bots/:name` · `/dispatcher` | status |
