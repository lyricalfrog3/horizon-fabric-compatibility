//! Bridge connecting Horizon's native host hooks to the dynamic Fabric runtime.

pub use crate::creative_tab_builtins::ExportedCreativeTab;
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

pub use crate::place_builtins::PlacementRequest;
pub use blocks::{
    BlockChange, BlockContainer, BlockEffects, BlockRemoval, BlockSchedule, BlockSound, BlockUse,
    ContainerRead, ItemDrop, ParticleBurst, PlacedBy, Placement, RegisteredBlock, RegisteredState,
};
pub use items::{CreativeTabAddition, RegisteredItem, TabPlacement};

/// A desktop action a mod asked for; the runtime cannot reach the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRequest {
    /// `Blaze3D.openUri`: show the link in the system browser.
    OpenUri(String),
    /// `Blaze3D.openPath`: show the folder or file in the file manager.
    OpenPath(String),
    /// `ClipboardManager.setClipboard`: put the text on the system clipboard.
    SetClipboard(String),
}

/// Host interface matching Horizon's `mod_api::Host` services.
pub trait HostBridge {
    fn block(&self, pos: [i32; 3]) -> String;
    fn block_property(&self, pos: [i32; 3], key: &str) -> Option<String>;
    fn set_block_property(&mut self, pos: [i32; 3], key: &str, value: &str) -> bool;
    fn give(&mut self, item: &str, count: u32) -> std::result::Result<(), String>;
    /// Remove the block at `pos` the way a player breaking it would (drops
    /// included). Returns whether a block was removed. Hosts that cannot
    /// break blocks leave the default, which reports nothing was removed.
    fn break_block(&mut self, pos: [i32; 3]) -> bool {
        let _ = pos;
        false
    }
    /// The item id in the player's main hand, if the host knows it.
    fn held_item(&self) -> Option<String> {
        None
    }
    /// The item id in the player's off hand, if the host knows it.
    fn off_hand_item(&self) -> Option<String> {
        None
    }
    /// Enchantment ids and levels on the main-hand item.
    fn held_enchantments(&self) -> Vec<(String, i32)> {
        Vec::new()
    }
    fn drop_item(&mut self, pos: [i32; 3], item: &str, count: u32) {
        let _ = pos;
        let _ = self.give(item, count);
    }
    /// Every property of the block at `pos`, `(name, value)`. Hosts that
    /// cannot say leave the default (no properties).
    fn block_properties(&self, pos: [i32; 3]) -> Vec<(String, String)> {
        let _ = pos;
        Vec::new()
    }
    /// A mod block's scheduled tick is due in `delay` game ticks: the host
    /// runs it with `FabricBridge::tick_block` when it is. Hosts that do not
    /// tick blocks leave the default, which drops it.
    fn schedule_block_tick(&mut self, pos: [i32; 3], block: &str, delay: u32) {
        let _ = (pos, block, delay);
    }
    /// A particle burst a mod's block code sent (`ServerLevel.sendParticles`):
    /// `count` particles at `at`, spread by `spread`, with `speed`.
    fn particle(&mut self, id: &str, at: [f64; 3], count: u32, spread: [f64; 3], speed: f64) {
        let _ = (id, at, count, spread, speed);
    }
    /// Remove the block at `pos` as `Level.removeBlock`/`destroyBlock` would:
    /// set it to air, dropping its loot when `drop`. Hosts that cannot leave
    /// the default, which reports nothing was removed.
    fn remove_block(&mut self, pos: [i32; 3], drop: bool) -> bool {
        let _ = (pos, drop);
        false
    }
    /// Whether `remove_block` works on this host.
    fn can_remove_blocks(&self) -> bool {
        false
    }
}

/// Control flow matching Horizon's `mod_api::Flow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Cancel,
}

/// A tooltip laid out from the mods' real `ClientTooltipComponent` list, in
/// GUI pixels relative to the tooltip's text origin. The component draws come
/// first (text, then images), followed by the mods' `POSTEXT` draws.
#[derive(Debug, Clone, Default)]
pub struct TooltipRender {
    pub width: i32,
    pub height: i32,
    pub draws: Vec<GuiDraw>,
}

