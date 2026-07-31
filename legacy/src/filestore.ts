import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

/** JSON-file-per-item store at <baseDir>/<scope>/<name>.json. Robust to missing dirs and junk files. */
export class JsonDirStore<T extends { name: string }> {
  constructor(private readonly baseDir: string) {}

  private slug(s: string): string {
    return String(s).replace(/[^A-Za-z0-9_-]/g, "_");
  }
  private dir(scope: string): string {
    return join(this.baseDir, this.slug(scope));
  }

  save(scope: string, item: T): void {
    const dir = this.dir(scope);
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, `${this.slug(item.name)}.json`), JSON.stringify(item, null, 2));
  }
  get(scope: string, name: string): T | undefined {
    const file = join(this.dir(scope), `${this.slug(name)}.json`);
    if (!existsSync(file)) return undefined;
    try {
      return JSON.parse(readFileSync(file, "utf8")) as T;
    } catch {
      return undefined;
    }
  }
  list(scope: string): T[] {
    const dir = this.dir(scope);
    if (!existsSync(dir)) return [];
    const out: T[] = [];
    for (const f of readdirSync(dir)) {
      if (!f.endsWith(".json")) continue;
      try {
        out.push(JSON.parse(readFileSync(join(dir, f), "utf8")) as T);
      } catch {
        // skip junk / partial files
      }
    }
    return out.sort((a, b) => a.name.localeCompare(b.name));
  }
  delete(scope: string, name: string): boolean {
    const file = join(this.dir(scope), `${this.slug(name)}.json`);
    if (!existsSync(file)) return false;
    rmSync(file);
    return true;
  }
}
