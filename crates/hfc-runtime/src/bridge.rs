//! Bridge connecting Horizon's native host hooks to the dynamic Fabric runtime.

use crate::interp::Interpreter;
use crate::loader::ClassLoader;
use crate::minecraft::{
    BlockPos, BlockState, Direction, EventResult, GuiDraw, GuiGraphics, ItemStack, Level, Player,
};
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
}

impl Default for FabricBridge {
    fn default() -> Self {
        Self::new()
    }
}

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
    pub fn load_mod_jar(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        self.interp.loader.add_jar(path)?;

        if let Ok(file) = std::fs::File::open(path) {
            if let Ok(mut archive) = zip::ZipArchive::new(file) {
                // 1. Scan root fabric.mod.json
                if let Ok(mut mod_json) = archive.by_name("fabric.mod.json") {
                    let mut text = String::new();
                    if std::io::Read::read_to_string(&mut mod_json, &mut text).is_ok() {
                        self.parse_and_add_metadata(&text, false);
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
                                        self.parse_and_add_metadata(&text, true);
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

    fn parse_and_add_metadata(&mut self, json_str: &str, is_nested: bool) {
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

    fn sync_level_to_host(&self, level_id: usize, host: &mut dyn HostBridge) {
        if let Some(final_level) = self.interp.heap.get_native::<Level>(level_id) {
            for (pos, new_state) in &final_level.replanted {
                host.set_block_property(
                    [pos.x, pos.y, pos.z],
                    "age",
                    &new_state.get_age().to_string(),
                );
            }
            for (pos, item, count) in &final_level.dropped_stacks {
                host.drop_item([pos.x, pos.y, pos.z], item, *count as u32);
            }
        }
    }

    /// Dispatch a right-click block event into the dynamic Fabric compatibility layer.
    pub fn right_click_block(
        &mut self,
        host: &mut dyn HostBridge,
        event: &RightClickBlockEvent,
    ) -> Flow {
        if !self.initialized {
            let _ = self.init();
        }

        let block_id = host.block(event.pos);
        if block_id == "minecraft:air" || block_id.is_empty() {
            return Flow::Continue;
        }

        // 1. Prepare initial BlockState from Host
        let mut block_state = BlockState::new(&block_id);
        if let Some(age_str) = host.block_property(event.pos, "age") {
            block_state = block_state.with_property("age", &age_str);
        }

        // 2. Prepare Level and BlockPos on heap
        let mut level = Level::new(false);
        let block_pos = BlockPos {
            x: event.pos[0],
            y: event.pos[1],
            z: event.pos[2],
        };
        level
            .blocks
            .insert([block_pos.x, block_pos.y, block_pos.z], block_state);

        let level_id = self.interp.heap.alloc_level(level);
        let player = Player::new(level_id);
        let player_id = self.interp.heap.alloc_player(player);

        let hit_dir = Direction::from_array(event.face);
        let hit_id = self
            .interp
            .heap
            .alloc_hit_result(block_pos.clone(), hit_dir.clone());
        let pos_id = self
            .interp
            .heap
            .alloc_block_pos(block_pos.x, block_pos.y, block_pos.z);
        let dir_id = self.interp.heap.alloc_direction(hit_dir);
        let main_hand = self
            .interp
            .resolve_static_field("net/minecraft/world/InteractionHand", "MAIN_HAND");

        // 3. Dispatch to UseBlockCallback.EVENT listeners (Fabric API)
        let use_block_event = self.interp.resolve_static_field(
            "net/fabricmc/fabric/api/event/player/UseBlockCallback",
            "EVENT",
        );
        if let Some(event_id) = use_block_event.as_obj() {
            if let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get(event_id) {
                if let Some(Value::Object(Some(arr_id))) = fields.get("listeners") {
                    let arr_id = *arr_id;
                    let listeners = if let Some(JavaObject::Array { elements, .. }) =
                        self.interp.heap.get(arr_id)
                    {
                        elements.clone()
                    } else {
                        Vec::new()
                    };
                    for listener in listeners {
                        let res = self.interp.execute_method(
                            "net/fabricmc/fabric/api/event/player/UseBlockCallback",
                            "interact",
                            "(Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/world/level/Level;Lnet/minecraft/world/InteractionHand;Lnet/minecraft/world/phys/BlockHitResult;)Lnet/minecraft/world/InteractionResult;",
                            &[
                                listener,
                                Value::Object(Some(player_id)),
                                Value::Object(Some(level_id)),
                                main_hand.clone(),
                                Value::Object(Some(hit_id)),
                            ],
                        );
                        if let Ok(val) = res {
                            let is_pass = if let Some(id) = val.as_obj() {
                                if let Some(JavaObject::Instance { fields, .. }) =
                                    self.interp.heap.get(id)
                                {
                                    fields
                                        .get("name")
                                        .and_then(|n| n.as_obj())
                                        .and_then(|nid| self.interp.heap.get_string(nid))
                                        .map(|s| s == "PASS")
                                        .unwrap_or(false)
                                } else {
                                    false
                                }
                            } else {
                                val.is_null()
                            };
                            if !is_pass {
                                self.sync_level_to_host(level_id, host);
                                return Flow::Cancel;
                            }
                        }
                    }
                }
            }
        }

        // 4. Dispatch to InteractionEvent.USE_BLOCK listeners (JamLib / mod events)
        let jamlib_use_block = self.interp.resolve_static_field(
            "io/github/jamalam360/jamlib/api/events/InteractionEvent",
            "USE_BLOCK",
        );
        if let Some(event_id) = jamlib_use_block.as_obj() {
            let mut listeners = Vec::new();
            if let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get(event_id) {
                if let Some(Value::Object(Some(arr_id))) = fields.get("listeners") {
                    let arr_id = *arr_id;
                    if let Some(JavaObject::Array { elements, .. }) = self.interp.heap.get(arr_id) {
                        listeners.extend(elements.clone());
                    }
                }
                if let Some(single) = fields.get("listener") {
                    if !single.is_null() && !listeners.contains(single) {
                        listeners.push(single.clone());
                    }
                }
            }
            for listener in listeners {
                let res = self.interp.execute_method(
                    "io/github/jamalam360/jamlib/api/events/InteractionEvent$UseBlock",
                    "onUseBlock",
                    "(Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/world/InteractionHand;Lnet/minecraft/core/BlockPos;Lnet/minecraft/core/Direction;)Lio/github/jamalam360/jamlib/api/events/core/EventResult;",
                    &[
                        listener,
                        Value::Object(Some(player_id)),
                        main_hand.clone(),
                        Value::Object(Some(pos_id)),
                        Value::Object(Some(dir_id)),
                    ],
                );
                if let Ok(Value::Object(Some(res_id))) = res {
                    if let Some(event_res) = self.interp.heap.get_native::<EventResult>(res_id) {
                        if event_res.cancelled {
                            self.sync_level_to_host(level_id, host);
                            return Flow::Cancel;
                        }
                    }
                }
            }
        }

        Flow::Continue
    }

    /// Dispatches item tooltip gathering to Fabric `ItemTooltipCallback` listeners.
    pub fn item_tooltip(&mut self, item_id: &str, count: u32, lines: &mut Vec<(String, u32)>) {
        let tooltip_event = self.interp.resolve_static_field(
            "net/fabricmc/fabric/api/client/item/v1/ItemTooltipCallback",
            "EVENT",
        );
        if let Some(event_id) = tooltip_event.as_obj() {
            if let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get(event_id) {
                if let Some(Value::Object(Some(arr_id))) = fields.get("listeners") {
                    let arr_id = *arr_id;
                    let listeners = if let Some(JavaObject::Array { elements, .. }) =
                        self.interp.heap.get(arr_id)
                    {
                        elements.clone()
                    } else {
                        Vec::new()
                    };

                    if !listeners.is_empty() {
                        let stack_id = self
                            .interp
                            .heap
                            .alloc_item_stack(ItemStack::new(item_id, count as i32));
                        let stack_val = Value::Object(Some(stack_id));

                        let mut comp_vals = Vec::new();
                        for (text_str, col) in lines.iter() {
                            let str_id = self.interp.heap.alloc_string(text_str.clone());
                            let mut cf = HashMap::new();
                            cf.insert("text".into(), Value::Object(Some(str_id)));
                            cf.insert("color".into(), Value::Int(*col as i32));
                            let cid = self.interp.heap.alloc(JavaObject::Instance {
                                class_name: "net/minecraft/network/chat/MutableComponent".into(),
                                fields: cf,
                            });
                            comp_vals.push(Value::Object(Some(cid)));
                        }
                        let list_id = self.interp.heap.alloc_list(comp_vals);
                        let list_val = Value::Object(Some(list_id));

                        for listener in listeners {
                            let _ = self.interp.execute_method(
                                "net/fabricmc/fabric/api/client/item/v1/ItemTooltipCallback",
                                "getTooltip",
                                "(Lnet/minecraft/world/item/ItemStack;Lnet/minecraft/world/item/Item$TooltipContext;Lnet/minecraft/world/item/TooltipFlag;Ljava/util/List;)V",
                                &[listener, stack_val.clone(), Value::NULL, Value::NULL, list_val.clone()],
                            );
                        }

                        if let Some(JavaObject::List(items)) = self.interp.heap.get(list_id) {
                            let mut new_lines = Vec::new();
                            for item in items {
                                if let Some(cid) = item.as_obj() {
                                    let (line_str, col) = if let Some(s) =
                                        self.interp.heap.get_string(cid)
                                    {
                                        (s.to_string(), u32::MAX)
                                    } else if let Some(JavaObject::Instance { fields, .. }) =
                                        self.interp.heap.get(cid)
                                    {
                                        let s = fields
                                            .get("text")
                                            .and_then(|v| v.as_obj())
                                            .and_then(|sid| self.interp.heap.get_string(sid))
                                            .unwrap_or("")
                                            .to_string();
                                        let c = fields
                                            .get("color")
                                            .map(|v| v.as_int() as u32)
                                            .unwrap_or(u32::MAX);
                                        (s, c)
                                    } else {
                                        (String::new(), u32::MAX)
                                    };
                                    new_lines.push((line_str, col));
                                }
                            }
                            *lines = new_lines;
                        }
                    }
                }
            }
        }
    }

    /// The screen a mod installed through `Minecraft.gui.setScreen` (or
    /// `Minecraft.setScreen`), if any.
    pub fn current_screen(&self) -> Option<Value> {
        let mc = self
            .interp
            .heap
            .get_singleton("net/minecraft/client/Minecraft")?;
        let JavaObject::Instance { fields, .. } = self.interp.heap.get(mc)? else {
            return None;
        };
        fields
            .get("screen")
            .filter(|value| !value.is_null())
            .cloned()
    }

    pub fn has_screen(&self) -> bool {
        self.current_screen().is_some()
    }

    /// Run `Screen.init(width, height)` when the open screen or GUI size
    /// changed, then `ScreenEvents.AFTER_INIT` for the new screen.
    fn prepare_screen(&mut self, width: i32, height: i32) -> Result<Option<Value>> {
        let Some(screen) = self.current_screen() else {
            self.screen_id = None;
            return Ok(None);
        };
        let screen_id = screen.as_obj();
        if self.screen_id != screen_id || self.screen_width != width || self.screen_height != height
        {
            self.interp.execute_method(
                "net/minecraft/client/gui/screens/Screen",
                "init",
                "(II)V",
                &[screen.clone(), Value::Int(width), Value::Int(height)],
            )?;
            self.screen_id = screen_id;
            self.screen_width = width;
            self.screen_height = height;
            self.fire_screen_after_init(&screen, width, height);
        }
        Ok(Some(screen))
    }

    fn fire_screen_after_init(&mut self, screen: &Value, width: i32, height: i32) {
        let event = self.interp.resolve_static_field(
            "net/fabricmc/fabric/api/client/screen/v1/ScreenEvents",
            "AFTER_INIT",
        );
        let Some(event_id) = event.as_obj() else {
            return;
        };
        let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get(event_id) else {
            return;
        };
        let Some(Value::Object(Some(arr_id))) = fields.get("listeners") else {
            return;
        };
        let arr_id = *arr_id;
        let listeners =
            if let Some(JavaObject::Array { elements, .. }) = self.interp.heap.get(arr_id) {
                elements.clone()
            } else {
                return;
            };
        let mc = self
            .interp
            .heap
            .get_singleton("net/minecraft/client/Minecraft");
        for listener in listeners {
            let _ = self.interp.execute_method(
                "net/fabricmc/fabric/api/client/screen/v1/ScreenEvents$AfterInit",
                "afterInit",
                "(Lnet/minecraft/client/Minecraft;Lnet/minecraft/client/gui/screens/Screen;II)V",
                &[
                    listener,
                    mc.map(|id| Value::Object(Some(id))).unwrap_or(Value::NULL),
                    screen.clone(),
                    Value::Int(width),
                    Value::Int(height),
                ],
            );
        }
    }

    /// Replay the open screen's `GuiGraphicsExtractor` display list. Returns
    /// `None` when no mod screen is open.
    pub fn render_screen(
        &mut self,
        width: i32,
        height: i32,
        partial_tick: f32,
    ) -> Result<Option<Vec<GuiDraw>>> {
        let Some(screen) = self.prepare_screen(width, height)? else {
            return Ok(None);
        };
        let graphics = self
            .interp
            .heap
            .alloc_native(GuiGraphics::new(width, height));
        self.interp.execute_method(
            "net/minecraft/client/gui/screens/Screen",
            "extractRenderStateWithTooltipAndSubtitles",
            "(Lnet/minecraft/client/gui/GuiGraphicsExtractor;IIF)V",
            &[
                screen,
                Value::Object(Some(graphics)),
                Value::Int(0),
                Value::Int(0),
                Value::Float(partial_tick),
            ],
        )?;
        let draws = self
            .interp
            .heap
            .get_native::<GuiGraphics>(graphics)
            .map(|g| g.draws.clone())
            .unwrap_or_default();
        Ok(Some(draws))
    }

    /// `Screen.tick()`, plus Fabric's `ClientTickEvents.END_CLIENT_TICK`.
    pub fn client_tick(&mut self) {
        if let Some(screen) = self.current_screen() {
            let _ = self.interp.execute_method(
                "net/minecraft/client/gui/screens/Screen",
                "tick",
                "()V",
                &[screen],
            );
        }
        let event = self.interp.resolve_static_field(
            "net/fabricmc/fabric/api/client/event/lifecycle/v1/ClientTickEvents",
            "END_CLIENT_TICK",
        );
        let Some(event_id) = event.as_obj() else {
            return;
        };
        let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get(event_id) else {
            return;
        };
        let Some(Value::Object(Some(arr_id))) = fields.get("listeners") else {
            return;
        };
        let arr_id = *arr_id;
        let listeners =
            if let Some(JavaObject::Array { elements, .. }) = self.interp.heap.get(arr_id) {
                elements.clone()
            } else {
                return;
            };
        let mc = self
            .interp
            .heap
            .get_singleton("net/minecraft/client/Minecraft");
        for listener in listeners {
            let _ = self.interp.execute_method(
                "net/fabricmc/fabric/api/client/event/lifecycle/v1/ClientTickEvents$EndClientTick",
                "onEndTick",
                "(Lnet/minecraft/client/Minecraft;)V",
                &[
                    listener,
                    mc.map(|id| Value::Object(Some(id))).unwrap_or(Value::NULL),
                ],
            );
        }
    }

    /// Trigger a Fabric key mapping by its translation key (the host's binding
    /// action), then run the client tick that consumes it.
    pub fn press_key_mapping(&mut self, name: &str) -> bool {
        let Some(registry) = self.interp.heap.get_singleton("hfc:fabric_key_mappings") else {
            return false;
        };
        let Some(JavaObject::List(items)) = self.interp.heap.get(registry) else {
            return false;
        };
        let mappings = items.clone();
        let mut pressed = false;
        for mapping in mappings {
            let Some(id) = mapping.as_obj() else {
                continue;
            };
            let matches = self
                .interp
                .heap
                .get(id)
                .and_then(|obj| match obj {
                    JavaObject::Instance { fields, .. } => fields.get("name"),
                    _ => None,
                })
                .and_then(Value::as_obj)
                .and_then(|name_id| self.interp.heap.get_string(name_id))
                .is_some_and(|mapping_name| mapping_name == name);
            if matches {
                if let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get_mut(id) {
                    let count = fields.get("clickCount").map(|v| v.as_int()).unwrap_or(0);
                    fields.insert("clickCount".into(), Value::Int(count + 1));
                }
                pressed = true;
            }
        }
        if pressed {
            self.client_tick();
        }
        pressed
    }

    /// Press every Fabric mapping bound to a host key code (GLFW key codes).
    pub fn press_key_code(&mut self, key_code: i32) -> bool {
        let Some(registry) = self.interp.heap.get_singleton("hfc:fabric_key_mappings") else {
            return false;
        };
        let Some(JavaObject::List(items)) = self.interp.heap.get(registry) else {
            return false;
        };
        let mappings = items.clone();
        let mut pressed = false;
        for mapping in mappings {
            let Some(id) = mapping.as_obj() else {
                continue;
            };
            let bound = self
                .interp
                .heap
                .get(id)
                .and_then(|obj| match obj {
                    JavaObject::Instance { fields, .. } => fields.get("key"),
                    _ => None,
                })
                .map(|v| v.as_int())
                .unwrap_or(-1);
            if bound == key_code {
                if let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get_mut(id) {
                    let count = fields.get("clickCount").map(|v| v.as_int()).unwrap_or(0);
                    fields.insert("clickCount".into(), Value::Int(count + 1));
                }
                pressed = true;
            }
        }
        if pressed {
            self.client_tick();
        }
        pressed
    }

    pub fn mouse_moved(&mut self, x: f64, y: f64) {
        if let Some(screen) = self.current_screen() {
            let _ = self.interp.execute_method(
                "net/minecraft/client/gui/screens/Screen",
                "mouseMoved",
                "(DD)V",
                &[screen, Value::Double(x), Value::Double(y)],
            );
        }
    }

    pub fn mouse_button(&mut self, x: f64, y: f64, button: i32, pressed: bool) -> bool {
        let Some(screen) = self.current_screen() else {
            return false;
        };
        let info = self.interp.heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/client/input/MouseButtonInfo".into(),
            fields: [
                ("button".to_string(), Value::Int(button)),
                ("modifiers".to_string(), Value::Int(0)),
            ]
            .into(),
        });
        let event = self.interp.heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/client/input/MouseButtonEvent".into(),
            fields: [
                ("x".to_string(), Value::Double(x)),
                ("y".to_string(), Value::Double(y)),
                ("buttonInfo".to_string(), Value::Object(Some(info))),
            ]
            .into(),
        });
        let (method, descriptor, mut args) = if pressed {
            (
                "mouseClicked",
                "(Lnet/minecraft/client/input/MouseButtonEvent;Z)Z",
                vec![screen, Value::Object(Some(event)), Value::from_bool(false)],
            )
        } else {
            (
                "mouseReleased",
                "(Lnet/minecraft/client/input/MouseButtonEvent;)Z",
                vec![screen, Value::Object(Some(event))],
            )
        };
        args.truncate(if pressed { 3 } else { 2 });
        self.interp
            .execute_method(
                "net/minecraft/client/gui/screens/Screen",
                method,
                descriptor,
                &args,
            )
            .map(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub fn mouse_scrolled(&mut self, x: f64, y: f64, dx: f64, dy: f64) -> bool {
        let Some(screen) = self.current_screen() else {
            return false;
        };
        self.interp
            .execute_method(
                "net/minecraft/client/gui/screens/Screen",
                "mouseScrolled",
                "(DDDD)Z",
                &[
                    screen,
                    Value::Double(x),
                    Value::Double(y),
                    Value::Double(dx),
                    Value::Double(dy),
                ],
            )
            .map(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub fn key_event(&mut self, key: i32, scancode: i32, modifiers: i32, pressed: bool) -> bool {
        let Some(screen) = self.current_screen() else {
            return false;
        };
        let event = self.interp.heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/client/input/KeyEvent".into(),
            fields: [
                ("key".to_string(), Value::Int(key)),
                ("keycode".to_string(), Value::Int(scancode)),
                ("modifiers".to_string(), Value::Int(modifiers)),
            ]
            .into(),
        });
        let method = if pressed { "keyPressed" } else { "keyReleased" };
        self.interp
            .execute_method(
                "net/minecraft/client/gui/screens/Screen",
                method,
                "(Lnet/minecraft/client/input/KeyEvent;)Z",
                &[screen, Value::Object(Some(event))],
            )
            .map(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub fn char_event(&mut self, codepoint: i32) -> bool {
        let Some(screen) = self.current_screen() else {
            return false;
        };
        let event = self.interp.heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/client/input/CharacterEvent".into(),
            fields: [("codepoint".to_string(), Value::Int(codepoint))].into(),
        });
        self.interp
            .execute_method(
                "net/minecraft/client/gui/screens/Screen",
                "charTyped",
                "(Lnet/minecraft/client/input/CharacterEvent;)Z",
                &[screen, Value::Object(Some(event))],
            )
            .map(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// `Screen.onClose`, which usually installs the screen that was open
    /// before. The host leaves Fabric screen mode once `current_screen` is
    /// empty again.
    pub fn close_screen(&mut self) {
        if let Some(screen) = self.current_screen() {
            let _ = self.interp.execute_method(
                "net/minecraft/client/gui/screens/Screen",
                "onClose",
                "()V",
                &[screen],
            );
        }
        self.screen_id = None;
    }

    /// Dispatches tooltip color queries to Iceberg `RenderTooltipEvents.COLOREXT` listeners.
    pub fn item_tooltip_color(
        &mut self,
        item_id: Option<&str>,
        background: &mut u32,
        border_top: &mut u32,
        border_bottom: &mut u32,
    ) {
        let color_event = self.interp.resolve_static_field(
            "com/anthonyhilyard/iceberg/events/client/RenderTooltipEvents",
            "COLOREXT",
        );
        if let Some(event_id) = color_event.as_obj() {
            if let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get(event_id) {
                if let Some(Value::Object(Some(arr_id))) = fields.get("listeners") {
                    let arr_id = *arr_id;
                    let listeners = if let Some(JavaObject::Array { elements, .. }) =
                        self.interp.heap.get(arr_id)
                    {
                        elements.clone()
                    } else {
                        Vec::new()
                    };

                    if !listeners.is_empty() {
                        let stack_val = if let Some(id) = item_id {
                            let sid = self.interp.heap.alloc_item_stack(ItemStack::new(id, 1));
                            Value::Object(Some(sid))
                        } else {
                            Value::NULL
                        };

                        let bg_start = Value::Int(*background as i32);
                        let bg_end = Value::Int(*background as i32);
                        let border_start = Value::Int(*border_top as i32);
                        let border_end = Value::Int(*border_bottom as i32);

                        for listener in listeners {
                            let res = self.interp.execute_method(
                                "com/anthonyhilyard/iceberg/events/client/RenderTooltipEvents$ColorExt",
                                "onColor",
                                "(Lnet/minecraft/world/item/ItemStack;Lnet/minecraft/client/gui/GuiGraphicsExtractor;IILnet/minecraft/client/gui/Font;IIIILjava/util/List;ZILnet/minecraft/resources/Identifier;ZZ)Lcom/anthonyhilyard/iceberg/events/client/RenderTooltipEvents$ColorExtResult;",
                                &[
                                    listener,
                                    stack_val.clone(),
                                    Value::NULL,
                                    Value::Int(0),
                                    Value::Int(0),
                                    Value::NULL,
                                    bg_start.clone(),
                                    bg_end.clone(),
                                    border_start.clone(),
                                    border_end.clone(),
                                    Value::NULL,
                                    Value::from_bool(false),
                                    Value::Int(0),
                                    Value::NULL,
                                    Value::from_bool(false),
                                    Value::from_bool(false),
                                ],
                            );

                            if let Ok(Value::Object(Some(res_id))) = res {
                                if let Some(JavaObject::Instance { fields, .. }) =
                                    self.interp.heap.get(res_id)
                                {
                                    if let Some(bg) =
                                        fields.get("backgroundStart").map(|v| v.as_int())
                                    {
                                        *background = bg as u32;
                                    }
                                    if let Some(bt) = fields.get("borderStart").map(|v| v.as_int())
                                    {
                                        *border_top = bt as u32;
                                    }
                                    if let Some(bb) = fields.get("borderEnd").map(|v| v.as_int()) {
                                        *border_bottom = bb as u32;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
