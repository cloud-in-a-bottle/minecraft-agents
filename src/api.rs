//! HTTP control API + embedded dashboard (port of api.ts). axum only, azalea-free.
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::manager::BotManager;
use crate::types::{
    AgentStatus, AssignOutcome, CreateResult, DispatcherStatus, RejectReason, Settings,
    SettingsPatch,
};

const DASHBOARD: &str = r#"<!doctype html><meta charset=utf-8>
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
  header select { background:#0e1116; color:#d6dde6; border:1px solid #2b333d; border-radius:4px; padding:2px 5px; font:inherit; }
  header button { background:#238636; color:#fff; border:1px solid #2ea043; border-radius:4px; padding:3px 12px; font:inherit; cursor:pointer; }
  header button:disabled { background:#21262d; color:#6e7681; border-color:#2b333d; cursor:default; }
  .conn { display:inline-block; width:9px; height:9px; border-radius:50%; margin-right:6px; vertical-align:middle; }
  .conn.on{background:#3fb950} .conn.off{background:#f85149}
  #rows tr { cursor:pointer; }
  .clickable { cursor:pointer; text-decoration:underline dotted; text-underline-offset:3px; }
  #overlay { position:fixed; inset:0; background:rgba(0,0,0,.55); display:none; z-index:10; }
  #overlay.show { display:block; }
  #detail { position:absolute; top:0; right:0; width:min(640px,100%); height:100%; background:#0e1116; border-left:1px solid #232a34; display:flex; flex-direction:column; }
  #detail .dhead { padding:10px 14px; border-bottom:1px solid #232a34; display:flex; gap:12px; align-items:baseline; }
  #detail .dhead b { color:#fff; font-size:14px; } #detail .dhead .x { margin-left:auto; cursor:pointer; color:#8b97a6; font-size:18px; }
  #dlog { flex:1; overflow-y:auto; margin:0; padding:10px 14px; white-space:pre-wrap; word-break:break-word; font-size:12px; color:#c2ccd6; }
  #dlog .srv{color:#d29922} #dlog .err{color:#f85149} #dlog .tool{color:#7ee787} #dlog .think{color:#a5a5ff}
</style>
<header>
  <b>minecraft-agents</b>
  <span class=stat><span id=conn class="conn off"></span><span id=disp class=clickable title="view dispatcher log">connecting…</span></span>
  <span class=stat>agents <span id=total>0</span></span>
  <span class=stat>active <span id=active>0</span></span>
  <span class=stat>tokens <span id=tok>0</span></span>
  <span class=stat>traffic <span id=net>0</span></span>
  <span class=stat title="process CPU, % of all-core capacity">cpu <span id=cpu>—</span></span>
  <span class=stat title="resident memory (and % of the container limit)">mem <span id=mem>—</span></span>
  <span class=stat title="fleet LLM API rate (rolling): requests/min · tokens/min">llm <span id=llm>—</span></span>
  <span class=stat>server <input id=host placeholder=host style="width:200px"> : <input id=port type=number style="width:78px"></span>
  <span class=stat>login <input id=login type=text placeholder="/login <pw>" style="width:200px" title="sent on join; two-step with &&, e.g. /register <pw> <pw> && /login <pw>"></span>
  <span class=stat>per-user cap <input id=cap type=number min=0 title="0 = unlimited; applies to next summon"></span>
  <span class=stat>model <select id=model title="planner for new tasks"></select></span>
  <span class=stat>max steps <input id=maxsteps type=number min=1 max=1000 title="skill calls per goal (1-1000)"></span>
  <button id=apply disabled title="apply staged settings (reconnects the fleet only if host/port/login changed)">apply</button>
  <span class=stat muted id=upd></span>
</header>
<div id=wrap><table>
  <thead><tr><th>agent</th><th>owner</th><th>state</th><th>goal</th><th>step</th><th>conv</th><th>tok in/out</th><th>cache</th><th>net ↓/↑</th><th>hp/food</th></tr></thead>
  <tbody id=rows></tbody>
</table></div>
<div id=overlay><div id=detail>
  <div class=dhead><b id=dname></b><span id=dstate class=muted></span><span class=x id=dclose>×</span></div>
  <pre id=dlog></pre>
</div></div>
<script>
const k = n => n>=1000 ? (n/1000).toFixed(n>=10000?0:1)+'k' : String(n||0);
const fb = n => { n=n||0; if(n>=1e9)return (n/1e9).toFixed(2)+'GB'; if(n>=1e6)return (n/1e6).toFixed(1)+'MB'; if(n>=1e3)return (n/1e3).toFixed(0)+'KB'; return n+'B'; };
const capEl = document.getElementById('cap');
const modelEl = document.getElementById('model');
const stepsEl = document.getElementById('maxsteps');
const hostEl = document.getElementById('host'), portEl = document.getElementById('port'), loginEl = document.getElementById('login');
const applyEl = document.getElementById('apply');
const postCfg = patch => fetch('/config', { method:'POST', headers:{'content-type':'application/json'}, body: JSON.stringify(patch) });
// All settings are staged and applied together via the apply button.
let dirty = false;
const markDirty = () => { dirty = true; applyEl.disabled = false; applyEl.textContent = 'apply'; };
[hostEl, portEl, loginEl, capEl, stepsEl].forEach(el => el.addEventListener('input', markDirty));
modelEl.addEventListener('change', markDirty);
applyEl.addEventListener('click', async () => {
  const p = Number(portEl.value);
  if (!Number.isInteger(p) || p < 1 || p > 65535) { applyEl.textContent = 'bad port'; return; }
  const cap = Number(capEl.value);
  if (!Number.isInteger(cap) || cap < 0) { applyEl.textContent = 'bad cap'; return; }
  const steps = Number(stepsEl.value);
  if (!Number.isInteger(steps) || steps < 1 || steps > 1000) { applyEl.textContent = 'bad steps'; return; }
  applyEl.disabled = true; applyEl.textContent = 'applying…';
  const r = await postCfg({ mcHost: hostEl.value.trim(), mcPort: p, loginMessage: loginEl.value, maxPerUser: cap, model: modelEl.value, maxSteps: steps }).catch(() => null);
  if (r && r.ok) { dirty = false; applyEl.textContent = 'applied'; }
  else { applyEl.disabled = false; applyEl.textContent = 'failed'; }
});
// ---- detail panel: click an agent (or the dispatcher) to read its log/conversation ----
let selected = null; // agent username, or '__dispatcher__'
let lastLogHtml = null; // skip DOM rebuilds when the log text is unchanged
const overlay = document.getElementById('overlay'), dlog = document.getElementById('dlog');
const esc = s => (s||'').replace(/&/g,'&amp;').replace(/</g,'&lt;');
const lineClass = l => l.includes(' srv: ')?'srv' : /error|kicked|gave up|failed/.test(l)?'err' : l.includes(' thinks: ')?'think' : /s->s/.test(l)?'tool' : '';
function openDetail(name){ selected = name; lastLogHtml = null; overlay.classList.add('show'); renderDetail(true); }
function closeDetail(){ selected = null; overlay.classList.remove('show'); }
document.getElementById('dclose').addEventListener('click', closeDetail);
overlay.addEventListener('click', e => { if (e.target === overlay) closeDetail(); });
document.addEventListener('keydown', e => { if (e.key === 'Escape') closeDetail(); });
document.getElementById('rows').addEventListener('click', e => {
  const tr = e.target.closest('tr[data-name]'); if (tr) openDetail(tr.dataset.name);
});
document.getElementById('disp').addEventListener('click', () => openDetail('__dispatcher__'));
async function renderDetail(reset){
  if (selected == null) return;
  const isDisp = selected === '__dispatcher__';
  const s = await fetch(isDisp ? '/dispatcher' : '/bots/'+encodeURIComponent(selected)).then(r=>r.json()).catch(()=>null);
  if (selected == null) return;
  document.getElementById('dname').textContent = isDisp ? (s&&s.username||'dispatcher') : selected;
  document.getElementById('dstate').textContent = s ? (isDisp ? (s.online?'connected':'disconnected') : (s.state||'') + (s.goal?(' — '+s.goal):'')) : 'unavailable';
  const log = (s&&s.log)||[];
  const html = log.map(l => '<span class="'+lineClass(l)+'">'+esc(l)+'</span>').join('\n') || '<span class=muted>no activity yet</span>';
  // Don't rebuild the DOM when nothing changed, or while text is selected (rebuilding clears the selection mid-copy).
  const sel = window.getSelection();
  const selecting = sel && !sel.isCollapsed && dlog.contains(sel.anchorNode);
  if (!reset && (html === lastLogHtml || selecting)) return;
  const atBottom = dlog.scrollHeight - dlog.scrollTop - dlog.clientHeight < 40;
  lastLogHtml = html;
  dlog.innerHTML = html;
  if (reset || atBottom) dlog.scrollTop = dlog.scrollHeight;
}
async function tick(){
  try {
    const [bots, disp, cfg, met] = await Promise.all([
      fetch('/bots').then(r=>r.json()),
      fetch('/dispatcher').then(r=>r.json()).catch(()=>({})),
      fetch('/config').then(r=>r.json()).catch(()=>({})),
      fetch('/metrics').then(r=>r.json()).catch(()=>({})),
    ]);
    if (Array.isArray(cfg.models) && modelEl.options.length !== cfg.models.length)
      modelEl.innerHTML = cfg.models.map(m => '<option value="'+m+'">'+m+'</option>').join('');
    if (!dirty) {  // don't clobber staged edits before they're applied
      if (cfg.mcHost != null) hostEl.value = cfg.mcHost;
      if (cfg.mcPort != null) portEl.value = cfg.mcPort;
      if (cfg.loginMessage != null) loginEl.value = cfg.loginMessage;
      if (cfg.maxPerUser != null) capEl.value = cfg.maxPerUser;
      if (cfg.model != null) modelEl.value = cfg.model;
      if (cfg.maxSteps != null) stepsEl.value = cfg.maxSteps;
    }
    const online = !!disp.online;
    document.getElementById('conn').className = 'conn ' + (online?'on':'off');
    document.getElementById('disp').textContent = disp.username
      ? disp.username + (online ? ' connected' : ' disconnected')
      : 'no dispatcher';
    document.getElementById('total').textContent = bots.length;
    document.getElementById('active').textContent = bots.filter(b=>b.state==='working').length;
    document.getElementById('tok').textContent = k(bots.reduce((s,b)=>s+(b.tokensIn||0)+(b.tokensOut||0),0));
    document.getElementById('net').textContent = fb(bots.reduce((s,b)=>s+(b.netIn||0)+(b.netOut||0),0)+(disp.netIn||0)+(disp.netOut||0));
    document.getElementById('cpu').textContent = met.cpu_pct!=null ? met.cpu_pct.toFixed(0)+'%' : 'n/a';
    document.getElementById('mem').textContent = met.mem_mb!=null ? Math.round(met.mem_mb)+'MB'+(met.mem_pct!=null?' ('+met.mem_pct.toFixed(0)+'%)':'') : 'n/a';
    document.getElementById('llm').textContent = (met.llm_rpm||0)+'/min · '+k(met.llm_tpm||0)+' tok/min';
    document.getElementById('upd').textContent = new Date().toLocaleTimeString();
    bots.sort((a,b)=> (a.username).localeCompare(b.username, undefined, {numeric:true}));
    document.getElementById('rows').innerHTML = bots.map(b => {
      const hp = b.health==null?'—':Math.round(b.health), food = b.food==null?'—':Math.round(b.food);
      const g = b.goal ? b.goal.replace(/</g,'&lt;') : '<span class=muted>—</span>';
      return '<tr data-name="'+b.username+'">'+
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
    if (selected != null) renderDetail(false);
  } catch (e) { document.getElementById('upd').textContent = 'error'; }
}
tick(); setInterval(tick, 1500);
</script>"#;

type Mgr = State<Arc<BotManager>>;

pub fn create_api(manager: Arc<BotManager>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/dispatcher", get(dispatcher))
        .route("/config", get(get_config).post(post_config))
        .route("/bots", get(list_bots))
        .route("/bots/:name", get(get_bot))
        .route("/summon", post(summon))
        .route("/bots/:name/goal", post(assign_goal))
        .route("/bots/:name/chat", post(chat))
        .route("/bots/:name/stop", post(stop))
        .route("/dev/reset", post(dev_reset))
        .with_state(manager)
}

// ---- helpers ----

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

fn body_json(bytes: &Bytes) -> Value {
    serde_json::from_slice::<Value>(bytes).unwrap_or(Value::Null)
}

/// JS `Number(x)` coercion for the numeric config fields.
fn js_number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::Null => 0.0,
        _ => f64::NAN,
    }
}

fn is_int(n: f64) -> bool {
    n.is_finite() && n.fract() == 0.0
}

/// serde shim for CreateResult (types.rs is not editable).
#[derive(Serialize)]
struct CreateResultJson {
    created: Vec<String>,
    rejected: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

impl From<CreateResult> for CreateResultJson {
    fn from(r: CreateResult) -> Self {
        CreateResultJson {
            created: r.created,
            rejected: r.rejected,
            reason: r.reason.map(|x| match x {
                RejectReason::AtCapacity => "at_capacity",
                RejectReason::UserLimit => "user_limit",
            }),
        }
    }
}

// ---- routes ----

async fn index() -> Html<&'static str> {
    Html(DASHBOARD)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn metrics() -> Json<crate::stats::Metrics> {
    Json(crate::stats::snapshot())
}

async fn dispatcher(State(m): Mgr) -> Json<DispatcherStatus> {
    Json(m.dispatcher_status())
}

async fn get_config(State(m): Mgr) -> Json<Settings> {
    Json(m.get_settings())
}

// Live-editable settings; no restart. Host/port/login changes reconnect the fleet.
async fn post_config(State(m): Mgr, body: Bytes) -> Response {
    let b = body_json(&body);
    let mut patch = SettingsPatch::default();

    if let Some(v) = b.get("maxPerUser") {
        let n = js_number(v);
        if !n.is_finite() || n < 0.0 {
            return err(StatusCode::BAD_REQUEST, "maxPerUser must be a number >= 0");
        }
        patch.max_per_user = Some(n as usize);
    }
    if let Some(v) = b.get("mcHost") {
        match v.as_str() {
            Some(s) => patch.mc_host = Some(s.trim().to_string()),
            None => return err(StatusCode::BAD_REQUEST, "mcHost must be a string"),
        }
    }
    if let Some(v) = b.get("mcPort") {
        let p = js_number(v);
        if !is_int(p) || p < 1.0 || p > 65535.0 {
            return err(StatusCode::BAD_REQUEST, "mcPort must be 1-65535");
        }
        patch.mc_port = Some(p as u16);
    }
    if let Some(v) = b.get("loginMessage") {
        match v.as_str() {
            Some(s) => patch.login_message = Some(s.to_string()),
            None => return err(StatusCode::BAD_REQUEST, "loginMessage must be a string"),
        }
    }
    if let Some(v) = b.get("model") {
        match v.as_str() {
            Some(s) => patch.model = Some(s.to_string()),
            None => return err(StatusCode::BAD_REQUEST, "model must be a string"),
        }
    }
    if let Some(v) = b.get("maxSteps") {
        let n = js_number(v);
        if !is_int(n) || n < 1.0 || n > 1000.0 {
            return err(StatusCode::BAD_REQUEST, "maxSteps must be 1-1000");
        }
        patch.max_steps = Some(n as u32);
    }

    match m.update_settings(patch) {
        Ok(()) => Json(m.get_settings()).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

async fn list_bots(State(m): Mgr) -> Json<Vec<AgentStatus>> {
    Json(m.list())
}

async fn get_bot(State(m): Mgr, Path(name): Path<String>) -> Response {
    match m.status(&name) {
        Some(s) => Json(s).into_response(),
        None => err(StatusCode::NOT_FOUND, "no such bot"),
    }
}

// Summon N fresh workers on one goal (admin channel; owner recorded as "api").
async fn summon(State(m): Mgr, body: Bytes) -> Response {
    let b = body_json(&body);
    let count = js_number(b.get("count").unwrap_or(&Value::Null));
    if !is_int(count) || count < 1.0 {
        return err(StatusCode::BAD_REQUEST, "body needs { count: positive integer }");
    }
    let goal = match b.get("goal").and_then(|v| v.as_str()) {
        Some(g) => g,
        None => return err(StatusCode::BAD_REQUEST, "body needs { goal: string }"),
    };
    let res = m.create_new(count as usize, goal, Some("api"));
    Json(CreateResultJson::from(res)).into_response()
}

// Retask an existing worker (admin: no owner check); reconnects it if logged out.
async fn assign_goal(State(m): Mgr, Path(name): Path<String>, body: Bytes) -> Response {
    let b = body_json(&body);
    let goal = match b.get("goal").and_then(|v| v.as_str()) {
        Some(g) => g,
        None => return err(StatusCode::BAD_REQUEST, "body needs { goal: string }"),
    };
    match m.assign(&name, goal) {
        AssignOutcome::NotFound => err(StatusCode::NOT_FOUND, "no such bot"),
        AssignOutcome::Busy => err(StatusCode::CONFLICT, "bot is busy; stop it to reassign"),
        AssignOutcome::Ok => Json(json!({ "ok": true })).into_response(),
    }
}

async fn chat(State(m): Mgr, Path(name): Path<String>, body: Bytes) -> Response {
    let b = body_json(&body);
    let message = match b.get("message").and_then(|v| v.as_str()) {
        Some(msg) => msg,
        None => return err(StatusCode::BAD_REQUEST, "body needs { message: string }"),
    };
    if m.chat(&name, message) {
        Json(json!({ "ok": true })).into_response()
    } else {
        err(StatusCode::NOT_FOUND, "no such bot")
    }
}

async fn stop(State(m): Mgr, Path(name): Path<String>) -> Response {
    if m.stop(&name) {
        Json(json!({ "ok": true })).into_response()
    } else {
        err(StatusCode::NOT_FOUND, "no such bot")
    }
}

// Dev reset: wipe every agent + its memory; keeps live settings and the shared
// library. Guarded by an explicit confirm.
async fn dev_reset(State(m): Mgr, body: Bytes) -> Response {
    let b = body_json(&body);
    if b.get("confirm") != Some(&Value::Bool(true)) {
        return err(
            StatusCode::BAD_REQUEST,
            r#"send { "confirm": true } — this disconnects and forgets all agents"#,
        );
    }
    Json(json!({ "ok": true, "removed": m.wipe_agents() })).into_response()
}
