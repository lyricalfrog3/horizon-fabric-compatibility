//! A compatibility census over a folder of Fabric mods.
//!
//! The census runs real mods through the interpreter and ranks what is missing
//! by how many mods it affects, so a fix can be chosen for breadth instead of
//! by the mod in front of us. **It measures, it does not claim:** a mod with
//! zero missing symbols is not "working"; its init simply hit nothing the
//! runtime lacks.
//!
//! # `census.json` (version 1)
//!
//! ```json
//! {
//!   "version": 1,
//!   "hfc_commit": "33d06b5",
//!   "mode": "alone" | "together",
//!   "resources": true,
//!   "corpus": [{"file":"a.jar","id":"a","version":"1","sha256":"..."}],
//!   "mods": [{
//!     "id":"a","file":"a.jar","version":"1",
//!     "entrypoints":[{"class":"com/a/Mod","method":"onInitialize","ok":true,
//!                     "error":null,"instructions":1234,"elapsed_ms":3,
//!                     "budget_exhausted":false}],
//!     "missing":[{"symbol":"net/minecraft/...","hits":2,"caller":"com/a/Mod.init"}],
//!     "events":[{"event":"net/.../Event.FIELD","listeners":1,"dispatched":false}],
//!     "mixins":[{"target":"net.minecraft.Foo","outcome":"applied"}],
//!     "standins":[{"owner":"net/minecraft/world/item/ItemStack","name":"shrink","descriptor":"(I)V"}],
//!     "counts":{"blocks":0,"items":3,"creative_tabs":1}
//!   }],
//!   "shared": { "missing": [...], "events": [...], "mixins": [...], "counts": {...} },
//!   "skipped": [{"file":"b.jar","reason":"missing dependency x"}]
//! }
//! ```
//!
//! In `--alone` mode everything is in the per-mod records. In `--together`
//! mode the events, mixins and counts are shared by the whole folder and live
//! in `shared`; entrypoints and their missing symbols are still attributed to
//! the mod whose jar holds the entrypoint class.

use crate::bridge::{CensusLog, DISPATCHED_EVENTS, FabricBridge};
use hfc_fabric::Metadata;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The schema version of `census.json`.
pub const SCHEMA_VERSION: u32 = 1;

