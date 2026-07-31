import { Store } from "./dist/store.js";
import { BotManager } from "./dist/manager.js";
import { rmSync } from "node:fs";

const dbp = "/tmp/mca-mgr.db";
for (const s of [dbp, dbp + "-wal", dbp + "-shm"]) { try { rmSync(s); } catch {} }

const baseConfig = () => ({
  port: 8080, dbPath: dbp,
  mc: { host: "seed-host", port: 25565, auth: "offline", loginMessage: "" },
  llm: { apiKey: "sk-test", model: "claude-haiku-4-5", effort: "low", maxSteps: 40 },
  bots: [], dispatcherName: "agents", commandAllowlist: [], maxBots: 20, maxPerUser: 5,
});

// --- Boot 1: change settings + create ownership, no network (don't startAll) ---
let store = new Store(dbp);
let m = new BotManager(baseConfig(), store);
m.updateSettings({ mcHost: "saved.example.net", mcPort: 25599, loginMessage: "/login secret", maxPerUser: 3 });
m.claim([1, 5], "Steve");   // reserve numbers for Steve (offline)
m.give([5], "Steve", "Alex"); // transfer 5 to Alex
console.log("boot1 settings:", JSON.stringify(m.getSettings()));

// --- Boot 2: fresh Store + manager on same DB, env seed is still "seed-host" ---
store = new Store(dbp);
m = new BotManager(baseConfig(), store);
console.log("boot2 settings:", JSON.stringify(m.getSettings()));
console.log("boot2 owners:", m.list().map((a) => `${a.username}=${a.owner}`).join(", "));
