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
            drag_button: None,
            last_mouse: (0.0, 0.0),
            mixins: MixinManifest::default(),
            menu_cache: HashMap::new(),
            menu_screen: None,
        }
    }

    pub fn loaded_mods(&self) -> &[FabricModMetadata] {
        &self.mods
    }

    /// Textures mods registered with `TextureManager.register`, keyed by
    /// identifier. The host decodes and draws them; the runtime has no GPU.
    pub fn registered_textures(&self) -> &HashMap<String, crate::minecraft::PngImage> {
        &self.interp.heap.textures
    }

    /// Read every loaded mod's mixin configs and the injectors their client
    /// mixin classes declare. Only the menu-widget `@Inject` subset is applied;
    /// everything else is recorded in [`MixinManifest::unsupported`].
    fn load_mixin_manifest(&mut self) -> MixinManifest {
        let mut manifest = MixinManifest::default();
        let mods = self.mods.clone();
        for meta in &mods {
            let Some(jar) = meta.jar.as_deref() else {
                continue;
            };
            for entry in mixin::config_entries(&meta.raw) {
                if entry.environment == "server" {
                    continue;
                }
                let Some(bytes) =
                    mixin::read_mod_resource(jar, meta.nested_jar.as_deref(), &entry.config)
                else {
                    manifest.unsupported.push(format!(
                        "{}: missing mixin config {}",
                        meta.id, entry.config
                    ));
                    continue;
                };
                let config = String::from_utf8_lossy(&bytes);
                for class_name in mixin::config_classes(&config) {
                    if self.interp.loader.get(&class_name).is_none() {
                        if let Err(error) = self.interp.loader.get_or_load(&class_name) {
                            manifest.unsupported.push(format!("{class_name}: {error}"));
                            continue;
                        }
                    }
                    let Some(class) = self.interp.loader.get(&class_name) else {
                        continue;
                    };
                    for injector in mixin::injectors_from_class(class) {
                        if injector.supported() {
                            manifest.injectors.push(injector);
                        } else {
                            manifest.unsupported.push(format!(
                                "{}.{}{} -> {}.{}: {:?}",
                                injector.mixin_class,
                                injector.callback_method,
                                injector.callback_descriptor,
                                injector.target_class,
                                injector.target_method,
                                injector.point
                            ));
                        }
                    }
                }
            }
        }
        manifest
    }

    /// Whether any applied injector targets this host menu.
    pub fn has_menu_injectors(&self, kind: MenuKind) -> bool {
        let class_name = kind.screen_class();
        let method_name = kind.layout_method();
        self.mixins.injectors.iter().any(|injector| {
            injector.supported()
                && injector.target_class == class_name
                && injector.target_method == method_name
        })
    }

    /// Run the host menu's real layout method with the mods' menu injectors
    /// active and return the widgets they added. Results are cached per GUI
    /// size; the host calls this whenever the menu opens or resizes.
    pub fn prepare_menu_contributions(
        &mut self,
        kind: MenuKind,
        width: i32,
        height: i32,
    ) -> Vec<MenuWidget> {
        let key = (kind, width, height);
        if !self.menu_cache.contains_key(&key) {
            let mut ids = self.run_menu_layout(kind, width, height);
            ids.retain(|id| self.is_menu_widget(*id));
            self.menu_cache.insert(key, ids);
        }
        self.menu_cache
            .get(&key)
            .into_iter()
            .flatten()
            .filter_map(|id| self.menu_widget(*id))
            .collect()
    }

    fn run_menu_layout(&mut self, kind: MenuKind, width: i32, height: i32) -> Vec<usize> {
        if !self.has_menu_injectors(kind) {
            return Vec::new();
        }
        let class_name = kind.screen_class();
        let method_name = kind.layout_method();
        let mut fields = HashMap::new();
        fields.insert("width".into(), Value::Int(width));
        fields.insert("height".into(), Value::Int(height));
        if let Some(minecraft) = self
            .interp
            .heap
            .get_singleton("net/minecraft/client/Minecraft")
        {
            fields.insert("minecraft".into(), Value::Object(Some(minecraft)));
        }
        let screen = self.interp.heap.alloc(JavaObject::Instance {
            class_name: class_name.into(),
            fields,
        });
        self.menu_screen = Some(screen);
        let injectors: Vec<crate::mixin::MixinInjector> = self
            .mixins
            .injectors
            .iter()
            .filter(|injector| {
                injector.supported()
                    && injector.target_class == class_name
                    && injector.target_method == method_name
            })
            .cloned()
            .collect();
        self.interp.menu_contributions.clear();
        self.interp.menu_injection = true;
        for injector in &injectors {
            self.run_menu_callback(injector, screen);
        }
        self.interp.menu_injection = false;
        std::mem::take(&mut self.interp.menu_contributions)
    }

    /// Call one menu mixin callback with a synthetic target-screen receiver,
    /// `CallbackInfo`, and synthesized layout parameters. The vanilla layout
    /// method is not run: the callback only builds and adds its widgets, and
    /// the host places them (see `MenuWidget`).
    fn run_menu_callback(&mut self, injector: &crate::mixin::MixinInjector, screen: usize) {
        if self.interp.loader.get(&injector.mixin_class).is_none() {
            let _ = self.interp.loader.get_or_load(&injector.mixin_class);
        }
        let is_static = self
            .interp
            .loader
            .get(&injector.mixin_class)
            .and_then(|class| {
                class.methods.get(&(
                    injector.callback_method.clone(),
                    injector.callback_descriptor.clone(),
                ))
            })
            .map(|method| method.is_static)
            .unwrap_or(false);
        let info = self
            .interp
            .heap
            .alloc_native(crate::minecraft::CallbackInfo::default());
        let types = crate::mixin::descriptor_parameter_types(&injector.callback_descriptor);
        let mut args = Vec::with_capacity(types.len() + 2);
        if !is_static {
            args.push(Value::Object(Some(screen)));
        }
        args.push(Value::Object(Some(info)));
        for parameter in types.iter().skip(1) {
            args.push(self.synthesize_menu_parameter(parameter, screen));
        }
        self.interp.injection_depth = 1;
        let result = self.interp.execute_method(
            &injector.mixin_class,
            &injector.callback_method,
            &injector.callback_descriptor,
            &args,
        );
        self.interp.injection_depth = 0;
        if let Err(error) = result {
            log::warn!(
                "Menu callback {}.{}{} failed: {error}",
                injector.mixin_class,
                injector.callback_method,
                injector.callback_descriptor
            );
        }
    }

    fn synthesize_menu_parameter(&mut self, descriptor: &str, screen: usize) -> Value {
        let name = descriptor
            .strip_prefix('L')
            .and_then(|name| name.strip_suffix(';'))
            .unwrap_or(descriptor);
        match descriptor {
            "I" | "Z" | "B" | "S" | "C" => Value::Int(0),
            "J" => Value::Long(0),
            "F" => Value::Float(0.0),
            "D" => Value::Double(0.0),
            _ if name.contains("mixinextras/sugar/ref/Local") => {
                // MixinExtras `@Local` sugar wrappers: give the callback a
                // mutable box it can read and write. The values are the host's
                // synthesized ones (zero/false), not the vanilla layout's
                // counters; the callback only needs to build its widget.
                let value = if name.ends_with("IntRef") {
                    Value::Int(0)
                } else if name.ends_with("LongRef") {
                    Value::Long(0)
                } else if name.ends_with("FloatRef") {
                    Value::Float(0.0)
                } else if name.ends_with("DoubleRef") {
                    Value::Double(0.0)
                } else if name.ends_with("BooleanRef") {
                    // A menu callback's boolean is usually its "add the
                    // widget" gate; false would suppress every contribution.
                    Value::Int(1)
                } else {
                    Value::NULL
                };
                let mut fields = HashMap::new();
                fields.insert("value".into(), value);
                let id = self.interp.heap.alloc(JavaObject::Instance {
                    class_name: name.to_string(),
                    fields,
                });
                Value::Object(Some(id))
            }
            _ if name.ends_with("Layout")
                || name.ends_with("RowHelper")
                || name.ends_with("GridLayout") =>
            {
                let id = self.interp.heap.alloc(JavaObject::Instance {
                    class_name: name.to_string(),
                    fields: HashMap::new(),
                });
                Value::Object(Some(id))
            }
            _ if name == "net/minecraft/client/gui/screens/Screen" || name.ends_with("/Screen") => {
                Value::Object(Some(screen))
            }
            _ if name == "net/minecraft/client/Minecraft" => self
                .interp
                .heap
                .get_singleton("net/minecraft/client/Minecraft")
                .map(|id| Value::Object(Some(id)))
                .unwrap_or(Value::NULL),
            _ => Value::NULL,
        }
    }

    /// True when `id` is (or extends) a widget with a render-state method.
    fn is_menu_widget(&mut self, id: usize) -> bool {
        let Some(class_name) = self
            .interp
            .heap
            .get(id)
            .map(|object| object.class_name().to_string())
        else {
            return false;
        };
        let mut queue = vec![class_name];
        let mut visited = std::collections::HashSet::new();
        while let Some(current) = queue.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if self.interp.loader.get(&current).is_none() {
                let _ = self.interp.loader.get_or_load(&current);
            }
            let Some(class) = self.interp.loader.get(&current) else {
                continue;
            };
            if class.methods.keys().any(|(name, descriptor)| {
                (name == "extractRenderState" || name == "extractWidgetRenderState")
                    && descriptor.starts_with("(Lnet/minecraft/client/gui/GuiGraphicsExtractor;")
            }) {
                return true;
            }
            if let Some(super_class) = &class.super_class {
                queue.push(super_class.clone());
            }
            queue.extend(class.interfaces.iter().cloned());
        }
        false
    }

    /// One contributed widget's public description.
    fn menu_widget(&self, id: usize) -> Option<MenuWidget> {
        let JavaObject::Instance { fields, .. } = self.interp.heap.get(id)? else {
            return None;
        };
        let get = |name: &str| fields.get(name).map(Value::as_int);
        let label = fields
            .get("message")
            .map(|message| crate::builtins::text_of_value(&self.interp.heap, message))
            .unwrap_or_default();
        Some(MenuWidget {
            id,
            label,
            x: get("x").unwrap_or(0),
            y: get("y").unwrap_or(0),
            width: get("width").unwrap_or(0),
            height: get("height").unwrap_or(0),
            visible: get("visible").map(|value| value != 0).unwrap_or(true),
            active: get("active").map(|value| value != 0).unwrap_or(true),
        })
    }

    /// The contributed widget under a GUI point for a prepared menu size.
    pub fn menu_widget_at(
        &self,
        kind: MenuKind,
        width: i32,
        height: i32,
        x: f64,
        y: f64,
    ) -> Option<usize> {
        let ids = self.menu_cache.get(&(kind, width, height))?;
        for (index, id) in ids.iter().enumerate() {
            let Some(widget) = self.menu_widget(*id) else {
                continue;
            };
            if widget.visible
                && widget.active
                && x >= f64::from(widget.x)
                && y >= f64::from(widget.y)
                && x < f64::from(widget.x + widget.width)
                && y < f64::from(widget.y + widget.height)
            {
                return Some(index);
            }
        }
        None
    }

    /// Run one contributed widget's real render extraction.
    pub fn menu_widget_draw(
        &mut self,
        kind: MenuKind,
        width: i32,
        height: i32,
        index: usize,
        mouse_x: i32,
        mouse_y: i32,
    ) -> Vec<GuiDraw> {
        let Some(id) = self
            .menu_cache
            .get(&(kind, width, height))
            .and_then(|ids| ids.get(index).copied())
        else {
            return Vec::new();
        };
        let graphics = self
            .interp
            .heap
            .alloc_native(GuiGraphics::new(width, height));
        let result = self.interp.execute_method(
            "net/minecraft/client/gui/components/AbstractWidget",
            "extractRenderState",
            "(Lnet/minecraft/client/gui/GuiGraphicsExtractor;IIF)V",
            &[
                Value::Object(Some(id)),
                Value::Object(Some(graphics)),
                Value::Int(mouse_x),
                Value::Int(mouse_y),
                Value::Float(0.0),
            ],
        );
        if let Err(error) = result {
            log::warn!("Menu widget render failed: {error}");
            return Vec::new();
        }
        self.interp
            .heap
            .get_native::<GuiGraphics>(graphics)
            .map(|graphics| graphics.draws.clone())
            .unwrap_or_default()
    }

    /// Click or release a contributed widget.
    pub fn menu_widget_click(
        &mut self,
        kind: MenuKind,
        width: i32,
        height: i32,
        index: usize,
        x: f64,
        y: f64,
        button: i32,
        pressed: bool,
    ) -> bool {
        let Some(id) = self
            .menu_cache
            .get(&(kind, width, height))
            .and_then(|ids| ids.get(index).copied())
        else {
            return false;
        };
        let event = self.mouse_event(x, y, button);
        let (method, descriptor, args) = if pressed {
            (
                "mouseClicked",
                "(Lnet/minecraft/client/input/MouseButtonEvent;Z)Z",
                vec![Value::Object(Some(id)), event, Value::from_bool(false)],
            )
        } else {
            (
                "mouseReleased",
                "(Lnet/minecraft/client/input/MouseButtonEvent;)Z",
                vec![Value::Object(Some(id)), event],
            )
        };
        self.interp
            .execute_method(
                "net/minecraft/client/gui/components/AbstractWidget",
                method,
                descriptor,
                &args,
            )
            .map(|value| value.as_bool())
            .unwrap_or(false)
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
            .filter(|value| value.as_obj() != self.menu_screen)
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

    /// The 26.3 `MouseButtonEvent` a screen dispatch carries.
    fn mouse_event(&mut self, x: f64, y: f64, button: i32) -> Value {
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
        Value::Object(Some(event))
    }

    pub fn mouse_moved(&mut self, x: f64, y: f64) {
        let (dx, dy) = (x - self.last_mouse.0, y - self.last_mouse.1);
        self.last_mouse = (x, y);
        let Some(screen) = self.current_screen() else {
            return;
        };
        if let Some(button) = self.drag_button {
            // `MouseHandler.onMove` with an active button dispatches
            // `mouseDragged`, not `mouseMoved`: a scrollbar thumb or slider
            // only follows the pointer through the drag, while the wheel
            // reaches `mouseScrolled`.
            let event = self.mouse_event(x, y, button);
            let _ = self.interp.execute_method(
                "net/minecraft/client/gui/screens/Screen",
                "mouseDragged",
                "(Lnet/minecraft/client/input/MouseButtonEvent;DD)Z",
                &[screen, event, Value::Double(dx), Value::Double(dy)],
            );
        } else {
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
        self.last_mouse = (x, y);
        let event = self.mouse_event(x, y, button);
        let (method, descriptor, mut args) = if pressed {
            (
                "mouseClicked",
                "(Lnet/minecraft/client/input/MouseButtonEvent;Z)Z",
                vec![screen, event, Value::from_bool(false)],
            )
        } else {
            (
                "mouseReleased",
                "(Lnet/minecraft/client/input/MouseButtonEvent;)Z",
                vec![screen, event],
            )
        };
        args.truncate(if pressed { 3 } else { 2 });
        let handled = self
            .interp
            .execute_method(
                "net/minecraft/client/gui/screens/Screen",
                method,
                descriptor,
                &args,
            )
            .map(|v| v.as_bool())
            .unwrap_or(false);
        if pressed {
            self.drag_button = Some(button);
        } else if self.drag_button == Some(button) {
            self.drag_button = None;
        }
        handled
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
        self.drag_button = None;
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