/// Classes a mod uses but the runtime replaces with a native object.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StandinRef {
    pub owner: String,
    pub name: String,
    pub descriptor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusEntry {
    pub file: String,
    pub id: String,
    pub version: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntrypointRecord {
    pub class: String,
    pub method: String,
    pub ok: bool,
    pub error: Option<String>,
    pub instructions: u64,
    pub elapsed_ms: u64,
    pub budget_exhausted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingSymbol {
    /// `class <name>` or `<owner>.<name><descriptor>`, caller stripped.
    pub symbol: String,
    pub hits: u64,
    /// One caller frame, when the interpreter ran with call tracing.
    pub caller: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    /// `owner class.FIELD`, binary names.
    pub event: String,
    pub listeners: usize,
    /// Whether the host dispatches this event (`DISPATCHED_EVENTS`).
    pub dispatched: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MixinRecord {
    /// The target class, dotted.
    pub target: String,
    /// `applied`, `partial`, `ignored` or `native` (see the docs).
    pub outcome: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub blocks: usize,
    pub items: usize,
    pub creative_tabs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModRecord {
    pub id: String,
    pub file: String,
    pub version: String,
    pub entrypoints: Vec<EntrypointRecord>,
    pub missing: Vec<MissingSymbol>,
    pub events: Vec<EventRecord>,
    pub mixins: Vec<MixinRecord>,
    pub standins: Vec<StandinRef>,
    pub counts: Counts,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedRecord {
    pub missing: Vec<MissingSymbol>,
    pub events: Vec<EventRecord>,
    pub mixins: Vec<MixinRecord>,
    pub counts: Counts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    pub file: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub version: u32,
    pub hfc_commit: String,
    pub mode: String,
    pub resources: bool,
    pub corpus: Vec<CorpusEntry>,
    pub mods: Vec<ModRecord>,
    #[serde(default)]
    pub shared: SharedRecord,
    pub skipped: Vec<Skipped>,
}

/// What to census and where to write it.
#[derive(Debug, Clone)]
pub struct Options {
    /// The folder of `.jar` files. Default `test-mods/corpus`.
    pub mods: PathBuf,
    /// The output folder; a new temp folder when unset.
    pub out: Option<PathBuf>,
    /// The client's prepared resources for registry ids and tags.
    pub resources: Option<PathBuf>,
    /// An older `census.json` to compare with.
    pub compare: Option<PathBuf>,
    /// One bridge per mod (with its dependencies) instead of one for all.
    pub alone: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mods: PathBuf::from("test-mods/corpus"),
            out: None,
            resources: None,
            compare: None,
            alone: false,
        }
    }
}

/// What `run` wrote.
#[derive(Debug)]
pub struct Written {
    pub report: Report,
    pub dir: PathBuf,
    pub json: PathBuf,
    pub markdown: PathBuf,
}

/// A syntax error in `--compare`'s file.
fn read_report(path: &Path) -> Result<Report, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn git_commit() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|commit| !commit.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// One jar of the corpus: its scan, its Fabric metadata and the corpus entry.
struct Candidate {
    path: PathBuf,
    scan: hfc_fabric::JarScan,
    metadata: Metadata,
}

impl Candidate {
    fn entry(&self) -> CorpusEntry {
        CorpusEntry {
            file: self.scan.file_name.clone(),
            id: self.metadata.id.clone(),
            version: self.metadata.version.clone(),
            sha256: self.scan.sha256.clone(),
        }
    }
}

/// Read the folder: every Fabric jar with metadata, and what had to be skipped.
fn corpus(dir: &Path) -> Result<(Vec<Candidate>, Vec<Skipped>), String> {
    let discovered = hfc_fabric::discover(dir).map_err(|e| e.to_string())?;
    let limits = hfc_core::Limits::default();
    let mut candidates = Vec::new();
    let mut skipped = Vec::new();
    for jar in discovered {
        let scan = match hfc_fabric::scan_path(&jar.path, &limits) {
            Ok(scan) => scan,
            Err(error) => {
                skipped.push(Skipped {
                    file: jar.file_name,
                    reason: error.to_string(),
                });
                continue;
            }
        };
        let Some(metadata) = scan.metadata.clone() else {
            skipped.push(Skipped {
                file: scan.file_name.clone(),
                reason: scan
                    .error
                    .clone()
                    .unwrap_or_else(|| "no fabric.mod.json".into()),
            });
            continue;
        };
        candidates.push(Candidate {
            path: jar.path,
            scan,
            metadata,
        });
    }
    candidates.sort_by(|a, b| a.metadata.id.cmp(&b.metadata.id));
    Ok((candidates, skipped))
}

/// Class names each jar carries, so an entrypoint can be attributed to a mod.
fn class_owners(candidates: &[Candidate]) -> BTreeMap<String, String> {
    let mut owners = BTreeMap::new();
    for candidate in candidates {
        let Ok(file) = std::fs::File::open(&candidate.path) else {
            continue;
        };
        let Ok(mut zip) = zip::ZipArchive::new(file) else {
            continue;
        };
        for index in 0..zip.len() {
            let Ok(entry) = zip.by_index(index) else {
                continue;
            };
            let Ok(name) = entry.name() else {
                continue;
            };
            if let Some(name) = name.as_ref().strip_suffix(".class") {
                owners
                    .entry(name.to_string())
                    .or_insert_with(|| candidate.metadata.id.clone());
            }
        }
    }
    owners
}

/// The classes a mod's jar names in method references to native stand-ins.
fn standin_refs(path: &Path) -> Vec<StandinRef> {
    let natives: BTreeSet<&str> = crate::minecraft::native_classes().iter().copied().collect();
    let mut out = BTreeSet::new();
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return Vec::new();
    };
    for index in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(index) else {
            continue;
        };
        let is_class = entry
            .name()
            .map(|name| name.ends_with(".class"))
            .unwrap_or(false);
        if !is_class {
            continue;
        }
        let mut bytes = Vec::new();
        if std::io::Read::read_to_end(&mut entry, &mut bytes).is_err() {
            continue;
        }
        for (owner, name, descriptor) in crate::class::scan_method_refs(&bytes) {
            if natives.contains(owner.as_str()) {
                out.insert(StandinRef {
                    owner,
                    name,
                    descriptor,
                });
            }
        }
    }
    out.into_iter().collect()
}

/// Missing entries as `note_missing` wrote them, grouped by symbol.
pub fn group_missing(lines: &[String]) -> Vec<MissingSymbol> {
    let mut by_symbol: BTreeMap<String, (u64, Option<String>)> = BTreeMap::new();
    for line in lines {
        let (symbol, caller) = match line.split_once(" <- ") {
            Some((symbol, caller)) => (
                symbol.trim().to_string(),
                Some(
                    caller
                        .split(" <- ")
                        .next()
                        .unwrap_or(caller)
                        .trim()
                        .to_string(),
                ),
            ),
            None => (line.trim().to_string(), None),
        };
        if symbol.is_empty() {
            continue;
        }
        let entry = by_symbol.entry(symbol).or_insert((0, caller));
        entry.0 += 1;
    }
    let mut out: Vec<MissingSymbol> = by_symbol
        .into_iter()
        .map(|(symbol, (hits, caller))| MissingSymbol {
            symbol,
            hits,
            caller,
        })
        .collect();
    out.sort_by(|a, b| b.hits.cmp(&a.hits).then(a.symbol.cmp(&b.symbol)));
    out
}

/// The `owner` part of a missing symbol, for the grouped ranking.
pub fn symbol_owner(symbol: &str) -> &str {
    if let Some(class) = symbol.strip_prefix("class ") {
        return class;
    }
    symbol.split(['.', '(']).next().unwrap_or(symbol)
}

/// One symbol ranked across mods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedSymbol {
    pub symbol: String,
    /// How many distinct mods hit it.
    pub mods: usize,
    pub hits: u64,
    pub caller: Option<String>,
    pub mod_ids: Vec<String>,
}

