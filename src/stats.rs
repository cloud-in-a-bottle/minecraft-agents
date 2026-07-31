//! Process CPU/memory sampling + aggregate LLM API rate, for the dashboard `/metrics` endpoint.
//! /proc-based (Linux runtime); returns zeros on non-Linux dev so the dashboard degrades to n/a.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

// Cumulative LLM counters — bumped by llm.rs on each planner request.
static LLM_REQUESTS: AtomicU64 = AtomicU64::new(0);
static LLM_TOKENS: AtomicU64 = AtomicU64::new(0);

// Sampled gauges — updated by the 2s sampler task.
static CPU_PERMILLE: AtomicU64 = AtomicU64::new(0); // % of total capacity × 10
static RSS_KB: AtomicU64 = AtomicU64::new(0);
static MEM_LIMIT_KB: AtomicU64 = AtomicU64::new(0); // cgroup limit; 0 = unknown
static LLM_RPM: AtomicU64 = AtomicU64::new(0);
static LLM_TPM: AtomicU64 = AtomicU64::new(0);

const CLK_TCK: f64 = 100.0; // Linux _SC_CLK_TCK default

/// Record one planner API call and the tokens it consumed (input + output).
pub fn record_llm(tokens: u64) {
    LLM_REQUESTS.fetch_add(1, Ordering::Relaxed);
    LLM_TOKENS.fetch_add(tokens, Ordering::Relaxed);
}

#[derive(Serialize)]
pub struct Metrics {
    pub cpu_pct: f64,          // 0–100 of total (all-core) capacity
    pub mem_mb: f64,           // resident set size
    pub mem_pct: Option<f64>,  // RSS / cgroup limit, when the limit is known
    pub llm_rpm: u64,          // requests/min (rolling, whole fleet)
    pub llm_tpm: u64,          // tokens/min (rolling, whole fleet)
}

pub fn snapshot() -> Metrics {
    let rss_kb = RSS_KB.load(Ordering::Relaxed);
    let limit_kb = MEM_LIMIT_KB.load(Ordering::Relaxed);
    Metrics {
        cpu_pct: CPU_PERMILLE.load(Ordering::Relaxed) as f64 / 10.0,
        mem_mb: rss_kb as f64 / 1024.0,
        mem_pct: (limit_kb > 0).then(|| rss_kb as f64 / limit_kb as f64 * 100.0),
        llm_rpm: LLM_RPM.load(Ordering::Relaxed),
        llm_tpm: LLM_TPM.load(Ordering::Relaxed),
    }
}

/// Spawn the sampler: every 2s recompute process CPU%, RSS, and per-minute LLM rates from deltas.
pub fn spawn_sampler() {
    MEM_LIMIT_KB.store(read_mem_limit_kb(), Ordering::Relaxed);
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
    tokio::spawn(async move {
        let mut last = Instant::now();
        let mut last_ticks = read_cpu_ticks();
        let (mut last_reqs, mut last_toks) =
            (LLM_REQUESTS.load(Ordering::Relaxed), LLM_TOKENS.load(Ordering::Relaxed));
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let now = Instant::now();
            let dt = now.duration_since(last).as_secs_f64().max(0.001);
            last = now;

            let ticks = read_cpu_ticks();
            let cpu_secs = ticks.saturating_sub(last_ticks) as f64 / CLK_TCK;
            last_ticks = ticks;
            let cpu_pct = (cpu_secs / dt / cores * 100.0).clamp(0.0, 100.0);
            CPU_PERMILLE.store((cpu_pct * 10.0) as u64, Ordering::Relaxed);

            RSS_KB.store(read_rss_kb(), Ordering::Relaxed);

            let (reqs, toks) =
                (LLM_REQUESTS.load(Ordering::Relaxed), LLM_TOKENS.load(Ordering::Relaxed));
            LLM_RPM.store((reqs.saturating_sub(last_reqs) as f64 / dt * 60.0) as u64, Ordering::Relaxed);
            LLM_TPM.store((toks.saturating_sub(last_toks) as f64 / dt * 60.0) as u64, Ordering::Relaxed);
            (last_reqs, last_toks) = (reqs, toks);
        }
    });
}

/// Process CPU time (utime + stime) in clock ticks, from /proc/self/stat.
fn read_cpu_ticks() -> u64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // comm (field 2) may contain spaces/parens; fields resume after the last ')'.
    let Some(rp) = s.rfind(')') else { return 0 };
    let f: Vec<&str> = s[rp + 1..].split_whitespace().collect();
    // post-')' index 0 = state (field 3); utime = field 14 → 11, stime = field 15 → 12.
    let utime = f.get(11).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
    let stime = f.get(12).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
    utime + stime
}

/// Resident set size (kB) from /proc/self/status.
fn read_rss_kb() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(0);
        }
    }
    0
}

/// Container memory limit (kB) from cgroup v2 then v1; 0 if unlimited/unknown.
fn read_mem_limit_kb() -> u64 {
    if let Ok(s) = std::fs::read_to_string("/sys/fs/cgroup/memory.max") {
        let t = s.trim();
        if t != "max" {
            if let Ok(b) = t.parse::<u64>() {
                return b / 1024;
            }
        }
    }
    if let Ok(s) = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes") {
        if let Ok(b) = s.trim().parse::<u64>() {
            if b < (1u64 << 62) {
                return b / 1024;
            }
        }
    }
    0
}
