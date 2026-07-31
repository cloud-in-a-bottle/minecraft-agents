//! Drives bot-authored reactive rules: on each tick, fire any rule whose condition holds.

use crate::routines::{eval_condition, run_steps, Budget, RunCtx};
use crate::skill::{BotView, Exec, Rule};
use parking_lot::Mutex;
use serde_json::Map;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

type Note = Arc<dyn Fn(&str) + Send + Sync>;

/// Interior-mutable run/cooldown tracking, shared into each spawned rule task.
pub struct RuleEngine {
    running: Arc<Mutex<HashSet<String>>>,
    cooldown_until: Arc<Mutex<HashMap<String, Instant>>>,
}

impl Default for RuleEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl RuleEngine {
    pub fn new() -> Self {
        Self {
            running: Arc::new(Mutex::new(HashSet::new())),
            cooldown_until: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Fire any enabled, not-running, off-cooldown rule whose condition holds. Errors are swallowed
    /// so one bad rule can't stop the tick.
    pub fn tick(
        &self,
        rules: &[Rule],
        view: Arc<dyn BotView + Send + Sync>,
        exec: Exec,
        note: Option<Note>,
    ) {
        for rule in rules {
            if !rule.enabled || self.running.lock().contains(&rule.name) {
                continue;
            }
            if let Some(until) = self.cooldown_until.lock().get(&rule.name) {
                if Instant::now() < *until {
                    continue;
                }
            }
            if !eval_condition(&*view, &rule.condition) {
                continue;
            }

            self.running.lock().insert(rule.name.clone());
            if let Some(n) = &note {
                n(&format!("⚙ setting \"{}\" fired ({})", rule.name, rule.condition));
            }

            let name = rule.name.clone();
            let step_note: Option<Note> = note.clone().map(|n| {
                let rn = name.clone();
                Arc::new(move |m: &str| n(&format!("⚙ {rn}: {m}"))) as Note
            });
            let steps = rule.steps.clone();
            let view = view.clone();
            let exec = exec.clone();
            let running = self.running.clone();
            let cooldown = self.cooldown_until.clone();

            tokio::spawn(async move {
                let mut ctx = RunCtx {
                    exec,
                    view,
                    budget: Budget { steps: 0, max: 100 },
                    deadline: Instant::now() + Duration::from_secs(60),
                    log: Vec::new(),
                    note: step_note,
                    interrupt: None, // background rule; independent of the planner's inject queue
                };
                let args = Map::new();
                let _ = run_steps(&steps, &args, &mut ctx).await; // swallow: a failing rule must not crash the tick
                running.lock().remove(&name);
                cooldown.lock().insert(name, Instant::now() + Duration::from_secs(10));
            });
        }
    }
}
