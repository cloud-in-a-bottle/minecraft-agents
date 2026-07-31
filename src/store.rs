//! SQLite persistence (port of store.ts): live settings, agent ownership, durable per-scope memory.
use crate::skill::{LedgerItem, LedgerStatus, Memory};
use crate::types::Pos;
use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};

pub struct Store {
    conn: Mutex<Connection>,
}

fn status_str(s: LedgerStatus) -> &'static str {
    match s {
        LedgerStatus::Todo => "todo",
        LedgerStatus::Doing => "doing",
        LedgerStatus::Done => "done",
    }
}

fn parse_status(s: &str) -> LedgerStatus {
    match s {
        "doing" => LedgerStatus::Doing,
        "done" => LedgerStatus::Done,
        _ => LedgerStatus::Todo,
    }
}

impl Store {
    pub fn new(path: &str) -> Result<Store> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode = WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings  (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS agents    (username TEXT PRIMARY KEY, owner TEXT);
             CREATE TABLE IF NOT EXISTS waypoints (scope TEXT, name TEXT, x REAL, y REAL, z REAL, PRIMARY KEY (scope, name));
             CREATE TABLE IF NOT EXISTS notes     (scope TEXT, name TEXT, value TEXT, PRIMARY KEY (scope, name));
             CREATE TABLE IF NOT EXISTS ledger    (scope TEXT, text TEXT, status TEXT, PRIMARY KEY (scope, text));",
        )?;
        Ok(Store { conn: Mutex::new(conn) })
    }

    // --- live settings (server host/port, login, per-user cap) ---
    pub fn get_setting(&self, key: &str) -> Option<String> {
        self.conn
            .lock()
            .query_row("SELECT value FROM settings WHERE key=?", [key], |r| r.get(0))
            .optional()
            .unwrap_or(None)
    }
    pub fn set_setting(&self, key: &str, value: &str) {
        self.conn
            .lock()
            .execute(
                "INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )
            .unwrap();
    }

    // --- agent ownership, written on any change ---
    pub fn set_owner(&self, username: &str, owner: Option<&str>) {
        self.conn
            .lock()
            .execute(
                "INSERT INTO agents(username,owner) VALUES(?,?) ON CONFLICT(username) DO UPDATE SET owner=excluded.owner",
                params![username, owner],
            )
            .unwrap();
    }
    pub fn all_agents(&self) -> Vec<(String, Option<String>)> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT username, owner FROM agents").unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))
            .unwrap();
        rows.filter_map(Result::ok).collect()
    }
    /// Dev reset: drop all agents + memory. Keeps settings; the file library is untouched.
    pub fn wipe_agent_data(&self) {
        self.conn
            .lock()
            .execute_batch("DELETE FROM agents; DELETE FROM waypoints; DELETE FROM notes; DELETE FROM ledger;")
            .unwrap();
    }
}

impl Memory for Store {
    fn set_waypoint(&self, scope: &str, name: &str, pos: Pos) {
        self.conn
            .lock()
            .execute(
                "INSERT INTO waypoints(scope,name,x,y,z) VALUES(?,?,?,?,?) ON CONFLICT(scope,name) DO UPDATE SET x=excluded.x,y=excluded.y,z=excluded.z",
                params![scope, name, pos.x, pos.y, pos.z],
            )
            .unwrap();
    }
    fn get_waypoint(&self, scope: &str, name: &str) -> Option<Pos> {
        self.conn
            .lock()
            .query_row(
                "SELECT x,y,z FROM waypoints WHERE scope=? AND name=?",
                params![scope, name],
                |r| Ok(Pos { x: r.get(0)?, y: r.get(1)?, z: r.get(2)? }),
            )
            .optional()
            .unwrap_or(None)
    }
    fn list_waypoints(&self, scope: &str) -> Vec<(String, Pos)> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT name,x,y,z FROM waypoints WHERE scope=? ORDER BY name").unwrap();
        let rows = stmt
            .query_map([scope], |r| {
                Ok((r.get::<_, String>(0)?, Pos { x: r.get(1)?, y: r.get(2)?, z: r.get(3)? }))
            })
            .unwrap();
        rows.filter_map(Result::ok).collect()
    }

    fn set_note(&self, scope: &str, key: &str, text: &str) {
        self.conn
            .lock()
            .execute(
                "INSERT INTO notes(scope,name,value) VALUES(?,?,?) ON CONFLICT(scope,name) DO UPDATE SET value=excluded.value",
                params![scope, key, text],
            )
            .unwrap();
    }
    fn list_notes(&self, scope: &str, query: Option<&str>) -> Vec<(String, String)> {
        let all: Vec<(String, String)> = {
            let conn = self.conn.lock();
            let mut stmt = conn.prepare("SELECT name,value FROM notes WHERE scope=? ORDER BY name").unwrap();
            let rows = stmt
                .query_map([scope], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .unwrap();
            rows.filter_map(Result::ok).collect()
        };
        match query {
            Some(q) => all.into_iter().filter(|(k, v)| k.contains(q) || v.contains(q)).collect(),
            None => all,
        }
    }

    fn ledger(&self, scope: &str) -> Vec<LedgerItem> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT text,status FROM ledger WHERE scope=? ORDER BY rowid").unwrap();
        let rows = stmt
            .query_map([scope], |r| {
                Ok(LedgerItem { text: r.get::<_, String>(0)?, status: parse_status(&r.get::<_, String>(1)?) })
            })
            .unwrap();
        rows.filter_map(Result::ok).collect()
    }
    fn set_ledger_item(&self, scope: &str, text: &str, status: LedgerStatus) -> Vec<LedgerItem> {
        self.conn
            .lock()
            .execute(
                "INSERT INTO ledger(scope,text,status) VALUES(?,?,?) ON CONFLICT(scope,text) DO UPDATE SET status=excluded.status",
                params![scope, text, status_str(status)],
            )
            .unwrap();
        self.ledger(scope)
    }

    /// Short text injected into perception each step so plans survive the step budget.
    fn summary(&self, scope: &str) -> String {
        let l: Vec<LedgerItem> = self.ledger(scope).into_iter().filter(|i| i.status != LedgerStatus::Done).collect();
        let wp = self.list_waypoints(scope);
        let mut parts: Vec<String> = Vec::new();
        if !l.is_empty() {
            let items: Vec<String> = l.iter().map(|i| format!("[{}] {}", status_str(i.status), i.text)).collect();
            parts.push(format!("ledger: {}", items.join("; ")));
        }
        if !wp.is_empty() {
            let names: Vec<&str> = wp.iter().map(|(n, _)| n.as_str()).collect();
            parts.push(format!("waypoints: {}", names.join(", ")));
        }
        parts.join("\n")
    }
}
