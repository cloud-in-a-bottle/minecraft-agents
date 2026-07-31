import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Rule, RuleStore } from "./skillkit.js";

/** File-backed rule library: one JSON file per rule at <baseDir>/<scope>/<name>.json. */
export class FileRuleStore implements RuleStore {
  constructor(private readonly baseDir: string) {}

  private slug(s: string): string {
    return String(s).replace(/[^A-Za-z0-9_-]/g, "_");
  }

  private scopeDir(scope: string): string {
    return join(this.baseDir, this.slug(scope));
  }

  saveRule(scope: string, rule: Rule): void {
    const dir = this.scopeDir(scope);
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, `${this.slug(rule.name)}.json`), JSON.stringify(rule, null, 2));
  }

  listRules(scope: string): Rule[] {
    const dir = this.scopeDir(scope);
    if (!existsSync(dir)) return [];
    const rules: Rule[] = [];
    for (const f of readdirSync(dir)) {
      if (!f.endsWith(".json")) continue;
      try {
        rules.push(JSON.parse(readFileSync(join(dir, f), "utf8")) as Rule);
      } catch {
        // skip junk / partial files
      }
    }
    return rules.sort((a, b) => a.name.localeCompare(b.name));
  }

  deleteRule(scope: string, name: string): boolean {
    const file = join(this.scopeDir(scope), `${this.slug(name)}.json`);
    if (!existsSync(file)) return false;
    rmSync(file);
    return true;
  }
}
