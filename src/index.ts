import { loadConfig } from "./config.js";
import { resolveApiKey, resolveOpenAiKey } from "./secrets.js";
import { BotManager } from "./manager.js";
import { Store } from "./store.js";
import { createApi } from "./api.js";

const config = loadConfig();
[config.llm.apiKey, config.llm.openaiApiKey] = await Promise.all([resolveApiKey(), resolveOpenAiKey()]);

const store = new Store(config.dbPath);
const manager = new BotManager(config, store);
manager.startAll();

const app = createApi(manager);
app.listen(config.port, "0.0.0.0", () => {
  console.log(
    `minecraft-agents listening on :${config.port} — dispatcher "${config.dispatcherName}" -> ${config.mc.host}:${config.mc.port}`,
  );
});
