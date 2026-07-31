import { JsonDirStore } from "./filestore.js";
import type { Routine, RoutineStore } from "./skillkit.js";

/** File-backed routine library: one JSON file per routine under a shared directory. */
export class FileRoutineStore implements RoutineStore {
  private readonly store: JsonDirStore<Routine>;
  constructor(baseDir: string) {
    this.store = new JsonDirStore<Routine>(baseDir);
  }
  saveRoutine(scope: string, routine: Routine): void {
    this.store.save(scope, routine);
  }
  getRoutine(scope: string, name: string): Routine | undefined {
    return this.store.get(scope, name);
  }
  listRoutines(scope: string): { name: string; description: string }[] {
    return this.store.list(scope).map((r) => ({ name: r.name, description: r.description }));
  }
}
