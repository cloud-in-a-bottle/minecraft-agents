//! JSON-file-per-item library (port of filestore.ts + routinestore.ts + rulestore.ts).
use crate::skill::{Routine, Rule, RoutineStore, RuleStore};
use parking_lot::Mutex;
use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};

/// Items stored by name; the file basename derives from it.
pub trait Named {
    fn name(&self) -> &str;
}
impl Named for Routine {
    fn name(&self) -> &str {
        &self.name
    }
}
impl Named for Rule {
    fn name(&self) -> &str {
        &self.name
    }
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

/// JSON-file-per-item store at <baseDir>/<scope>/<name>.json. Robust to missing dirs and junk files.
pub struct JsonDirStore<T> {
    base_dir: PathBuf,
    write_lock: Mutex<()>, // serializes writers; all agents share one store instance
    _marker: std::marker::PhantomData<T>,
}

impl<T: Serialize + DeserializeOwned + Named> JsonDirStore<T> {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        JsonDirStore {
            base_dir: base_dir.into(),
            write_lock: Mutex::new(()),
            _marker: std::marker::PhantomData,
        }
    }

    fn dir(&self, scope: &str) -> PathBuf {
        self.base_dir.join(slug(scope))
    }

    pub fn save(&self, scope: &str, item: &T) {
        let Ok(json) = serde_json::to_string_pretty(item) else { return };
        let dir = self.dir(scope);
        let name = slug(item.name());
        let file = dir.join(format!("{name}.json"));
        let tmp = dir.join(format!(".{name}.json.tmp"));
        // Serialize writers + write atomically (temp then rename): a concurrent reader never sees a
        // half-written file, and two agents saving at once can't clobber each other.
        let _guard = self.write_lock.lock();
        let _ = std::fs::create_dir_all(&dir);
        if std::fs::write(&tmp, json.as_bytes()).is_ok() {
            let _ = std::fs::rename(&tmp, &file);
        }
    }
    pub fn get(&self, scope: &str, name: &str) -> Option<T> {
        let file = self.dir(scope).join(format!("{}.json", slug(name)));
        let text = std::fs::read_to_string(file).ok()?;
        serde_json::from_str(&text).ok()
    }
    pub fn list(&self, scope: &str) -> Vec<T> {
        let dir = self.dir(scope);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        let mut out: Vec<T> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(item) = serde_json::from_str::<T>(&text) {
                    out.push(item);
                }
            }
        }
        out.sort_by(|a, b| a.name().cmp(b.name()));
        out
    }
    pub fn delete(&self, scope: &str, name: &str) -> bool {
        let file = self.dir(scope).join(format!("{}.json", slug(name)));
        let _guard = self.write_lock.lock(); // serialize with a concurrent save of the same file
        if !Path::new(&file).exists() {
            return false;
        }
        std::fs::remove_file(file).is_ok()
    }
}

/// File-backed routine library: one JSON file per routine under a shared directory.
pub struct FileRoutineStore {
    store: JsonDirStore<Routine>,
}
impl FileRoutineStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        FileRoutineStore { store: JsonDirStore::new(base_dir) }
    }
}
impl RoutineStore for FileRoutineStore {
    fn save_routine(&self, scope: &str, routine: Routine) {
        self.store.save(scope, &routine);
    }
    fn get_routine(&self, scope: &str, name: &str) -> Option<Routine> {
        self.store.get(scope, name)
    }
    fn list_routines(&self, scope: &str) -> Vec<(String, String)> {
        self.store.list(scope).into_iter().map(|r| (r.name, r.description)).collect()
    }
}

/// File-backed rule library: one JSON file per rule under a shared directory.
pub struct FileRuleStore {
    store: JsonDirStore<Rule>,
}
impl FileRuleStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        FileRuleStore { store: JsonDirStore::new(base_dir) }
    }
}
impl RuleStore for FileRuleStore {
    fn save_rule(&self, scope: &str, rule: Rule) {
        self.store.save(scope, &rule);
    }
    fn list_rules(&self, scope: &str) -> Vec<Rule> {
        self.store.list(scope)
    }
    fn delete_rule(&self, scope: &str, name: &str) -> bool {
        self.store.delete(scope, name)
    }
}
