//! Deterministic interpreter for saved routines: composes existing skills with
//! repeat/until/when control flow. No eval — steps are plain data, tools are whitelisted.

use crate::skill::{BotView, Exec};
use futures::future::BoxFuture;
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

#[derive(Debug)]
pub struct RoutineError(pub String);

impl std::fmt::Display for RoutineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for RoutineError {}

/// Comparison operators over numbers (i64 inv/find counts and f32 vitals both widen to f64).
fn cmp_op(op: &str) -> Option<fn(f64, f64) -> bool> {
    Some(match op {
        ">=" => |a, b| a >= b,
        "<=" => |a, b| a <= b,
        "==" => |a, b| a == b,
        "!=" => |a, b| a != b,
        ">" => |a, b| a > b,
        "<" => |a, b| a < b,
        _ => return None,
    })
}

fn cond_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(.+?)(>=|<=|==|!=|>|<)(-?\d+)$").unwrap())
}

fn param_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{(\w+)\}").unwrap())
}

fn err_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(error|unknown|cannot|no such|not carrying)").unwrap())
}

/// Evaluate a resolved condition: "have:cobblestone>=64", "find:iron_ore==0", "health<8", "food<10".
pub fn eval_condition(view: &dyn BotView, cond: &str) -> bool {
    let stripped: String = cond.chars().filter(|c| !c.is_whitespace()).collect();
    let caps = match cond_re().captures(&stripped) {
        Some(c) => c,
        None => return false,
    };
    let left = &caps[1];
    let op = &caps[2];
    let num: f64 = caps[3].parse().unwrap_or(0.0);
    let lhs: f64 = if let Some(item) = left.strip_prefix("have:") {
        view.inv_count(item) as f64
    } else if let Some(block) = left.strip_prefix("find:") {
        view.nearby_count(block) as f64
    } else if left == "health" {
        view.health() as f64
    } else if left == "food" {
        view.food() as f64
    } else {
        return false;
    };
    cmp_op(op).map_or(false, |f| f(lhs, num))
}

/// JS `String(v)` for substitution: strings verbatim, primitives stringified, else JSON.
fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => v.to_string(),
    }
}

/// Substitute `{param}` placeholders in a string; unknown params stay `{param}`.
fn subst(s: &str, args: &Map<String, Value>) -> String {
    param_re()
        .replace_all(s, |c: &regex::Captures| match args.get(&c[1]) {
            Some(v) => js_string(v),
            None => format!("{{{}}}", &c[1]),
        })
        .into_owned()
}

/// Substitute `{param}` placeholders from args, recursively, in strings/arrays/objects.
pub fn resolve(value: &Value, args: &Map<String, Value>) -> Value {
    match value {
        Value::String(s) => Value::String(subst(s, args)),
        Value::Array(a) => Value::Array(a.iter().map(|v| resolve(v, args)).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), resolve(v, args))).collect())
        }
        _ => value.clone(),
    }
}

/// Tool names referenced anywhere in a step tree — used to validate a routine before saving.
pub fn referenced_tools(steps: &[Value]) -> HashSet<String> {
    let mut out = HashSet::new();
    collect_tools(steps, &mut out);
    out
}

fn collect_tools(steps: &[Value], out: &mut HashSet<String>) {
    for s in steps {
        if let Some(o) = s.as_object() {
            if let Some(Value::String(t)) = o.get("tool") {
                out.insert(t.clone());
            }
            if let Some(Value::Array(d)) = o.get("do") {
                collect_tools(d, out);
            }
            if let Some(Value::Array(e)) = o.get("else") {
                collect_tools(e, out);
            }
        }
    }
}

pub struct Budget {
    pub steps: u32,
    pub max: u32,
}

