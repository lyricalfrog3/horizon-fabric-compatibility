//! Bridge connecting Horizon's native host hooks to the dynamic Fabric runtime.

use crate::interp::Interpreter;
use crate::loader::ClassLoader;
use crate::minecraft::{
    BlockPos, BlockState, Direction, EventResult, GuiDraw, GuiGraphics, ItemStack, Level, MenuKind,
    MenuWidget, Player,
};
use crate::mixin::{self, MixinManifest};
use crate::value::{JavaObject, Value};
use hfc_core::Result;
use std::collections::HashMap;
use std::path::Path;

/// Host interface matching Horizon's `mod_api::Host` services.
pub trait HostBridge {
    fn block(&self, pos: [i32; 3]) -> String;
    fn block_property(&self, pos: [i32; 3], key: &str) -> Option<String>;
    fn set_block_property(&mut self, pos: [i32; 3], key: &str, value: &str) -> bool;
    fn give(&mut self, item: &str, count: u32) -> std::result::Result<(), String>;
    fn drop_item(&mut self, pos: [i32; 3], item: &str, count: u32) {
        let _ = pos;
        let _ = self.give(item, count);
    }
}

/// Control flow matching Horizon's `mod_api::Flow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Cancel,
}

/// Metadata for a discovered Fabric mod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricModMetadata {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub authors: Vec<String>,
    pub is_nested: bool,
    /// The `fabric.mod.json` this entry was read from. Fabric Loader exposes
    /// unknown top-level keys through `ModMetadata.getCustomValue` and
    /// `containsCustomValue`; Mod Menu reads `fabric-api:module-lifecycle`
    /// there to mark a mod as a library and `modmenu` for parent/badge data.
    pub raw: String,
    /// The jar holding this mod's resources, and for a nested mod the jar
    /// inside it. `ModContainer.getPath` resolves a mod's icon path here.
    pub jar: Option<String>,
    pub nested_jar: Option<String>,
}

/// The identity a jar declares in its `fabric.mod.json` ([`FabricBridge::jar_summary`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JarSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
}

/// Event matching Horizon's `RightClickBlockEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RightClickBlockEvent {
    pub pos: [i32; 3],
    pub face: [i32; 3],
}

/// A discovered mod entrypoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModEntrypoint {
    pub class_name: String,
    pub method_name: String,
    pub descriptor: String,
}

impl ModEntrypoint {
    pub fn main(class_name: impl Into<String>) -> Self {
        Self {
            class_name: class_name.into(),
            method_name: "onInitialize".into(),
            descriptor: "()V".into(),
        }
    }

    pub fn client(class_name: impl Into<String>) -> Self {
        Self {
            class_name: class_name.into(),
            method_name: "onInitializeClient".into(),
            descriptor: "()V".into(),
        }
    }
}

/// Instructions one entrypoint may run before the host gives up on it.
///
/// A JVM has no such limit; this only keeps a mis-decoded branch from hanging
/// the render thread. It must still be generous: Fabric API's registry and
/// attachment sync walk every block, item and registry entry, and real mods
/// resolve their whole config/`Codec` graph at init. With the game's real
/// Minecraft and JDK classes those entrypoints need millions of instructions
/// (Mod Menu alone ~370k, Fabric's attachment sync ~4.9M), so the old 200k
/// cap aborted legitimate initialization and left mods half-registered.
const ENTRYPOINT_BUDGET: u64 = 10_000_000;

/// The Fabric compatibility bridge managing mod execution.
pub struct FabricBridge {
    pub interp: Interpreter,
    pub entrypoints: Vec<ModEntrypoint>,
    pub mods: Vec<FabricModMetadata>,
    pub raw_translations: HashMap<String, HashMap<String, String>>,
    pub translations: HashMap<String, String>,
    pub initialized: bool,
    /// The screen object currently hosted by the client, and the GUI size it
    /// was initialized for. A changed object or size re-runs `Screen.init`.
    screen_id: Option<usize>,
    screen_width: i32,
    screen_height: i32,
    /// The button held since a press on the screen (26.3's `MouseHandler`
    /// `activeButton`), which turns mouse movement into `mouseDragged` so a
    /// scrollbar thumb or slider follows the pointer.
    drag_button: Option<i32>,
    /// The last pointer position, for the drag deltas.
    last_mouse: (f64, f64),
    /// Mixin injectors read from the loaded mods.
    pub mixins: MixinManifest,
    /// Widget ids a host menu's layout pass contributed, by menu and size.
    menu_cache: HashMap<(MenuKind, i32, i32), Vec<usize>>,
    /// The synthetic screen a menu contribution was built for. When a mod's
    /// Done button installs it back, the host must see "no Fabric screen".
    menu_screen: Option<usize>,
    gui_texture_cache: HashMap<String, Option<crate::minecraft::PngImage>>,
}