/// Missing symbols ranked by the number of mods that hit them.
pub fn rank_symbols(mods: &[ModRecord]) -> Vec<RankedSymbol> {
    let mut by_symbol: BTreeMap<String, (BTreeSet<String>, u64, Option<String>)> = BTreeMap::new();
    for m in mods {
        for missing in &m.missing {
            let entry = by_symbol
                .entry(missing.symbol.clone())
                .or_insert_with(|| (BTreeSet::new(), 0, None));
            entry.0.insert(m.id.clone());
            entry.1 += missing.hits;
            if entry.2.is_none() {
                entry.2 = missing.caller.clone();
            }
        }
    }
    let mut out: Vec<RankedSymbol> = by_symbol
        .into_iter()
        .map(|(symbol, (mods, hits, caller))| RankedSymbol {
            symbol,
            mods: mods.len(),
            hits,
            caller,
            mod_ids: mods.into_iter().collect(),
        })
        .collect();
    out.sort_by(|a, b| {
        b.mods
            .cmp(&a.mods)
            .then(b.hits.cmp(&a.hits))
            .then(a.symbol.cmp(&b.symbol))
    });
    out
}

/// Owner classes ranked by the number of mods that hit a missing member.
pub fn rank_owners(mods: &[ModRecord]) -> Vec<(String, usize)> {
    let mut by_owner: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for m in mods {
        for missing in &m.missing {
            by_owner
                .entry(symbol_owner(&missing.symbol).to_string())
                .or_default()
                .insert(m.id.clone());
        }
    }
    let mut out: Vec<(String, usize)> = by_owner
        .into_iter()
        .map(|(owner, mods)| (owner, mods.len()))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

fn missing_of(entrypoints: &[EntrypointRecord], log: &CensusLog) -> Vec<MissingSymbol> {
    let mut lines = Vec::new();
    for entry in &log.entrypoints {
        if entrypoints
            .iter()
            .any(|record| record.class == entry.class_name && record.method == entry.method_name)
        {
            lines.extend(entry.missing.iter().cloned());
        }
    }
    group_missing(&lines)
}

fn entrypoint_records(log: &CensusLog) -> Vec<EntrypointRecord> {
    log.entrypoints
        .iter()
        .map(|entry| EntrypointRecord {
            class: entry.class_name.clone(),
            method: entry.method_name.clone(),
            ok: entry.ok,
            error: entry.error.clone(),
            instructions: entry.instructions,
            elapsed_ms: entry.elapsed_ms,
            budget_exhausted: entry.budget_exhausted,
        })
        .collect()
}

fn event_record(event: String, listeners: usize) -> EventRecord {
    EventRecord {
        dispatched: DISPATCHED_EVENTS.contains(&event.as_str()),
        event,
        listeners,
    }
}

fn event_records(bridge: &FabricBridge) -> Vec<EventRecord> {
    bridge
        .census_events()
        .into_iter()
        .map(|(event, listeners)| event_record(event, listeners))
        .collect()
}

/// The events the entrypoints in `entrypoints` registered listeners on.
fn events_of(entrypoints: &[EntrypointRecord], log: &CensusLog) -> Vec<EventRecord> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for entry in &log.entrypoints {
        if entrypoints
            .iter()
            .any(|record| record.class == entry.class_name && record.method == entry.method_name)
        {
            for (event, listeners) in &entry.events {
                *counts.entry(event.clone()).or_default() += *listeners;
            }
        }
    }
    counts
        .into_iter()
        .map(|(event, listeners)| event_record(event, listeners))
        .collect()
}

/// The mixin classes a jar's declared mixin configs list.
fn mixin_classes(path: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(file) = std::fs::File::open(path) else {
        return out;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return out;
    };
    let Ok(mut metadata) = zip.by_name("fabric.mod.json") else {
        return out;
    };
    let mut json = String::new();
    if std::io::Read::read_to_string(&mut metadata, &mut json).is_err() {
        return out;
    }
    drop(metadata);
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) else {
        return out;
    };
    let Some(mixins) = value.get("mixins").and_then(|mixins| mixins.as_array()) else {
        return out;
    };
    for mixin in mixins {
        let config = match mixin {
            serde_json::Value::String(config) => config.clone(),
            serde_json::Value::Object(object) => match object.get("config") {
                Some(serde_json::Value::String(config)) => config.clone(),
                _ => continue,
            },
            _ => continue,
        };
        let Ok(mut entry) = zip.by_name(&config) else {
            continue;
        };
        let mut text = String::new();
        if std::io::Read::read_to_string(&mut entry, &mut text).is_err() {
            continue;
        }
        drop(entry);
        let Ok(config) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(package) = config.get("package").and_then(|p| p.as_str()) else {
            continue;
        };
        let package = package.replace('.', "/");
        for list in ["mixins", "client", "server"] {
            let Some(classes) = config.get(list).and_then(|c| c.as_array()) else {
                continue;
            };
            for class in classes.iter().filter_map(|class| class.as_str()) {
                out.insert(format!("{package}/{}", class.replace('.', "/")));
            }
        }
    }
    out
}

