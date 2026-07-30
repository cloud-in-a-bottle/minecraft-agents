import express, { type Request, type Response } from "express";
import type { BotManager } from "./manager.js";

const DASHBOARD = `<!doctype html><meta charset=utf-8>
<title>minecraft-agents</title>
<meta name=viewport content="width=device-width,initial-scale=1">
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  body { margin:0; font:13px/1.4 ui-monospace,SFMono-Regular,Menlo,monospace; background:#0e1116; color:#d6dde6; }
  header { position:sticky; top:0; padding:10px 14px; background:#151a21; border-bottom:1px solid #232a34; display:flex; gap:16px; align-items:baseline; flex-wrap:wrap; }
  header b { color:#fff; font-size:15px; }
  header .stat { color:#8b97a6; } header .stat span { color:#d6dde6; }
  #wrap { height:calc(100vh - 44px); overflow-y:auto; }
  table { width:100%; border-collapse:collapse; }
  thead th { position:sticky; top:0; background:#11161d; color:#8b97a6; text-align:left; font-weight:normal; padding:6px 10px; border-bottom:1px solid #232a34; }
  td { padding:5px 10px; border-bottom:1px solid #1a1f27; white-space:nowrap; }
  td.goal { white-space:nowrap; overflow:hidden; text-overflow:ellipsis; max-width:340px; color:#aeb8c4; }
  tr:hover td { background:#141a22; }
  .dot { display:inline-block; width:8px; height:8px; border-radius:50%; margin-right:7px; }
  .working{background:#3fb950} .idle{background:#388bfd} .connecting{background:#d29922} .error{background:#f85149} .stopped{background:#6e7681}
  .name { color:#fff; } .owner { color:#8b97a6; } .num { color:#aeb8c4; } .cache { color:#3fb950; }
  .muted { color:#6e7681; }
  header input { width:50px; background:#0e1116; color:#d6dde6; border:1px solid #2b333d; border-radius:4px; padding:2px 5px; font:inherit; }
</style>
<header>
  <b>minecraft-agents</b>
  <span class=stat>dispatcher <span id=disp>—</span></span>
  <span class=stat>agents <span id=total>0</span></span>
  <span class=stat>active <span id=active>0</span></span>
  <span class=stat>tokens <span id=tok>0</span></span>
  <span class=stat>traffic <span id=net>0</span></span>
  <span class=stat>server <input id=host placeholder=host style="width:110px"> : <input id=port type=number style="width:58px"></span>
  <span class=stat>login <input id=login type=password placeholder="/login <pw>" style="width:120px" title="sent in chat on spawn; reconnects the fleet"></span>
  <span class=stat>per-user cap <input id=cap type=number min=0 title="0 = unlimited; applies to next summon, no restart"></span>
  <span class=stat muted id=upd></span>
</header>
<div id=wrap><table>
  <thead><tr><th>agent</th><th>owner</th><th>state</th><th>goal</th><th>step</th><th>conv</th><th>tok in/out</th><th>cache</th><th>net ↓/↑</th><th>hp/food</th></tr></thead>
  <tbody id=rows></tbody>
</table></div>
<script>
const k = n => n>=1000 ? (n/1000).toFixed(n>=10000?0:1)+'k' : String(n||0);
const fb = n => { n=n||0; if(n>=1e9)return (n/1e9).toFixed(2)+'GB'; if(n>=1e6)return (n/1e6).toFixed(1)+'MB'; if(n>=1e3)return (n/1e3).toFixed(0)+'KB'; return n+'B'; };
const capEl = document.getElementById('cap');
const hostEl = document.getElementById('host'), portEl = document.getElementById('port'), loginEl = document.getElementById('login');
const postCfg = patch => fetch('/config', { method:'POST', headers:{'content-type':'application/json'}, body: JSON.stringify(patch) });
capEl.addEventListener('change', () => { const v=Number(capEl.value); if(v>=0) postCfg({ maxPerUser: v }); });
hostEl.addEventListener('change', () => postCfg({ mcHost: hostEl.value }));
portEl.addEventListener('change', () => { const p=Number(portEl.value); if(p>0) postCfg({ mcPort: p }); });
loginEl.addEventListener('change', () => postCfg({ loginMessage: loginEl.value }));
async function tick(){
  try {
    const [bots, disp, cfg] = await Promise.all([
      fetch('/bots').then(r=>r.json()),
      fetch('/dispatcher').then(r=>r.json()).catch(()=>({})),
      fetch('/config').then(r=>r.json()).catch(()=>({})),
    ]);
    if (document.activeElement !== capEl && cfg.maxPerUser != null) capEl.value = cfg.maxPerUser;
    if (document.activeElement !== hostEl && cfg.mcHost != null) hostEl.value = cfg.mcHost;
    if (document.activeElement !== portEl && cfg.mcPort != null) portEl.value = cfg.mcPort;
    if (document.activeElement !== loginEl && cfg.loginMessage != null) loginEl.value = cfg.loginMessage;
    document.getElementById('disp').textContent = disp.username ? disp.username+(disp.online?' ●':' ○') : '—';
    document.getElementById('total').textContent = bots.length;
    document.getElementById('active').textContent = bots.filter(b=>b.state==='working').length;
    document.getElementById('tok').textContent = k(bots.reduce((s,b)=>s+(b.tokensIn||0)+(b.tokensOut||0),0));
    document.getElementById('net').textContent = fb(bots.reduce((s,b)=>s+(b.netIn||0)+(b.netOut||0),0)+(disp.netIn||0)+(disp.netOut||0));
    document.getElementById('upd').textContent = new Date().toLocaleTimeString();
    bots.sort((a,b)=> (a.username).localeCompare(b.username, undefined, {numeric:true}));
    document.getElementById('rows').innerHTML = bots.map(b => {
      const hp = b.health==null?'—':Math.round(b.health), food = b.food==null?'—':Math.round(b.food);
      const g = b.goal ? b.goal.replace(/</g,'&lt;') : '<span class=muted>—</span>';
      return '<tr>'+
        '<td class=name><span class="dot '+b.state+'"></span>'+b.username+'</td>'+
        '<td class=owner>'+(b.owner||'<span class=muted>unowned</span>')+'</td>'+
        '<td>'+b.state+'</td>'+
        '<td class=goal title="'+(b.goal||'').replace(/"/g,'&quot;')+'">'+g+'</td>'+
        '<td class=num>'+(b.step||0)+'</td>'+
        '<td class=num>'+(b.convSteps||0)+'</td>'+
        '<td class=num>'+k(b.tokensIn)+' / '+k(b.tokensOut)+'</td>'+
        '<td class=cache>'+k(b.cacheReadTokens)+'</td>'+
        '<td class=num>'+fb(b.netIn)+' / '+fb(b.netOut)+'</td>'+
        '<td class=num>'+hp+' / '+food+'</td>'+
      '</tr>';
    }).join('') || '<tr><td colspan=10 class=muted>no agents — summon with @agents in game, or POST /summon</td></tr>';
  } catch (e) { document.getElementById('upd').textContent = 'error'; }
}
tick(); setInterval(tick, 1500);
</script>`;