/// The identifier of a GUI texture entry in a mod jar:
/// `assets/<namespace>/textures/gui/<path>.png` is
/// `<namespace>:textures/gui/<path>.png`, the name a mod passes to
/// `TextureManager.getTexture`.
fn gui_texture_key(name: &str) -> Option<String> {
    let rest = name.strip_prefix("assets/")?;
    let (namespace, path) = rest.split_once('/')?;
    (path.starts_with("textures/gui/") && path.ends_with(".png"))
        .then(|| format!("{namespace}:{path}"))
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RightClickBlockEvent {
    pub pos: [i32; 3],
    pub face: [i32; 3],
    /// Where on the block the click landed (`UseOnContext.getClickLocation`).
    pub hit: [f64; 3],
    /// `Player.isSecondaryUseActive` (sneaking).
    pub sneaking: bool,
    /// The click is the off hand's turn (`Player.getItemInHand(OFF_HAND)`).
    pub off_hand: bool,
}

/// One entrypoint as the census saw it.
#[derive(Debug, Clone, PartialEq)]
pub struct EntrypointRun {
    /// Binary class name (`com/example/Mod`).
    pub class_name: String,
    pub method_name: String,
    pub ok: bool,
    pub error: Option<String>,
    pub instructions: u64,
    pub elapsed_ms: u64,
    /// The entrypoint used the whole budget; it may be cut short.
    pub budget_exhausted: bool,
    /// Symbols the interpreter could not resolve while it ran.
    pub missing: Vec<String>,
    /// Event listeners the entrypoint added, as `(owner.FIELD, listeners)`.
    pub events: Vec<(String, usize)>,
}

/// What the runtime saw while `init` ran, for the compatibility census
/// (`examples/census.rs`). `None` in the game, so it pays nothing.
#[derive(Debug, Clone, Default)]
pub struct CensusLog {
    pub entrypoints: Vec<EntrypointRun>,
}

/// Every event object the bridge dispatches, as `owner class.FIELD` with the
/// binary class name the runtime keeps. The census reports listeners on events
/// that are not in this list.
pub const DISPATCHED_EVENTS: &[&str] = &[
    "net/fabricmc/fabric/api/event/player/UseBlockCallback.EVENT",
    "net/fabricmc/fabric/api/event/player/PlayerBlockBreakEvents.BEFORE",
    "net/fabricmc/fabric/api/event/lifecycle/v1/ServerLifecycleEvents.SERVER_STARTING",
    "net/fabricmc/fabric/api/client/item/v1/ItemTooltipCallback.EVENT",
    "net/fabricmc/fabric/api/client/screen/v1/ScreenEvents.AFTER_INIT",
    "net/fabricmc/fabric/api/client/event/lifecycle/v1/ClientTickEvents.END_CLIENT_TICK",
    "com/anthonyhilyard/iceberg/events/client/RenderTooltipEvents.GATHER",
    "com/anthonyhilyard/iceberg/events/client/RenderTooltipEvents.COLOREXT",
    "com/anthonyhilyard/iceberg/events/client/RenderTooltipEvents.POSTEXT",
    "com/anthonyhilyard/iceberg/events/client/RenderTickEvents.START",
    "com/anthonyhilyard/prism/events/client/RenderTickEvent.START",
    "io/github/jamalam360/jamlib/api/events/InteractionEvent.USE_BLOCK",
];

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
/// cap aborted legitimate initialization and left mods half-registered. Large
/// block registries need more still (mdm: ~46M); see
/// `docs/architecture/fabric-runtime.md` for the measurements.
const ENTRYPOINT_BUDGET: u64 = 100_000_000;

/// The most instructions one `getShape` or `getCollisionShape` call may use in
/// `read_block_shapes`.
const BLOCK_SHAPE_BUDGET: u64 = 100_000_000;

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
    /// When the previous render-tick event was dispatched, for the real
    /// `DeltaTracker` a hovered tooltip's shine and model animation read.
    last_render_tick: Option<std::time::Instant>,
    /// The tooltip the host is showing (item and lines), the stack object
    /// standing for it, and the container screen whose hovered slot holds
    /// that stack. Kept while the same tooltip shows, so a mod that compares
    /// the hovered stack by identity (vanilla keeps one `ItemStack` per slot)
    /// sees a new hover only when the host shows a different tooltip.
    hovered_tooltip: Option<(HoveredTooltipKey, usize, Option<usize>)>,
    /// The block facts read from the mod's blocks, and the classpath key they
    /// belong to (`bridge/block_cache.rs`).
    block_facts_cache: block_cache::BlockFactsCache,
    /// Block ids whose use was already reported as changing the held item, so
    /// a click on one does not flood the log.
    hand_change_logged: std::collections::HashSet<String>,
    /// Block ids whose tick had to keep its heap scope (class init, a static
    /// or a cache kept a scoped object), logged once each.
    scope_kept_logged: std::collections::HashSet<String>,
    /// The compatibility census's recorder; `None` in the game.
    pub census: Option<CensusLog>,
}