fn mixin_records(classes: &BTreeSet<String>, bridge: &mut FabricBridge) -> Vec<MixinRecord> {
    let manifest = bridge.census_mixins().clone();
    let normalized: BTreeSet<String> = classes
        .iter()
        .map(|class| class.replace('/', "."))
        .collect();
    let mut targets: BTreeSet<String> = BTreeSet::new();
    for injector in &manifest.injectors {
        if normalized.contains(&injector.mixin_class.replace('/', ".")) {
            targets.insert(injector.target_class.replace('/', "."));
        }
    }
    for accessor in &manifest.accessors {
        if normalized.contains(&accessor.interface.replace('/', ".")) {
            targets.insert(accessor.target_class.replace('/', "."));
        }
    }
    for method in &manifest.interface_methods {
        if normalized.contains(&method.mixin_class.replace('/', ".")) {
            targets.insert(method.target_class.replace('/', "."));
        }
    }
    let natives: BTreeSet<String> = crate::minecraft::native_classes()
        .iter()
        .map(|name| name.replace('/', "."))
        .collect();
    targets
        .into_iter()
        .map(|target| {
            let dotted = target.replace('/', ".");
            let outcome = if natives.contains(&dotted) || !bridge.census_interprets(&dotted) {
                "native"
            } else {
                let injectors: Vec<_> = manifest
                    .injectors
                    .iter()
                    .filter(|injector| injector.target_class.replace('/', ".") == dotted)
                    .collect();
                // "applied" means the injector was checked against the
                // classpath and the interpreter dispatches it; a parsed
                // annotation that was never wired is "partial" at best, however
                // supported its `@At` looks. `supported()` is a parse check, not
                // a wiring check, and treating it as one claimed compatibility
                // the runtime did not have.
                let wired: Vec<_> = manifest
                    .wired
                    .iter()
                    .filter(|injector| injector.declaring_class.replace('/', ".") == dotted)
                    .collect();
                let accessor = manifest
                    .accessors
                    .iter()
                    .any(|accessor| accessor.target_class.replace('/', ".") == dotted)
                    || manifest
                        .interface_methods
                        .iter()
                        .any(|method| method.target_class.replace('/', ".") == dotted);
                if !wired.is_empty() {
                    // Every injector targeting this class runs => applied;
                    // some were recorded unsupported => partial. A target
                    // whose injectors do not run still counts an accessor
                    // mixin, but only as partial: the injectors are missing.
                    if wired.len() == injectors.len() {
                        "applied"
                    } else {
                        "partial"
                    }
                } else if accessor {
                    // Only `@Accessor`/`@Invoker` or interface methods, which
                    // are a separate mechanism; the class is still modified.
                    "applied"
                } else {
                    "ignored"
                }
            };
            MixinRecord {
                target: dotted,
                outcome: outcome.to_string(),
            }
        })
        .collect()
}