impl Default for FabricBridge {
    fn default() -> Self {
        Self::new()
    }
}

mod events;
mod menu;
mod screen;
mod textures;

impl FabricBridge {
    pub fn new() -> Self {
        Self {
            interp: Interpreter::new(),
            entrypoints: Vec::new(),
            mods: Vec::new(),
            raw_translations: HashMap::new(),
            translations: HashMap::new(),
            initialized: false,
            screen_id: None,
            screen_width: 0,
            screen_height: 0,
            drag_button: None,
            last_mouse: (0.0, 0.0),
            mixins: MixinManifest::default(),
            menu_cache: HashMap::new(),
            menu_screen: None,
            gui_texture_cache: HashMap::new(),
        }
    }

    pub fn loaded_mods(&self) -> &[FabricModMetadata] {
        &self.mods
    }

    pub fn with_loader(loader: ClassLoader) -> Self {
        Self {
            interp: Interpreter::with_loader(loader),
            entrypoints: Vec::new(),
            mods: Vec::new(),
            raw_translations: HashMap::new(),
            translations: HashMap::new(),
            initialized: false,
            screen_id: None,
            screen_width: 0,
            screen_height: 0,
            drag_button: None,
            last_mouse: (0.0, 0.0),
            mixins: MixinManifest::default(),
            menu_cache: HashMap::new(),
            menu_screen: None,
            gui_texture_cache: HashMap::new(),
        }
    }

    /// Rebuild active translations for the given language code, falling back to en_us.
    pub fn reload_translations(&mut self, current_code: &str) {
        let mut merged = self
            .raw_translations
            .get("en_us")
            .cloned()
            .unwrap_or_default();
        if current_code != "en_us" {
            if let Some(other) = self.raw_translations.get(current_code) {
                merged.extend(other.clone());
            }
        }
        self.interp.heap.translations = merged.clone();
        self.translations = merged;
    }

    pub fn translations(&self) -> &HashMap<String, String> {
        &self.translations
    }

    /// Merge the host's active language into the runtime dictionary. Mod
    /// resources provide their own keys; vanilla GUI keys such as `gui.done`
    /// come from the client's loaded language file.
    pub fn extend_translations(&mut self, entries: HashMap<String, String>) {
        self.interp.heap.add_translations(entries.clone());
        self.translations.extend(entries);
    }

    /// Add a mod JAR file.
    pub fn add_jar(&mut self, path: impl AsRef<Path>) -> Result<()> {
        self.interp.loader.add_jar(path)
    }

    /// Register the JDK's `lib/modules` image as a class source.
    pub fn add_jimage(&mut self, path: impl AsRef<Path>) -> Result<()> {
        self.interp.loader.add_jimage(path)
    }

    /// Walk every entrypoint's transitive class references. Classes found are
    /// parsed and cached; classes that no registered jar or JDK image provides
    /// are reported, which is what a mod will actually fail on.
    pub fn class_closure(&mut self) -> crate::loader::ClosureReport {
        let roots = self
            .entrypoints
            .iter()
            .map(|entrypoint| entrypoint.class_name.clone())
            .collect::<Vec<_>>();
        self.interp.loader.closure(roots)
    }

    /// Add a mod JAR file and automatically discover its `fabric.mod.json` entrypoints
    /// from both the top-level JAR and embedded nested JARs.
    /// What a jar's root `fabric.mod.json` declares, read without loading the
    /// jar. A host that keeps a per-mod on/off list asks this before
    /// [`FabricBridge::load_mod_jar`], so a disabled mod is never initialized
    /// yet can still be listed (and switched back on).
    pub fn jar_summary(path: impl AsRef<Path>) -> Option<JarSummary> {
        let file = std::fs::File::open(path.as_ref()).ok()?;
        let mut archive = zip::ZipArchive::new(file).ok()?;
        let text = mixin::read_zip_entry(&mut archive, "fabric.mod.json")?;
        let json: serde_json::Value = serde_json::from_slice(&text).ok()?;
        let field = |key: &str| json.get(key).and_then(|v| v.as_str()).map(str::to_string);
        let id = field("id")?;
        Some(JarSummary {
            name: field("name").unwrap_or_else(|| id.clone()),
            version: field("version").unwrap_or_else(|| "1.0.0".into()),
            description: field("description").unwrap_or_default(),
            id,
        })
    }

