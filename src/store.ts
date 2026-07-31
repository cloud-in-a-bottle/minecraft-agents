import { DatabaseSync } from "node:sqlite";
import { mkdirSync } from "node:fs";
import { dirname } from "node:path";
import type { LedgerItem, Memory, Pos, Routine, RoutineStore } from "./skillkit.js";

/** SQLite persistence: live settings, agent ownership, durable per-scope memory, and routines. */
export class Store implements Memory, RoutineStore {
  private readonly db: DatabaseSync;

  constructor(path: string) {
    mkdirSync(dirname(path), { recursive: true });
    this.db = new DatabaseSync(path);
    this.db.exec("PRAGMA journal_mode = WAL");
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS settings  (key TEXT PRIMARY KEY, value TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS agents    (username TEXT PRIMARY KEY, owner TEXT);
      CREATE TABLE IF NOT EXISTS waypoints (scope TEXT, name TEXT, x REAL, y REAL, z REAL, PRIMARY KEY (scope, name));
      CREATE TABLE IF NOT EXISTS notes     (scope TEXT, name TEXT, value TEXT, PRIMARY KEY (scope, name));
      CREATE TABLE IF NOT EXISTS ledger    (scope TEXT, text TEXT, status TEXT, PRIMARY KEY (scope, text));
      CREATE TABLE IF NOT EXISTS routines  (scope TEXT, name TEXT, description TEXT, steps TEXT, PRIMARY KEY (scope, name));
    `);
  }

  // --- live settings (server host/port, login, per-user cap) ---
  getSetting(key: string): string | undefined {
    return (this.db.prepare("SELECT value FROM settings WHERE key=?").get(key) as { value: string } | undefined)?.value;
  }
  setSetting(key: string, value: string): void {
    this.db.prepare("INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").run(key, value);
  }

  // --- agent ownership, written on any change ---
  setOwner(username: string, owner: string | null): void {
    this.db.prepare("INSERT INTO agents(username,owner) VALUES(?,?) ON CONFLICT(username) DO UPDATE SET owner=excluded.owner").run(username, owner);
  }
  allAgents(): { username: string; owner: string | null }[] {
    return this.db.prepare("SELECT username, owner FROM agents").all() as { username: string; owner: string | null }[];
  }

  // --- Memory: durable, scoped per owner ---
  setWaypoint(scope: string, name: string, pos: Pos): void {
    this.db
      .prepare("INSERT INTO waypoints(scope,name,x,y,z) VALUES(?,?,?,?,?) ON CONFLICT(scope,name) DO UPDATE SET x=excluded.x,y=excluded.y,z=excluded.z")
      .run(scope, name, pos.x, pos.y, pos.z);
  }
  getWaypoint(scope: string, name: string): Pos | undefined {
    const r = this.db.prepare("SELECT x,y,z FROM waypoints WHERE scope=? AND name=?").get(scope, name) as Pos | undefined;
    return r ? { x: r.x, y: r.y, z: r.z } : undefined;
  }
  listWaypoints(scope: string): [string, Pos][] {
    const rows = this.db.prepare("SELECT name,x,y,z FROM waypoints WHERE scope=? ORDER BY name").all(scope) as unknown as (Pos & { name: string })[];
    return rows.map((r) => [r.name, { x: r.x, y: r.y, z: r.z }]);
  }

  setNote(scope: string, key: string, text: string): void {
    this.db.prepare("INSERT INTO notes(scope,name,value) VALUES(?,?,?) ON CONFLICT(scope,name) DO UPDATE SET value=excluded.value").run(scope, key, text);
  }
  listNotes(scope: string, query?: string): [string, string][] {
    const rows = this.db.prepare("SELECT name,value FROM notes WHERE scope=? ORDER BY name").all(scope) as { name: string; value: string }[];
    const all = rows.map((r) => [r.name, r.value] as [string, string]);
    return query ? all.filter(([k, v]) => k.includes(query) || v.includes(query)) : all;
  }

  ledger(scope: string): LedgerItem[] {
    return this.db.prepare("SELECT text,status FROM ledger WHERE scope=? ORDER BY rowid").all(scope) as unknown as LedgerItem[];
  }
  setLedgerItem(scope: string, text: string, status: LedgerItem["status"]): LedgerItem[] {
    this.db.prepare("INSERT INTO ledger(scope,text,status) VALUES(?,?,?) ON CONFLICT(scope,text) DO UPDATE SET status=excluded.status").run(scope, text, status);
    return this.ledger(scope);
  }

  // --- RoutineStore: saved, replayable procedures ---
  saveRoutine(scope: string, routine: Routine): void {
    this.db
      .prepare("INSERT INTO routines(scope,name,description,steps) VALUES(?,?,?,?) ON CONFLICT(scope,name) DO UPDATE SET description=excluded.description, steps=excluded.steps")
      .run(scope, routine.name, routine.description, JSON.stringify(routine.steps));
  }
  getRoutine(scope: string, name: string): Routine | undefined {
    const r = this.db.prepare("SELECT name,description,steps FROM routines WHERE scope=? AND name=?").get(scope, name) as
      | { name: string; description: string; steps: string }
      | undefined;
    return r ? { name: r.name, description: r.description, steps: JSON.parse(r.steps) } : undefined;
  }
  listRoutines(scope: string): { name: string; description: string }[] {
    return this.db.prepare("SELECT name,description FROM routines WHERE scope=? ORDER BY name").all(scope) as { name: string; description: string }[];
  }

  /** Short text injected into perception each step so plans survive the step budget. */
  summary(scope: string): string {
    const l = this.ledger(scope).filter((i) => i.status !== "done");
    const wp = this.listWaypoints(scope);
    const parts: string[] = [];
    if (l.length) parts.push(`ledger: ${l.map((i) => `[${i.status}] ${i.text}`).join("; ")}`);
    if (wp.length) parts.push(`waypoints: ${wp.map(([n]) => n).join(", ")}`);
    return parts.join("\n");
  }
}
