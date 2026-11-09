//! The game's block and item registries and their tags, as data a host loads
//! from its resources directory. Mods ask the registry whether an id exists
//! and which entries a tag holds; the runtime answers from this snapshot.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Registries {
    /// Whether a host snapshot was read at all. Without one the runtime knows no
    /// tag's members, which is not the same as knowing a tag is empty: a tag
    /// lookup then answers "not known" rather than "missing".
    pub loaded: bool,
    pub blocks: BTreeSet<String>,
    pub items: BTreeSet<String>,
    /// Raw tag definitions by `namespace:path`: plain ids and `#tag` references.
    block_tags: HashMap<String, Vec<String>>,
    item_tags: HashMap<String, Vec<String>>,
}

fn tag_values(value: &serde_json::Value) -> Vec<String> {
    value
        .get("values")
        .and_then(|v| v.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|entry| match entry {
                    serde_json::Value::String(s) => Some(s.clone()),
                    other => other.get("id").and_then(|id| id.as_str()).map(String::from),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn read_tag_dir(dir: &Path, namespace: &str, into: &mut HashMap<String, Vec<String>>) {
    fn walk(root: &Path, dir: &Path, namespace: &str, into: &mut HashMap<String, Vec<String>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, namespace, into);
            } else if path.extension().is_some_and(|e| e == "json") {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
                    continue;
                };
                let Ok(rel) = path.strip_prefix(root) else {
                    continue;
                };
                let name = rel.with_extension("").to_string_lossy().replace('\\', "/");
                into.entry(format!("{namespace}:{name}"))
                    .or_default()
                    .extend(tag_values(&json));
            }
        }
    }
    walk(dir, dir, namespace, into);
}

impl Registries {
    /// Read `registries.json`, `block-tags.json` and `data/<ns>/tags/{block,item}`
    /// from a Horizon resources directory. Anything missing stays empty.
    pub fn load(dir: &Path) -> Self {
        let mut out = Self::default();
        if let Ok(text) = std::fs::read_to_string(dir.join("registries.json")) {
            out.loaded = true;
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                let keys = |name: &str| -> BTreeSet<String> {
                    json.get(name)
                        .and_then(|r| r.get("entries"))
                        .and_then(|e| e.as_object())
                        .map(|m| m.keys().cloned().collect())
                        .unwrap_or_default()
                };
                out.blocks = keys("minecraft:block");
                out.items = keys("minecraft:item");
            }
        }
        if let Ok(text) = std::fs::read_to_string(dir.join("block-tags.json")) {
            if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(&text) {
                for (name, body) in map {
                    // Each value is a tag file's JSON text.
                    if let Some(json) = body
                        .as_str()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    {
                        out.block_tags.insert(name, tag_values(&json));
                    }
                }
            }
        }
        if let Ok(namespaces) = std::fs::read_dir(dir.join("data")) {
            for ns in namespaces.flatten() {
                let name = ns.file_name().to_string_lossy().to_string();
                read_tag_dir(&ns.path().join("tags/block"), &name, &mut out.block_tags);
                read_tag_dir(&ns.path().join("tags/item"), &name, &mut out.item_tags);
            }
        }
        out
    }

    /// Record that the host snapshot is the authority even though it lists no
    /// blocks or items: the tag files it read are the whole truth, so a tag it
    /// does not have really is missing.
    pub fn mark_loaded(&mut self) {
        self.loaded = true;
    }

    /// Keep the tags another snapshot holds (those mod jars defined).
    pub fn absorb_tags(&mut self, other: Registries) {
        for (name, values) in other.block_tags {
            self.block_tags.entry(name).or_default().extend(values);
        }
        for (name, values) in other.item_tags {
            self.item_tags.entry(name).or_default().extend(values);
        }
    }

    /// Add the tags a mod jar's data pack defines (`data/<ns>/tags/{block,item}`).
    /// A tag several packs define holds the union of their entries.
    pub fn add_jar_tags(&mut self, jar: &Path, nested: Option<&str>) {
        let Ok(file) = std::fs::File::open(jar) else {
            return;
        };
        let Ok(mut archive) = zip::ZipArchive::new(file) else {
            return;
        };
        let mut inner;
        let mut visit = |archive_names: Vec<(String, String)>| {
            for (name, text) in archive_names {
                let Some(rest) = name.strip_prefix("data/") else {
                    continue;
                };
                let Some((ns, rest)) = rest.split_once('/') else {
                    continue;
                };
                let (target, path) = if let Some(p) = rest.strip_prefix("tags/block/") {
                    (&mut self.block_tags, p)
                } else if let Some(p) = rest.strip_prefix("tags/item/") {
                    (&mut self.item_tags, p)
                } else {
                    continue;
                };
                let Some(path) = path.strip_suffix(".json") else {
                    continue;
                };
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    target
                        .entry(format!("{ns}:{path}"))
                        .or_default()
                        .extend(tag_values(&json));
                }
            }
        };
        fn collect<R: std::io::Read + std::io::Seek>(
            archive: &mut zip::ZipArchive<R>,
        ) -> Vec<(String, String)> {
            let mut out = Vec::new();
            for i in 0..archive.len() {
                let Ok(mut entry) = archive.by_index(i) else {
                    continue;
                };
                let Ok(name) = entry.name().map(|n| n.to_string()) else {
                    continue;
                };
                if name.starts_with("data/") && name.ends_with(".json") && name.contains("/tags/") {
                    let mut text = String::new();
                    if std::io::Read::read_to_string(&mut entry, &mut text).is_ok() {
                        out.push((name, text));
                    }
                }
            }
            out
        }
        match nested {
            None => visit(collect(&mut archive)),
            Some(entry) => {
                let Ok(mut file) = archive.by_name(entry) else {
                    return;
                };
                let mut bytes = Vec::new();
                if std::io::Read::read_to_end(&mut file, &mut bytes).is_err() {
                    return;
                }
                drop(file);
                let Ok(opened) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
                    return;
                };
                inner = opened;
                visit(collect(&mut inner));
            }
        }
    }

    pub fn contains(&self, kind: &str, id: &str) -> bool {
        match kind {
            "BLOCK" => self.blocks.contains(id),
            "ITEM" => self.items.contains(id),
            _ => false,
        }
    }

    /// Every id the tag holds, following `#tag` references.
    pub fn tag_members(&self, kind: &str, tag: &str) -> Vec<String> {
        let tags = match kind {
            "BLOCK" => &self.block_tags,
            "ITEM" => &self.item_tags,
            _ => return Vec::new(),
        };
        let mut seen = BTreeSet::new();
        let mut out = BTreeMap::new();
        let mut stack = vec![tag.to_string()];
        while let Some(name) = stack.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            for value in tags.get(&name).into_iter().flatten() {
                match value.strip_prefix('#') {
                    Some(nested) => stack.push(nested.to_string()),
                    None => {
                        out.insert(value.clone(), ());
                    }
                }
            }
        }
        out.into_keys().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_follow_references() {
        let mut r = Registries::default();
        r.block_tags
            .insert("a:ores".into(), vec!["#a:coal".into(), "a:iron".into()]);
        r.block_tags
            .insert("a:coal".into(), vec!["a:coal_ore".into(), "#a:ores".into()]);
        assert_eq!(
            r.tag_members("BLOCK", "a:ores"),
            vec!["a:coal_ore", "a:iron"]
        );
        assert!(r.tag_members("ITEM", "a:ores").is_empty());
    }
}