    pub fn load_mod_jar(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        self.interp.loader.add_jar(path)?;

        if let Ok(file) = std::fs::File::open(path) {
            if let Ok(mut archive) = zip::ZipArchive::new(file) {
                // 1. Scan root fabric.mod.json
                if let Ok(mut mod_json) = archive.by_name("fabric.mod.json") {
                    let mut text = String::new();
                    if std::io::Read::read_to_string(&mut mod_json, &mut text).is_ok() {
                        self.parse_and_add_metadata(&text, false, Some(path), None);
                        self.parse_and_add_entrypoints(&text);
                    }
                }

                // 2. Scan translation files
                for i in 0..archive.len() {
                    if let Ok(mut entry) = archive.by_index(i) {
                        let name_opt = entry.name().map(|n| n.to_string());
                        if let Ok(name) = name_opt {
                            if name.starts_with("assets/")
                                && name.ends_with(".json")
                                && name.contains("/lang/")
                            {
                                if let Some(code) = name
                                    .rsplit('/')
                                    .next()
                                    .and_then(|f| f.strip_suffix(".json"))
                                {
                                    let code = code.to_string();
                                    let mut text = String::new();
                                    if std::io::Read::read_to_string(&mut entry, &mut text).is_ok()
                                    {
                                        if let Ok(entries) =
                                            serde_json::from_str::<HashMap<String, String>>(&text)
                                        {
                                            self.raw_translations
                                                .entry(code)
                                                .or_default()
                                                .extend(entries);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // 3. Scan nested JARs for fabric.mod.json and translations
                let mut nested_jars = Vec::new();
                for i in 0..archive.len() {
                    if let Ok(entry) = archive.by_index(i) {
                        if let Ok(name) = entry.name() {
                            if name.ends_with(".jar") && !name.contains("..") {
                                nested_jars.push(name.to_string());
                            }
                        }
                    }
                }

                for nested_name in nested_jars {
                    if let Ok(mut entry) = archive.by_name(&nested_name) {
                        let mut bytes = Vec::new();
                        if std::io::Read::read_to_end(&mut entry, &mut bytes).is_ok() {
                            if let Ok(mut sub_archive) =
                                zip::ZipArchive::new(std::io::Cursor::new(&bytes))
                            {
                                for j in 0..sub_archive.len() {
                                    if let Ok(mut sub_entry) = sub_archive.by_index(j) {
                                        let sub_name_opt = sub_entry.name().map(|n| n.to_string());
                                        if let Ok(name) = sub_name_opt {
                                            if name.starts_with("assets/")
                                                && name.ends_with(".json")
                                                && name.contains("/lang/")
                                            {
                                                if let Some(code) = name
                                                    .rsplit('/')
                                                    .next()
                                                    .and_then(|f| f.strip_suffix(".json"))
                                                {
                                                    let code = code.to_string();
                                                    let mut text = String::new();
                                                    if std::io::Read::read_to_string(
                                                        &mut sub_entry,
                                                        &mut text,
                                                    )
                                                    .is_ok()
                                                    {
                                                        if let Ok(entries) = serde_json::from_str::<
                                                            HashMap<String, String>,
                                                        >(
                                                            &text
                                                        ) {
                                                            self.raw_translations
                                                                .entry(code)
                                                                .or_default()
                                                                .extend(entries);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                if let Ok(mut sub_mod_json) = sub_archive.by_name("fabric.mod.json")
                                {
                                    let mut text = String::new();
                                    if std::io::Read::read_to_string(&mut sub_mod_json, &mut text)
                                        .is_ok()
                                    {
                                        self.parse_and_add_metadata(
                                            &text,
                                            true,
                                            Some(path),
                                            Some(&nested_name),
                                        );
                                        self.parse_and_add_entrypoints(&text);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        self.reload_translations("en_us");
        Ok(())
    }

    fn parse_and_add_metadata(
        &mut self,
        json_str: &str,
        is_nested: bool,
        jar: Option<&Path>,
        nested_jar: Option<&str>,
    ) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
            if let Some(id) = json.get("id").and_then(|v| v.as_str()) {
                let name = json
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(id)
                    .to_string();
                let version = json
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("1.0.0")
                    .to_string();
                let description = json
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut authors = Vec::new();
                if let Some(arr) = json.get("authors").and_then(|v| v.as_array()) {
                    for a in arr {
                        if let Some(s) = a.as_str() {
                            authors.push(s.to_string());
                        } else if let Some(s) = a.get("name").and_then(|v| v.as_str()) {
                            authors.push(s.to_string());
                        }
                    }
                } else if let Some(s) = json.get("authors").and_then(|v| v.as_str()) {
                    authors.push(s.to_string());
                }

                let meta = FabricModMetadata {
                    id: id.to_string(),
                    name,
                    version,
                    description,
                    authors,
                    is_nested,
                    raw: json_str.to_string(),
                    jar: jar.map(|path| path.display().to_string()),
                    nested_jar: nested_jar.map(str::to_string),
                };
                if !self.mods.iter().any(|m| m.id == meta.id) {
                    self.interp.heap.register_mod(meta.clone());
                    self.mods.push(meta);
                }
            }
        }
    }

    fn parse_and_add_entrypoints(&mut self, json_str: &str) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
            if let Some(entrypoints) = json.get("entrypoints").and_then(|e| e.as_object()) {
                if let Some(main) = entrypoints.get("main") {
                    self.extract_entrypoints(main, "onInitialize");
                }
                if let Some(client) = entrypoints.get("client") {
                    self.extract_entrypoints(client, "onInitializeClient");
                }
            }
        }
    }

    fn extract_entrypoints(&mut self, val: &serde_json::Value, default_method: &str) {
        let entries = if let Some(arr) = val.as_array() {
            arr.clone()
        } else {
            vec![val.clone()]
        };

        for entry in entries {
            let ep_str = if let Some(s) = entry.as_str() {
                Some(s.to_string())
            } else {
                entry
                    .get("value")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string)
            };

            if let Some(raw) = ep_str {
                let (cls, method) = if let Some((c, m)) = raw.split_once("::") {
                    (c, m)
                } else {
                    (raw.as_str(), default_method)
                };
                let binary_name = cls.replace('.', "/");
                let ep = ModEntrypoint {
                    class_name: binary_name,
                    method_name: method.to_string(),
                    descriptor: "()V".to_string(),
                };
                if !self.entrypoints.contains(&ep) {
                    self.entrypoints.push(ep);
                }
            }
        }
    }

    /// Register a mod's ModInitializer entrypoint class name.
    pub fn add_entrypoint(&mut self, class_name: impl Into<String>) {
        let cls = class_name.into().replace('.', "/");
        let ep = ModEntrypoint::main(cls);
        if !self.entrypoints.contains(&ep) {
            self.entrypoints.push(ep);
        }
    }

    /// Register a custom ModEntrypoint.
    pub fn add_custom_entrypoint(&mut self, entrypoint: ModEntrypoint) {
        if !self.entrypoints.contains(&entrypoint) {
            self.entrypoints.push(entrypoint);
        }
    }

    /// Initialize all registered Fabric mod entrypoints.
    pub fn init(&mut self) -> Result<()> {
        if self.initialized {
            return Ok(());
        }

        let prev_budget = self.interp.step_budget;
        self.mixins = self.load_mixin_manifest();

        // Run common/main entrypoints (ModInitializer) first, then client entrypoints (ClientModInitializer)
        let mut sorted_entrypoints = self.entrypoints.clone();
        sorted_entrypoints.sort_by_key(|e| {
            if e.method_name == "onInitialize" {
                0
            } else {
                1
            }
        });

        for entry in &sorted_entrypoints {
            self.interp.step_budget = Some(ENTRYPOINT_BUDGET);
            let res = self.interp.execute_method(
                &entry.class_name,
                &entry.method_name,
                &entry.descriptor,
                &[],
            );
            if let Err(e) = res {
                log::warn!(
                    "Mod entrypoint {}.{}{} failed to initialize: {e}",
                    entry.class_name,
                    entry.method_name,
                    entry.descriptor
                );
            }
        }

        self.interp.step_budget = prev_budget;

        self.initialized = true;
        Ok(())
    }
}
