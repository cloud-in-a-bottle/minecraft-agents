//! Static, type-agnostic crafting recipe table (azalea 0.15.1 ships no client recipe DB).
//! Generated from minecraft-data pc/1.21.11 into `data/recipes.json`, name-keyed by result.
//!
//! Craft integration: `craft_item`/`craft_station` in skills/iron.rs call `recipes_all` +
//! family matching for craftability; ACTUAL craft execution vs azalea's container menu is a
//! separate integration TODO (azalea has no high-level `craft()`).
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Deserialize)]
pub struct ItemStack {
    pub name: String,
    pub count: u32,
}

/// One crafting recipe: shapeless `ingredients` or shaped `in_shape` (never both), plus the
/// derived 2x2-vs-table requirement. Names are bare (e.g. `oak_planks`).
#[derive(Deserialize)]
pub struct Recipe {
    pub result: ItemStack,
    pub ingredients: Option<Vec<String>>,
    pub in_shape: Option<Vec<Vec<Option<String>>>>,
    pub requires_table: bool,
}

impl Recipe {
    /// Consumed ingredients as ordered (name, count), summing duplicates (mirrors `recipeIngredients`).
    pub fn ingredient_counts(&self) -> Vec<(String, u32)> {
        let mut out: Vec<(String, u32)> = Vec::new();
        let mut add = |name: &str| match out.iter_mut().find(|(n, _)| n == name) {
            Some(e) => e.1 += 1,
            None => out.push((name.to_string(), 1)),
        };
        if let Some(ings) = &self.ingredients {
            for n in ings {
                add(n);
            }
        } else if let Some(shape) = &self.in_shape {
            for cell in shape.iter().flatten().flatten() {
                add(cell);
            }
        }
        out
    }
}

pub static RECIPES: LazyLock<HashMap<String, Vec<Recipe>>> =
    LazyLock::new(|| serde_json::from_str(include_str!("data/recipes.json")).expect("recipes.json"));

/// All crafting recipes producing `item_name`.
pub fn recipes_all(item_name: &str) -> &'static [Recipe] {
    RECIPES.get(item_name).map(Vec::as_slice).unwrap_or(&[])
}

/// Material family: the last underscore segment (acacia_planks -> planks, dark_oak_log -> log).
pub fn item_family(name: &str) -> &str {
    match name.rfind('_') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

pub struct ConsolidatedRecipe {
    pub out: u32,
    pub requires_table: bool,
    pub ingredients: Vec<String>,
}

/// Merge a target's recipes into one, collapsing an ingredient slot that varies only by material
/// family across recipes (e.g. any *_planks) into a `*_<family>` wildcard label.
pub fn consolidate(recipes: &[Recipe]) -> ConsolidatedRecipe {
    let signature = |r: &Recipe| -> String {
        let mut parts: Vec<String> = r
            .ingredient_counts()
            .into_iter()
            .map(|(name, c)| format!("{}:{}", item_family(&name), c))
            .collect();
        parts.sort();
        parts.join(",")
    };
    // Group by signature (insertion order); the largest group is the family-interchangeable form.
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (idx, r) in recipes.iter().enumerate() {
        let sig = signature(r);
        match groups.iter_mut().find(|(s, _)| *s == sig) {
            Some(g) => g.1.push(idx),
            None => groups.push((sig, vec![idx])),
        }
    }
    let best = &groups
        .iter()
        .reduce(|a, b| if b.1.len() > a.1.len() { b } else { a })
        .unwrap()
        .1;

    struct Slot {
        family: String,
        count: u32,
        names: Vec<String>,
    }
    let mut slots: Vec<(String, Slot)> = Vec::new();
    for &ri in best.iter() {
        for (name, c) in recipes[ri].ingredient_counts() {
            let family = item_family(&name).to_string();
            let key = format!("{family}:{c}");
            let idx = match slots.iter().position(|(k, _)| *k == key) {
                Some(p) => p,
                None => {
                    slots.push((key, Slot { family, count: c, names: Vec::new() }));
                    slots.len() - 1
                }
            };
            if !slots[idx].1.names.contains(&name) {
                slots[idx].1.names.push(name);
            }
        }
    }
    let rep = &recipes[best[0]];
    ConsolidatedRecipe {
        out: rep.result.count,
        requires_table: rep.requires_table,
        ingredients: slots
            .iter()
            .map(|(_, s)| {
                let label = if s.names.len() > 1 {
                    format!("*_{}", s.family)
                } else {
                    s.names[0].clone()
                };
                format!("{label} x{}", s.count)
            })
            .collect(),
    }
}

/// Family-pool an inventory (name->count) into family->total, for type-agnostic matching.
fn family_pool(inventory: &HashMap<String, u32>) -> HashMap<String, u32> {
    let mut pool: HashMap<String, u32> = HashMap::new();
    for (name, c) in inventory {
        *pool.entry(item_family(name).to_string()).or_default() += c;
    }
    pool
}

/// Recursively expand `item_name`'s recipe tree, crediting inventory by family at each level, and
/// list the base materials still missing. Type-agnostic: a needed `oak_planks` is covered by any
/// `*_planks` on hand. Recursion is bounded to depth 6.
pub fn missing_base_materials(
    item_name: &str,
    count: u32,
    inventory: &HashMap<String, u32>,
) -> Vec<(String, u32)> {
    let mut pool = family_pool(inventory);
    let mut need: Vec<(String, u32)> = Vec::new();
    expand(item_name, count, 6, &mut pool, &mut need);
    need
}

fn expand(name: &str, count: u32, depth: u32, pool: &mut HashMap<String, u32>, need: &mut Vec<(String, u32)>) {
    let family = item_family(name).to_string();
    let take = pool.get(&family).copied().unwrap_or(0).min(count);
    if take > 0 {
        *pool.get_mut(&family).unwrap() -= take;
    }
    let remaining = count - take;
    if remaining == 0 {
        return;
    }
    let recipe = if depth > 0 { recipes_all(name).first() } else { None };
    match recipe {
        Some(r) => {
            let out = r.result.count.max(1);
            let times = remaining.div_ceil(out);
            for (ing, ing_count) in r.ingredient_counts() {
                expand(&ing, ing_count * times, depth - 1, pool, need);
            }
        }
        None => match need.iter_mut().find(|(n, _)| *n == name) {
            Some(e) => e.1 += remaining,
            None => need.push((name.to_string(), remaining)),
        },
    }
}

/// True if some recipe for `item_name` has all direct ingredients covered by family-pooled
/// inventory, honoring its crafting-table requirement (mirrors mineflayer `recipesFor`).
pub fn can_craft(item_name: &str, inventory: &HashMap<String, u32>, has_table: bool) -> bool {
    let pool = family_pool(inventory);
    recipes_all(item_name).iter().any(|r| {
        if r.requires_table && !has_table {
            return false;
        }
        let mut p = pool.clone();
        r.ingredient_counts().into_iter().all(|(name, c)| {
            let family = item_family(&name);
            match p.get(family).copied().unwrap_or(0) {
                have if have >= c => {
                    *p.get_mut(family).unwrap() -= c;
                    true
                }
                _ => false,
            }
        })
    })
}
