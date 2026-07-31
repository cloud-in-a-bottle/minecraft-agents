//! JSON-file-per-item library (port of filestore.ts + routinestore.ts + rulestore.ts).
use crate::skill::{Routine, Rule, RoutineStore, RuleStore};
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
    _marker: std::marker::PhantomData<T>,
}

impl<T: Serialize + DeserializeOwned + Named> JsonDirStore<T> {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        JsonDirStore { base_dir: base_dir.into(), _marker: std::marker::PhantomData }
    }

    fn dir(&self, scope: &str) -> PathBuf {
        self.base_dir.join(slug(scope))
    }

    pub fn save(&self, scope: &str, item: &T) {
        let dir = self.dir(scope);
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join(format!("{}.json", slug(item.name())));
        if let Ok(json) = serde_json::to_string_pretty(item) {
            let _ = std::fs::write(file, json);
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