/// Run the census. Writes `census.json` and `census.md` under `options.out`
/// (or a fresh temp folder) and returns the report.
pub fn run(options: &Options) -> Result<Written, String> {
    let (candidates, mut skipped) = corpus(&options.mods)?;
    if candidates.is_empty() {
        return Err(format!(
            "{} has no Fabric jars with fabric.mod.json (see test-mods/README.md)",
            options.mods.display()
        ));
    }
    let owners = class_owners(&candidates);
    let mut report = Report {
        version: SCHEMA_VERSION,
        hfc_commit: git_commit(),
        mode: if options.alone { "alone" } else { "together" }.into(),
        resources: options.resources.is_some(),
        corpus: candidates.iter().map(Candidate::entry).collect(),
        mods: Vec::new(),
        shared: SharedRecord::default(),
        skipped: Vec::new(),
    };

    let bridge_for = |paths: &[PathBuf]| -> Result<FabricBridge, String> {
        let Some(mut bridge) = crate::discovery::bridge_with_vanilla() else {
            return Err(
                "no Minecraft installation (HORIZON_MINECRAFT_DIR or ~/.local/share/horizon-launcher/minecraft); the census needs its real classes"
                    .into(),
            );
        };
        if let Some(resources) = &options.resources {
            bridge.load_registries(resources);
        }
        let game = std::env::temp_dir().join(format!(
            "hfc-census-{}-{}",
            std::process::id(),
            paths
                .first()
                .and_then(|path| path.file_stem())
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "all".into())
        ));
        let _ = std::fs::remove_dir_all(&game);
        std::fs::create_dir_all(&game).map_err(|e| format!("{}: {e}", game.display()))?;
        bridge.set_game_dir(Some(game.clone()));
        for path in paths {
            if let Err(error) = bridge.load_mod_jar(path) {
                log::warn!("census: {}: {error}", path.display());
            }
        }
        bridge.enable_census();
        if let Err(error) = bridge.init() {
            let _ = std::fs::remove_dir_all(&game);
            return Err(format!("init failed: {error}"));
        }
        Ok(bridge)
    };

    if options.alone {
        // A dependency may be a nested jar of a corpus jar (Fabric API's
        // submodules are): the parent jar provides it.
        let mut providers: BTreeMap<&str, &Path> = BTreeMap::new();
        for candidate in &candidates {
            providers
                .entry(candidate.metadata.id.as_str())
                .or_insert(candidate.path.as_path());
            for nested in &candidate.scan.nested {
                if let Some(metadata) = &nested.metadata {
                    providers
                        .entry(metadata.id.as_str())
                        .or_insert(candidate.path.as_path());
                }
            }
        }
        let builtins = ["fabric", "fabricloader", "minecraft", "java"];
        for candidate in &candidates {
            let mut paths = vec![candidate.path.clone()];
            let mut missing_dependency = None;
            for dependency in &candidate.metadata.depends {
                if builtins.contains(&dependency.id.as_str()) {
                    continue;
                }
                match providers.get(dependency.id.as_str()) {
                    Some(provider) => paths.push(provider.to_path_buf()),
                    None => {
                        missing_dependency = Some(dependency.id.clone());
                        break;
                    }
                }
            }
            if let Some(dependency) = missing_dependency {
                skipped.push(Skipped {
                    file: candidate.scan.file_name.clone(),
                    reason: format!("missing dependency {dependency}"),
                });
                continue;
            }
            paths.sort();
            paths.dedup();
            let mut bridge = bridge_for(&paths)?;
            let log = bridge.census().cloned().unwrap_or_default();
            let entrypoints: Vec<EntrypointRecord> = entrypoint_records(&log)
                .into_iter()
                .filter(|entry| {
                    owners
                        .get(&entry.class)
                        .is_some_and(|id| *id == candidate.metadata.id)
                })
                .collect();
            let record = ModRecord {
                id: candidate.metadata.id.clone(),
                file: candidate.scan.file_name.clone(),
                version: candidate.metadata.version.clone(),
                missing: missing_of(&entrypoints, &log),
                events: events_of(&entrypoints, &log),
                mixins: mixin_records(&mixin_classes(&candidate.path), &mut bridge),
                standins: standin_refs(&candidate.path),
                counts: {
                    let (blocks, items, creative_tabs) = bridge.census_counts();
                    Counts {
                        blocks,
                        items,
                        creative_tabs,
                    }
                },
                entrypoints,
            };
            report.mods.push(record);
        }
    } else {
        let paths: Vec<PathBuf> = candidates
            .iter()
            .map(|candidate| candidate.path.clone())
            .collect();
        let mut bridge = bridge_for(&paths)?;
        let log = bridge.census().cloned().unwrap_or_default();
        let (blocks, items, creative_tabs) = bridge.census_counts();
        report.shared = SharedRecord {
            events: event_records(&bridge),
            mixins: candidates
                .iter()
                .flat_map(|candidate| mixin_records(&mixin_classes(&candidate.path), &mut bridge))
                .collect(),
            counts: Counts {
                blocks,
                items,
                creative_tabs,
            },
            missing: group_missing(bridge.census_missing()),
        };
        for candidate in &candidates {
            let entrypoints: Vec<EntrypointRecord> = log
                .entrypoints
                .iter()
                .filter(|entry| {
                    owners
                        .get(&entry.class_name)
                        .is_some_and(|id| *id == candidate.metadata.id)
                })
                .map(|entry| EntrypointRecord {
                    class: entry.class_name.clone(),
                    method: entry.method_name.clone(),
                    ok: entry.ok,
                    error: entry.error.clone(),
                    instructions: entry.instructions,
                    elapsed_ms: entry.elapsed_ms,
                    budget_exhausted: entry.budget_exhausted,
                })
                .collect();
            report.mods.push(ModRecord {
                id: candidate.metadata.id.clone(),
                file: candidate.scan.file_name.clone(),
                version: candidate.metadata.version.clone(),
                missing: missing_of(&entrypoints, &log),
                events: events_of(&entrypoints, &log),
                entrypoints,
                mixins: Vec::new(),
                standins: standin_refs(&candidate.path),
                counts: Counts::default(),
            });
        }
        report
            .shared
            .mixins
            .sort_by(|a, b| a.target.cmp(&b.target).then(a.outcome.cmp(&b.outcome)));
        report.shared.mixins.dedup();
    }
    report.skipped = skipped;

    let dir = options.out.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("hfc-census-out-{}", std::process::id()))
    });
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let json = dir.join("census.json");
    let markdown = dir.join("census.md");
    let text = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&json, &text).map_err(|e| format!("{}: {e}", json.display()))?;
    let old = match &options.compare {
        Some(path) => Some(read_report(path)?),
        None => None,
    };
    std::fs::write(&markdown, markdown_of(&report, old.as_ref()))
        .map_err(|e| format!("{}: {e}", markdown.display()))?;
    Ok(Written {
        report,
        dir,
        json,
        markdown,
    })
}