export function createApi(manager: BotManager): express.Express {
  const app = express();
  app.use(express.json());

  app.get("/health", (_req: Request, res: Response) => {
    res.json({ status: "ok" });
  });

  app.get("/dispatcher", (_req, res) => {
    res.json(manager.dispatcherStatus());
  });

  app.get("/config", (_req, res) => {
    res.json(manager.getSettings());
  });

  // Live-editable settings; no restart. Host/port/login changes reconnect the fleet.
  app.post("/config", (req, res) => {
    const b = req.body ?? {};
    const patch: { maxPerUser?: number; mcHost?: string; mcPort?: number; loginMessage?: string } = {};
    if (b.maxPerUser !== undefined) {
      const n = Number(b.maxPerUser);
      if (!Number.isFinite(n) || n < 0) return res.status(400).json({ error: "maxPerUser must be a number >= 0" });
      patch.maxPerUser = n;
    }
    if (b.mcHost !== undefined) {
      if (typeof b.mcHost !== "string") return res.status(400).json({ error: "mcHost must be a string" });
      patch.mcHost = b.mcHost.trim();
    }
    if (b.mcPort !== undefined) {
      const p = Number(b.mcPort);
      if (!Number.isInteger(p) || p < 1 || p > 65535) return res.status(400).json({ error: "mcPort must be 1-65535" });
      patch.mcPort = p;
    }
    if (b.loginMessage !== undefined) {
      if (typeof b.loginMessage !== "string") return res.status(400).json({ error: "loginMessage must be a string" });
      patch.loginMessage = b.loginMessage;
    }
    manager.updateSettings(patch);
    res.json(manager.getSettings());
  });

  app.get("/bots", (_req, res) => {
    res.json(manager.list());
  });

  app.get("/bots/:name", (req, res) => {
    const agent = manager.get(req.params.name);
    if (!agent) return res.status(404).json({ error: "no such bot" });
    res.json(agent.status());
  });

  // Summon N fresh workers on one goal (admin channel; owner recorded as "api").
  app.post("/summon", (req, res) => {
    const count = Number(req.body?.count);
    const goal = req.body?.goal;
    if (!Number.isInteger(count) || count < 1) return res.status(400).json({ error: "body needs { count: positive integer }" });
    if (typeof goal !== "string") return res.status(400).json({ error: "body needs { goal: string }" });
    res.json(manager.createNew(count, goal, "api"));
  });

  // Retask an existing worker (admin: no owner check); reconnects it if logged out.
  app.post("/bots/:name/goal", (req, res) => {
    const agent = manager.get(req.params.name);
    if (!agent) return res.status(404).json({ error: "no such bot" });
    const goal = req.body?.goal;
    if (typeof goal !== "string") return res.status(400).json({ error: "body needs { goal: string }" });
    if (!agent.assign(goal)) return res.status(409).json({ error: "bot is busy; stop it to reassign" });
    res.json({ ok: true });
  });

  app.post("/bots/:name/chat", (req, res) => {
    const agent = manager.get(req.params.name);
    if (!agent) return res.status(404).json({ error: "no such bot" });
    const message = req.body?.message;
    if (typeof message !== "string") return res.status(400).json({ error: "body needs { message: string }" });
    agent.chat(message);
    res.json({ ok: true });
  });

  app.post("/bots/:name/stop", (req, res) => {
    const agent = manager.get(req.params.name);
    if (!agent) return res.status(404).json({ error: "no such bot" });
    agent.stop();
    res.json({ ok: true });
  });

  app.get("/", (_req, res) => {
    res.type("html").send(DASHBOARD);
  });

  return app;
}
