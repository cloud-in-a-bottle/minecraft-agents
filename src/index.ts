import { loadConfig } from "./config.js";
import { resolveApiKey } from "./secrets.js";
import { BotManager } from "./manager.js";
import { createApi } from "./api.js";

const config = loadConfig();
config.llm.apiKey = await resolveApiKey();

const manager = new BotManager(config);
manager.startAll();

const app = createApi(manager);
app.listen(config.port, "0.0.0.0", () => {
  console.log(
    `minecraft-agents listening on :${config.port} — dispatcher "${config.dispatcherName}" -> ${config.mc.host}:${config.mc.port}`,
  );
});