/// The human report.
pub fn markdown_of(report: &Report, old: Option<&Report>) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut out = String::new();
    out.push_str("# Fabric runtime census\n\n");
    out.push_str(&format!(
        "{} jars, mode `{}`, HFC commit `{}`, run at {} (unix seconds).\n\n",
        report.corpus.len(),
        report.mode,
        report.hfc_commit,
        now
    ));
    out.push_str(
        "Zero missing symbols means init hit nothing the runtime lacks; it does not mean the mod works.\n\n",
    );
    out.push_str(
        "Entrypoints, mixins and stand-in calls are attributed by the classes in each jar's root;          classes inside nested jars are not attributed.\n\n",
    );
    if let Some(old) = old {
        out.push_str(&format!(
            "## Changes since `{}`\n\n{}\n\n",
            old.hfc_commit,
            compare_markdown(old, report)
        ));
    }
    let ranked = rank_symbols(&report.mods);
    out.push_str("## Top missing symbols by mods affected\n\n");
    out.push_str("| Symbol | Mods | Hits | Example caller | Mods |\n|---|---:|---:|---|---|\n");
    for symbol in ranked.iter().take(50) {
        out.push_str(&format!(
            "| `{}` | {} | {} | `{}` | {} |\n",
            symbol.symbol,
            symbol.mods,
            symbol.hits,
            symbol.caller.as_deref().unwrap_or("-"),
            symbol.mod_ids.join(", ")
        ));
    }
    out.push_str("\n## Top missing owners by mods affected\n\n| Owner | Mods |\n|---|---:|\n");
    for (owner, mods) in rank_owners(&report.mods).into_iter().take(30) {
        out.push_str(&format!("| `{owner}` | {mods} |\n"));
    }
    for (title, events) in [
        ("Events mods listen to that the host never dispatches", true),
        ("Events dispatched", false),
    ] {
        let mut rows: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for m in &report.mods {
            for event in &m.events {
                if event.dispatched != events {
                    let entry = rows.entry(event.event.as_str()).or_default();
                    entry.0 += 1;
                    entry.1 += event.listeners;
                }
            }
        }
        for event in &report.shared.events {
            if event.dispatched != events {
                let entry = rows.entry(event.event.as_str()).or_default();
                entry.0 += 1;
                entry.1 += event.listeners;
            }
        }
        out.push_str(&format!(
            "\n## {title}\n\n| Event | Mods | Listeners |\n|---|---:|---:|\n"
        ));
        let mut rows: Vec<_> = rows.into_iter().collect();
        rows.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
        for (event, (mods, listeners)) in rows {
            out.push_str(&format!("| `{event}` | {mods} | {listeners} |\n"));
        }
    }
    let mut mixins: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for m in &report.mods {
        for mixin in &m.mixins {
            mixins
                .entry((mixin.target.clone(), mixin.outcome.clone()))
                .or_default()
                .insert(m.id.clone());
        }
    }
    for mixin in &report.shared.mixins {
        mixins
            .entry((mixin.target.clone(), mixin.outcome.clone()))
            .or_default();
    }
    out.push_str("\n## Mixin targets\n\n| Target | Outcome | Mods |\n|---|---|---:|\n");
    let mut mixins: Vec<_> = mixins.into_iter().collect();
    mixins.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    for ((target, outcome), mods) in mixins {
        out.push_str(&format!("| `{target}` | {outcome} | {} |\n", mods.len()));
    }
    let mut standins: BTreeMap<(String, String, String), BTreeSet<String>> = BTreeMap::new();
    for m in &report.mods {
        for reference in &m.standins {
            standins
                .entry((
                    reference.owner.clone(),
                    reference.name.clone(),
                    reference.descriptor.clone(),
                ))
                .or_default()
                .insert(m.id.clone());
        }
    }
    out.push_str(
        "\n## Calls on native stand-ins (a stand-in may or may not implement these)\n\n| Owner.method | Descriptor | Mods |\n|---|---|---:|\n",
    );
    let mut standins: Vec<_> = standins.into_iter().collect();
    standins.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    for ((owner, name, descriptor), mods) in standins.into_iter().take(50) {
        out.push_str(&format!(
            "| `{owner}.{name}` | `{descriptor}` | {} |\n",
            mods.len()
        ));
    }
    let mut failures: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for m in &report.mods {
        for entry in &m.entrypoints {
            let reason = if entry.budget_exhausted {
                "budget exhausted".to_string()
            } else if let Some(error) = &entry.error {
                error.lines().next().unwrap_or(error).to_string()
            } else {
                continue;
            };
            failures.entry(reason).or_default().insert(m.id.clone());
        }
    }
    out.push_str("\n## Entrypoint failures and budget overruns\n\n| Reason | Mods |\n|---|---:|\n");
    let mut failures: Vec<_> = failures.into_iter().collect();
    failures.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    for (reason, mods) in failures {
        out.push_str(&format!("| {reason} | {} |\n", mods.len()));
    }
    out.push_str(
        "\n## Per mod\n\n| Mod | Version | Entrypoints ok/total | Missing | Undispatched events | Mixins | Init (ms) |\n|---|---|---:|---:|---:|---|---:|\n",
    );
    for m in &report.mods {
        let ok = m.entrypoints.iter().filter(|entry| entry.ok).count();
        let mixins = m
            .mixins
            .iter()
            .filter(|mixin| mixin.outcome == "applied")
            .count();
        let undispatched = m.events.iter().filter(|event| !event.dispatched).count();
        let init: u64 = m.entrypoints.iter().map(|entry| entry.elapsed_ms).sum();
        out.push_str(&format!(
            "| `{}` | {} | {}/{} | {} | {} | {}/{} | {} |\n",
            m.id,
            m.version,
            ok,
            m.entrypoints.len(),
            m.missing.len(),
            undispatched,
            mixins,
            m.mixins.len(),
            init
        ));
    }
    if !report.skipped.is_empty() {
        out.push_str("\n## Skipped jars\n\n| File | Reason |\n|---|---|\n");
        for skipped in &report.skipped {
            out.push_str(&format!("| `{}` | {} |\n", skipped.file, skipped.reason));
        }
    }
    out
}