pub(crate) type HoveredTooltipKey = (String, Vec<(String, u32)>);

impl Default for FabricBridge {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) mod blocks;
mod events;
pub(crate) mod items;
mod menu;
mod screen;
mod textures;

mod block_cache;

impl FabricBridge {
    /// Load the block/item registries and tags from a Horizon resources
    /// directory, so mods can resolve ids and tags.
    pub fn load_registries(&mut self, resources: &std::path::Path) {
        let mut loaded = crate::registry::Registries::load(resources);
        loaded.absorb_tags(std::mem::take(&mut self.interp.heap.registries));
        self.interp.heap.registries = loaded;
    }

    /// Let mods read and write files under `dir` (the game directory) on disk.
    /// Until this is called, mod file access stays in memory.
    ///
    /// The block facts read out of the mod's blocks are cached under the same
    /// folder's `.horizon-cache/fabric-blocks`, beside the generated content
    /// mods, so a later start with the same classpath does not read them again
    /// (`bridge/block_cache.rs`). [`FabricBridge::set_block_facts_cache`]
    /// overrides that.
    pub fn set_game_dir(&mut self, dir: Option<std::path::PathBuf>) {
        self.block_facts_cache = block_cache::BlockFactsCache::new(
            dir.as_ref()
                .map(|dir| dir.join(".horizon-cache").join(block_cache::CACHE_DIR)),
        );
        self.interp.heap.fs.set_root(dir);
    }

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
            last_render_tick: None,
            hovered_tooltip: None,
            block_facts_cache: block_cache::BlockFactsCache::new(None),
            hand_change_logged: std::collections::HashSet::new(),
            scope_kept_logged: std::collections::HashSet::new(),
            census: None,
        }
    }

    /// Register the tags a mod jar (and the jars inside it) carry in their data packs.
    fn add_jar_tags(&mut self, path: &Path) {
        let mut nested = Vec::new();
        if let Ok(file) = std::fs::File::open(path) {
            if let Ok(archive) = zip::ZipArchive::new(file) {
                nested = archive
                    .file_names()
                    .filter_map(|n| n.ok().map(|n| n.to_string()))
                    .filter(|n| n.starts_with("META-INF/jars/") && n.ends_with(".jar"))
                    .collect();
            }
        }
        let registries = &mut self.interp.heap.registries;
        registries.add_jar_tags(path, None);
        for entry in nested {
            registries.add_jar_tags(path, Some(&entry));
        }
    }

    pub fn loaded_mods(&self) -> &[FabricModMetadata] {
        &self.mods
    }

    /// Start recording what `init` sees, for the census. Off by default.
    pub fn enable_census(&mut self) {
        self.census = Some(CensusLog::default());
        self.interp.trace_missing = true;
        self.interp.trace_calls = true;
    }

    /// The census recorder, when one is on.
    pub fn census(&self) -> Option<&CensusLog> {
        self.census.as_ref()
    }

    /// Whether the loader can load `class` (dotted or binary), for the census's
    /// "applied or ignored" mixin column.
    pub fn census_interprets(&mut self, class: &str) -> bool {
        self.interp
            .loader
            .get_or_load(&class.replace('.', "/"))
            .is_ok()
    }

    /// The entrypoints the census recorded.
    pub fn census_entrypoints(&self) -> &[EntrypointRun] {
        self.census
            .as_ref()
            .map_or(&[], |census| census.entrypoints.as_slice())
    }

    /// Symbols the interpreter could not resolve, as `note_missing` wrote them.
    pub fn census_missing(&self) -> &[String] {
        &self.interp.missing
    }

    /// The mixin manifest this bridge built from the loaded jars.
    pub fn census_mixins(&self) -> &crate::mixin::MixinManifest {
        &self.mixins
    }

    /// Event objects with listeners that the runtime kept in statics, as
    /// `owner class.FIELD` (binary names) and the listener count.
    pub fn census_events(&self) -> Vec<(String, usize)> {
        let mut found: std::collections::BTreeMap<String, usize> = Default::default();
        let mut record = |owner: &str, field: &str, value: &Value| {
            let Some(id) = value.as_obj() else {
                return;
            };
            let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get(id) else {
                return;
            };
            let listeners = fields
                .get("listeners")
                .and_then(Value::as_obj)
                .and_then(|id| match self.interp.heap.get(id) {
                    Some(JavaObject::Array { elements, .. }) => Some(elements.len()),
                    _ => None,
                })
                .unwrap_or(0);
            let single = fields.get("listener").is_some_and(|value| !value.is_null()) as usize;
            if listeners + single > 0 {
                found.insert(format!("{owner}.{field}"), listeners + single);
            }
        };
        let classes: Vec<(String, Vec<(String, Value)>)> = self
            .interp
            .loader
            .classes()
            .map(|(name, class)| {
                (
                    name.to_string(),
                    class
                        .static_fields
                        .iter()
                        .map(|(field, value)| (field.clone(), value.clone()))
                        .collect(),
                )
            })
            .collect();
        for (owner, fields) in classes {
            for (field, value) in fields {
                record(&owner, &field, &value);
            }
        }
        let singletons: Vec<((String, String), Value)> = self
            .interp
            .static_singletons
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        for ((owner, field), value) in singletons {
            record(&owner, &field, &value);
        }
        found.into_iter().collect()
    }

    /// `(blocks, items, creative-tab entries)` the runtime's registries hold.
    pub fn census_counts(&self) -> (usize, usize, usize) {
        (
            self.interp.heap.registered_blocks.len(),
            self.interp.heap.registered_items.len(),
            self.interp.heap.creative_tab_additions.len()
                + self.interp.heap.creative_tab_exports.len(),
        )
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
            last_render_tick: None,
            hovered_tooltip: None,
            block_facts_cache: block_cache::BlockFactsCache::new(None),
            hand_change_logged: std::collections::HashSet::new(),
            scope_kept_logged: std::collections::HashSet::new(),
            census: None,
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

    /// The desktop actions mods asked for since the last call, oldest first.
    /// The host performs them (it owns the browser, file manager and clipboard).
    pub fn take_host_requests(&mut self) -> Vec<HostRequest> {
        std::mem::take(&mut self.interp.heap.host_requests)
    }

    /// The system clipboard's text, for `ClipboardManager.getClipboard`.
    pub fn set_clipboard_text(&mut self, text: &str) {
        self.interp.heap.clipboard = text.to_string();
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

    /// Run a static method of a loaded class under an instruction budget, with
    /// missing-symbol tracking on; the differential JVM test uses it. The
    /// value must be a `java.lang.String`.
    #[doc(hidden)]
    pub fn debug_call_static(
        &mut self,
        class: &str,
        method: &str,
        descriptor: &str,
        budget: u64,
    ) -> Result<String> {
        let previous_budget = self.interp.step_budget;
        let previous_trace = self.interp.trace_missing;
        self.interp.step_budget = Some(budget);
        self.interp.trace_missing = true;
        self.interp.missing.clear();
        let result = self.interp.execute_method(class, method, descriptor, &[]);
        self.interp.step_budget = previous_budget;
        self.interp.trace_missing = previous_trace;
        let value = result?;
        value
            .as_obj()
            .and_then(|id| self.interp.heap.get_string(id))
            .map(str::to_string)
            .ok_or_else(|| {
                hfc_core::Error::new(
                    "interpreter",
                    format!("{class}.{method} did not return a String"),
                )
            })
    }

    /// The symbols the last [`FabricBridge::debug_call_static`] could not
    /// resolve.
    #[doc(hidden)]
    pub fn debug_missing(&self) -> Vec<String> {
        self.interp.missing.clone()
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
        self.add_jar_tags(path);

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

                // 2b. The real sizes of the GUI textures the jar ships, for
                // `TextureManager.getTexture(id).getTexture()` readers.
                for i in 0..archive.len() {
                    if let Ok(mut entry) = archive.by_index(i) {
                        if let Ok(name) = entry.name() {
                            let name = name.to_string();
                            if let Some(key) = gui_texture_key(&name) {
                                let mut head = [0u8; 24];
                                if std::io::Read::read_exact(&mut entry, &mut head).is_ok()
                                    && let Some(size) = crate::builtins::png_size(&head)
                                {
                                    self.interp.heap.gui_texture_sizes.insert(key, size);
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
                                            } else if let Some(key) = gui_texture_key(&name) {
                                                let mut head = [0u8; 24];
                                                if std::io::Read::read_exact(
                                                    &mut sub_entry,
                                                    &mut head,
                                                )
                                                .is_ok()
                                                    && let Some(size) =
                                                        crate::builtins::png_size(&head)
                                                {
                                                    self.interp
                                                        .heap
                                                        .gui_texture_sizes
                                                        .insert(key, size);
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
        self.interp.mixin_interfaces.clear();
        self.interp.mixin_interface_methods.clear();
        for added in &self.mixins.interface_methods {
            let interfaces = self
                .interp
                .mixin_interfaces
                .entry(added.target_class.clone())
                .or_default();
            if !interfaces.contains(&added.interface) {
                interfaces.push(added.interface.clone());
            }
            self.interp
                .mixin_interface_methods
                .entry((
                    added.interface.clone(),
                    added.method.clone(),
                    added.descriptor.clone(),
                ))
                .or_default()
                .push(added.clone());
        }
        self.interp.mixin_accessors = self
            .mixins
            .accessors
            .iter()
            .map(|accessor| {
                (
                    (
                        accessor.interface.clone(),
                        accessor.method.clone(),
                        accessor.descriptor.clone(),
                    ),
                    accessor.clone(),
                )
            })
            .collect();

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
            let missing_start = self.interp.missing.len();
            let events_before: std::collections::BTreeMap<String, usize> =
                self.census_events().into_iter().collect();
            let started = std::time::Instant::now();
            let res = self.interp.execute_method(
                &entry.class_name,
                &entry.method_name,
                &entry.descriptor,
                &[],
            );
            let used = ENTRYPOINT_BUDGET - self.interp.step_budget.unwrap_or(0);
            let events: Vec<(String, usize)> = self
                .census_events()
                .into_iter()
                .filter(|(event, count)| events_before.get(event).copied().unwrap_or(0) < *count)
                .collect();
            if let Some(census) = &mut self.census {
                census.entrypoints.push(EntrypointRun {
                    class_name: entry.class_name.clone(),
                    method_name: entry.method_name.clone(),
                    ok: res.is_ok(),
                    error: res.as_ref().err().map(ToString::to_string),
                    instructions: used,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    budget_exhausted: used >= ENTRYPOINT_BUDGET,
                    missing: self.interp.missing[missing_start..].to_vec(),
                    events,
                });
            }
            log::debug!(
                "entrypoint {}.{} used {} instructions in {:?}",
                entry.class_name,
                entry.method_name,
                used,
                started.elapsed()
            );
            if entry.method_name == "onInitialize" && used > 1_000_000 {
                log::info!(
                    "Fabric: entrypoint {}.{} cost {used} of {ENTRYPOINT_BUDGET} instructions \
                     in {:.1?}",
                    entry.class_name,
                    entry.method_name,
                    started.elapsed()
                );
            }
            if let Err(e) = res {
                log::warn!(
                    "Mod entrypoint {}.{}{} failed to initialize: {e}",
                    entry.class_name,
                    entry.method_name,
                    entry.descriptor
                );
            }
        }

        // The entrypoints have registered their tab listeners and items.
        self.interp.step_budget = Some(ENTRYPOINT_BUDGET);
        self.interp.run_creative_tab_events();
        // Every block object exists now; read their states and shapes. The
        // objects themselves have to exist at play time (C1 placement and
        // C2/C3 use run the mod's own code), so the entrypoints always run;
        // reading the facts back out of them is what a cache skips when this
        // classpath was read before. Each block gets its own budget so one bad
        // block cannot starve the rest.
        let started = std::time::Instant::now();
        let registered = self.interp.heap.registered_blocks.len();
        // With no registered block there is nothing to cache, so the classpath
        // is not hashed for an empty list: a game without block mods must not
        // pay for the cache at all.
        if registered == 0 || !self.load_block_facts_cache() {
            self.interp.collect_block_facts(ENTRYPOINT_BUDGET);
            let blocks = self.interp.heap.block_facts.len();
            if blocks > 0 {
                log::info!(
                    "Fabric: facts of {blocks} registered block(s) read in {:.1?} (first start \
                     with this classpath; later starts reuse them from \
                     .horizon-cache/fabric-blocks)",
                    started.elapsed()
                );
            }
        }
        self.interp.step_budget = prev_budget;

        self.initialized = true;
        Ok(())
    }
}
