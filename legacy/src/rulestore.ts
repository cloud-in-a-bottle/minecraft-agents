import { JsonDirStore } from "./filestore.js";
import type { Rule, RuleStore } from "./skillkit.js";

/** File-backed rule library: one JSON file per rule under a shared directory. */
export class FileRuleStore implements RuleStore {
  private readonly store: JsonDirStore<Rule>;
  constructor(baseDir: string) {
    this.store = new JsonDirStore<Rule>(baseDir);
  }
  saveRule(scope: string, rule: Rule): void {
    this.store.save(scope, rule);
  }
  listRules(scope: string): Rule[] {
    return this.store.list(scope);
  }
  deleteRule(scope: string, name: string): boolean {
    return this.store.delete(scope, name);
  }
}