// TODO(verify): held as `Arc<dyn BotView + Send + Sync>` — auto-trait bounds are added at the
// object site, so skill.rs BotView needs no `: Send + Sync` supertrait for this to spawn/await.
pub struct RunCtx {
    pub exec: Exec,
    pub view: Arc<dyn BotView + Send + Sync>,
    pub budget: Budget,
    pub deadline: Instant,
    pub log: Vec<String>,
    /// Live progress sink (agent activity log); receives each control-flow entry and tool step.
    pub note: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// When set and it returns true, abort between steps so the planner can react (owner prompt / damage).
    pub interrupt: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

/// Record a line to the summary log and stream it live.
fn emit(ctx: &mut RunCtx, msg: String) {
    ctx.log.push(msg.clone());
    if let Some(n) = &ctx.note {
        n(&msg);
    }
}

/// JS `Number(x) || fallback`: numeric x (0 falsy) else fallback.
fn num_or(v: Option<&Value>, fallback: i64) -> i64 {
    match v.and_then(|v| v.as_f64()) {
        Some(n) if n != 0.0 => n as i64,
        _ => fallback,
    }
}

fn run_step<'a>(
    step: &'a Value,
    args: &'a Map<String, Value>,
    ctx: &'a mut RunCtx,
) -> BoxFuture<'a, Result<(), RoutineError>> {
    Box::pin(async move {
        if Instant::now() > ctx.deadline {
            return Err(RoutineError("time budget exhausted".to_string()));
        }
        if ctx.budget.steps >= ctx.budget.max {
            return Err(RoutineError(format!("step budget ({}) exhausted", ctx.budget.max)));
        }
        if let Some(intr) = &ctx.interrupt {
            if intr() {
                return Err(RoutineError("interrupted by an owner message or damage".to_string()));
            }
        }
        let obj = match step.as_object() {
            Some(o) => o,
            None => return Ok(()),
        };

        if let Some(Value::Array(do_steps)) = obj.get("do") {
            if let Some(Value::String(until)) = obj.get("until") {
                let cond = subst(until, args);
                let max = num_or(obj.get("max"), 64).min(256);
                emit(ctx, format!("until {cond} (max {max})"));
                let mut i = 0;
                while i < max && !eval_condition(&*ctx.view, &cond) {
                    run_steps(do_steps, args, ctx).await?;
                    i += 1;
                }
                return Ok(());
            }
            if let Some(rep) = obj.get("repeat").and_then(|v| v.as_f64()) {
                let n = (rep as i64).min(256);
                emit(ctx, format!("repeat {n}x"));
                for _ in 0..n {
                    run_steps(do_steps, args, ctx).await?;
                }
                return Ok(());
            }
            if let Some(Value::String(when)) = obj.get("when") {
                let cond = subst(when, args);
                let ok = eval_condition(&*ctx.view, &cond);
                emit(ctx, format!("when {cond} → {}", if ok { "do" } else { "else" }));
                if ok {
                    run_steps(do_steps, args, ctx).await?;
                } else if let Some(Value::Array(else_steps)) = obj.get("else") {
                    run_steps(else_steps, args, ctx).await?;
                }
                return Ok(());
            }
            run_steps(do_steps, args, ctx).await?; // bare group
            return Ok(());
        }

        if let Some(Value::String(tool)) = obj.get("tool") {
            ctx.budget.steps += 1;
            let raw = obj.get("args").cloned().unwrap_or_else(|| Value::Object(Map::new()));
            let resolved = resolve(&raw, args);
            let fut = (ctx.exec)(tool.clone(), resolved);
            let result = fut.await;
            emit(ctx, format!("{tool} -> {result}"));
            let stop = obj.get("stop_on_error").and_then(|v| v.as_bool()).unwrap_or(false);
            if stop && err_re().is_match(&result) {
                return Err(RoutineError(format!("step {tool} failed: {result}")));
            }
        }
        Ok(())
    })
}

pub fn run_steps<'a>(
    steps: &'a [Value],
    args: &'a Map<String, Value>,
    ctx: &'a mut RunCtx,
) -> BoxFuture<'a, Result<(), RoutineError>> {
    Box::pin(async move {
        for s in steps {
            run_step(s, args, ctx).await?;
        }
        Ok(())
    })
}