/// The `--compare` section: symbols gained/lost, per-mod missing deltas and
/// entrypoints that changed status.
pub fn compare_markdown(old: &Report, new: &Report) -> String {
    let before_ranked = rank_symbols(&old.mods);
    let after_ranked = rank_symbols(&new.mods);
    let before: BTreeMap<String, usize> = before_ranked
        .iter()
        .map(|symbol| (symbol.symbol.clone(), symbol.mods))
        .collect();
    let after: BTreeMap<String, usize> = after_ranked
        .iter()
        .map(|symbol| (symbol.symbol.clone(), symbol.mods))
        .collect();
    let mut out = String::from("### Missing symbols\n\n");
    let gained: Vec<_> = before
        .iter()
        .filter(|(symbol, _)| !after.contains_key(*symbol))
        .collect();
    let lost: Vec<_> = after
        .iter()
        .filter(|(symbol, _)| !before.contains_key(*symbol))
        .collect();
    if gained.is_empty() && lost.is_empty() {
        out.push_str("No symbols appeared or disappeared.\n\n");
    } else {
        for (symbol, mods) in gained {
            out.push_str(&format!("- fixed: `{symbol}` (was in {mods} mods)\n"));
        }
        for (symbol, mods) in lost {
            out.push_str(&format!("- new: `{symbol}` ({mods} mods)\n"));
        }
        out.push('\n');
    }
    let old_mods: BTreeMap<&str, usize> = old
        .mods
        .iter()
        .map(|m| (m.id.as_str(), m.missing.len()))
        .collect();
    let mut changed = Vec::new();
    for m in &new.mods {
        let was = old_mods.get(m.id.as_str()).copied().unwrap_or(0);
        if was != m.missing.len() {
            changed.push(format!(
                "- `{}`: {} missing (was {was})\n",
                m.id,
                m.missing.len()
            ));
        }
    }
    out.push_str("### Mods whose missing count changed\n\n");
    if changed.is_empty() {
        out.push_str("None.\n\n");
    } else {
        out.push_str(&changed.join(""));
        out.push('\n');
    }
    let old_status: BTreeMap<(&str, &str), bool> = old
        .mods
        .iter()
        .flat_map(|m| {
            m.entrypoints
                .iter()
                .map(move |entry| ((m.id.as_str(), entry.class.as_str()), entry.ok))
        })
        .collect();
    let mut flips = Vec::new();
    for m in &new.mods {
        for entry in &m.entrypoints {
            let was = old_status.get(&(m.id.as_str(), entry.class.as_str()));
            if was.is_some_and(|was| *was != entry.ok) {
                flips.push(format!(
                    "- `{}`: {} is now {}\n",
                    m.id,
                    entry.class,
                    if entry.ok { "ok" } else { "failing" }
                ));
            }
        }
    }
    out.push_str("### Entrypoints that changed status\n\n");
    if flips.is_empty() {
        out.push_str("None.\n");
    } else {
        out.push_str(&flips.join(""));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missing(symbol: &str, hits: u64, caller: Option<&str>) -> MissingSymbol {
        MissingSymbol {
            symbol: symbol.into(),
            hits,
            caller: caller.map(Into::into),
        }
    }

    fn record(id: &str, missing: Vec<MissingSymbol>) -> ModRecord {
        ModRecord {
            id: id.into(),
            file: format!("{id}.jar"),
            version: "1".into(),
            entrypoints: Vec::new(),
            missing,
            events: Vec::new(),
            mixins: Vec::new(),
            standins: Vec::new(),
            counts: Counts::default(),
        }
    }

    #[test]
    fn missing_symbols_strip_the_caller_and_group() {
        let grouped = group_missing(&[
            "net/minecraft/world/item/ItemStack.getItem()Lnet/minecraft/world/item/Item; <- com/a/Mod.init <- com/a/Hook.run".into(),
            "net/minecraft/world/item/ItemStack.getItem()Lnet/minecraft/world/item/Item;".into(),
            "class net/minecraft/world/level/Level".into(),
        ]);
        assert_eq!(grouped.len(), 2);
        let item = grouped
            .iter()
            .find(|symbol| symbol.symbol.contains("ItemStack.getItem"))
            .unwrap();
        assert_eq!(item.hits, 2);
        assert_eq!(item.caller.as_deref(), Some("com/a/Mod.init"));
        assert_eq!(
            grouped[0].symbol,
            "net/minecraft/world/item/ItemStack.getItem()Lnet/minecraft/world/item/Item;"
        );
    }

    #[test]
    fn ranking_counts_distinct_mods_first_then_hits() {
        let a = record(
            "a",
            vec![missing("x.Y", 5, Some("a")), missing("class Z", 1, None)],
        );
        let b = record("b", vec![missing("x.Y", 1, None)]);
        let ranked = rank_symbols(&[a, b]);
        assert_eq!(ranked[0].symbol, "x.Y");
        assert_eq!(ranked[0].mods, 2);
        assert_eq!(ranked[0].hits, 6);
        assert_eq!(ranked[0].mod_ids, ["a", "b"]);
        // A single mod with one hit ranks below a symbol two mods hit.
        assert_eq!(ranked[1].symbol, "class Z");
    }

    #[test]
    fn owners_group_by_the_class_part() {
        let ranked = rank_owners(&[
            record(
                "a",
                vec![
                    missing("x.Y", 1, None),
                    missing("x.Z", 1, None),
                    missing("class q.R", 1, None),
                ],
            ),
            record("b", vec![missing("x.Y", 1, None)]),
        ]);
        assert_eq!(ranked[0], ("x".into(), 2));
        assert_eq!(ranked[1], ("q.R".into(), 1));
    }

    #[test]
    fn the_dispatch_and_native_lists_have_no_duplicates() {
        let mut seen = BTreeSet::new();
        for event in crate::bridge::DISPATCHED_EVENTS {
            assert!(seen.insert(*event), "duplicate dispatched event {event}");
        }
        let mut seen = BTreeSet::new();
        for class in crate::minecraft::native_classes() {
            assert!(seen.insert(*class), "duplicate native class {class}");
        }
    }

    #[test]
    fn the_report_json_round_trips() {
        let report = Report {
            version: SCHEMA_VERSION,
            hfc_commit: "abc".into(),
            mode: "alone".into(),
            resources: false,
            corpus: vec![CorpusEntry {
                file: "a.jar".into(),
                id: "a".into(),
                version: "1".into(),
                sha256: "00".into(),
            }],
            mods: vec![record("a", vec![missing("x.Y", 1, Some("a"))])],
            shared: SharedRecord::default(),
            skipped: Vec::new(),
        };
        let text = serde_json::to_vec(&report).unwrap();
        let back: Report = serde_json::from_slice(&text).unwrap();
        assert_eq!(back, report);
    }

    #[test]
    fn compare_reports_fixed_and_new_symbols() {
        let old = Report {
            version: SCHEMA_VERSION,
            hfc_commit: "old".into(),
            mode: "alone".into(),
            resources: false,
            corpus: Vec::new(),
            mods: vec![record("a", vec![missing("old.Missing", 1, None)])],
            shared: SharedRecord::default(),
            skipped: Vec::new(),
        };
        let new = Report {
            version: SCHEMA_VERSION,
            hfc_commit: "new".into(),
            mode: "alone".into(),
            resources: false,
            corpus: Vec::new(),
            mods: vec![record("a", vec![missing("new.Missing", 1, None)])],
            shared: SharedRecord::default(),
            skipped: Vec::new(),
        };
        let text = compare_markdown(&old, &new);
        assert!(
            text.contains("fixed: `old.Missing` (was in 1 mods)"),
            "{text}"
        );
        assert!(text.contains("new: `new.Missing` (1 mods)"), "{text}");
    }
}
