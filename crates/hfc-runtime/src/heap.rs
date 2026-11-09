//! Heap memory storage for JVM objects.

use crate::value::{JavaObject, NativeObject, Value};
use std::collections::{BTreeMap, HashMap};

/// The heap storing all allocated Java objects.
#[derive(Default, Debug)]
pub struct Heap {
    objects: Vec<Option<JavaObject>>,
    interned_strings: HashMap<String, usize>,
    interned_classes: HashMap<String, usize>,
    pub loaded_mods: HashMap<String, crate::bridge::FabricModMetadata>,
    pub translations: HashMap<String, String>,
    /// Process-wide objects such as `Minecraft.getInstance()` and its font.
    /// A JVM keeps these in the class's static fields; the runtime keeps them
    /// here so builtins can share one without an interpreter borrow.
    singletons: HashMap<String, usize>,
    /// Textures mods registered with `TextureManager.register`, by identifier.
    /// The host draws them from the PNG bytes; the runtime never uploads.
    pub textures: HashMap<String, crate::minecraft::PngImage>,
    /// Sizes of the GUI textures the loaded mod jars ship, by identifier. A mod
    /// reading `TextureManager.getTexture(id).getTexture().getWidth()` gets the
    /// real size of the PNG in its jar, which is what Minecraft reports.
    pub gui_texture_sizes: HashMap<String, (u32, u32)>,
    /// What `java.nio.file` sees: the game directory on disk, or memory.
    pub fs: crate::vfs::Vfs,
    identifiers: HashMap<String, usize>,
    /// Block and item registries and tags, loaded by the host.
    pub registries: crate::registry::Registries,
    /// What mods asked of the host's desktop (open a link or a folder, copy
    /// text), taken by the host with `FabricBridge::take_host_requests`.
    pub host_requests: Vec<crate::bridge::HostRequest>,
    /// The text `ClipboardManager.getClipboard` answers: the last text a mod
    /// copied, or what the host put there.
    pub clipboard: String,
    /// The system properties `System.getProperty`/`System.getProperties` answer,
    /// with the values a JVM of this version reports. `user.dir` and
    /// `java.io.tmpdir` depend on the game directory and are filled in when
    /// they are asked for.
    pub properties: BTreeMap<String, String>,
    /// What mods registered in the runtime's built-in registries other than
    /// through the item/block paths (sound events, block entity types, menus,
    /// fluids, ...), in registration order.
    pub(crate) registry_entries: Vec<RegistryEntry>,
    /// The intrusive `Holder.Reference` made for an object
    /// (`createIntrusiveHolder`), by the object's heap id; registering the
    /// object binds its key.
    pub(crate) intrusive_holders: HashMap<usize, usize>,
    /// Items mods registered in `BuiltInRegistries.ITEM`, in registration order.
    pub(crate) registered_items: Vec<crate::item_builtins::RegisteredRecord>,
    /// Blocks mods registered in `BuiltInRegistries.BLOCK`, in registration order.
    pub(crate) registered_blocks: Vec<crate::block_builtins::BlockRecord>,
    /// `BlockItem`s mods registered: the item's id, its block object and the item object.
    pub(crate) registered_block_items: Vec<(String, usize, usize)>,
    /// What `collect_block_facts` read from the registered blocks.
    pub(crate) block_facts: Vec<crate::bridge::RegisteredBlock>,
    /// The ids of the registered blocks, for an O(1) membership test on the
    /// scheduled-tick path.
    pub(crate) block_ids: std::collections::HashSet<String>,
    /// Whether `block_facts` carries the blocks' outline and collision shapes:
    /// `read_block_shapes` read them for every block. Facts cached without
    /// shapes are still valid facts, but whoever reads them must ask for the
    /// shapes again rather than believe it has them.
    pub(crate) block_facts_shapes: bool,
    /// The `BlockState` objects of registered blocks, by block id and state key.
    pub(crate) block_state_objects: HashMap<String, HashMap<String, Value>>,
    /// One `ThreadLocal`'s value: the interpreter runs one thread, so a value
    /// per `ThreadLocal` object is exactly what a JVM's thread map holds.
    pub(crate) thread_locals: HashMap<usize, Value>,
    /// The field names `Unsafe.objectFieldOffset` handed out offsets for; the
    /// offset of a field is `UNSAFE_FIELD_BASE` plus its index here.
    pub(crate) unsafe_fields: Vec<String>,
    /// The `Supplier` each `ThreadLocal.withInitial` carries.
    pub(crate) thread_local_initializers: HashMap<usize, Value>,
    /// `Item.Properties` methods the runtime does not model, each logged once.
    pub unsupported_item_properties: Vec<String>,
    /// Creative-tab placements the tab events' listeners made, in call order.
    pub(crate) creative_tab_additions: Vec<crate::bridge::items::CreativeTabAddition>,
    /// Tabs mods registered in code, whose generators run after the entrypoints.
    pub(crate) registered_creative_tabs: Vec<crate::creative_tab_builtins::CreativeTabRecord>,
    /// Those tabs' contents, as the host content loader reads them.
    pub(crate) creative_tab_exports: Vec<crate::creative_tab_builtins::ExportedCreativeTab>,
    /// Creative-tab output calls the runtime does not model, each logged once.
    pub unsupported_creative_tab_calls: Vec<String>,
    /// The object count when the current block-code call opened its scope.
    /// Ids from here on are freed when it ends, unless something kept one.
    scope: Option<usize>,
    /// A scoped object was stored where the final reference scan cannot see
    /// it (a class's static fields, the interpreter's caches).
    escaped: bool,
}

/// One `WritableRegistry.register(key, value, info)` into a built-in registry.
#[derive(Debug, Clone)]
pub(crate) struct RegistryEntry {
    /// The registry's token (`SOUND_EVENT`), as `Registries.SOUND_EVENT` is.
    pub kind: String,
    /// `namespace:path`.
    pub id: String,
    pub value: Value,
    /// The `Holder.Reference` `register` answered.
    pub holder: usize,
}

/// The system properties that do not depend on where the game folder is.
fn default_properties() -> BTreeMap<String, String> {
    let mut properties = BTreeMap::new();
    let os_version = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|text| text.trim().to_string())
        .unwrap_or_default();
    for (key, value) in [
        ("java.version", "25"),
        ("java.specification.version", "25"),
        ("java.class.version", "69.0"),
        ("java.vendor", "Horizon Fabric Runtime"),
        ("java.vm.name", "Horizon Fabric Runtime"),
        ("java.vm.vendor", "Horizon Fabric Runtime"),
        ("java.vm.version", "25"),
        ("os.name", "Linux"),
        // The kernel release, as the JVM's `os.version` reports it.
        ("os.version", os_version.as_str()),
        ("os.arch", std::env::consts::ARCH),
        ("file.separator", "/"),
        ("path.separator", ":"),
        ("line.separator", "\n"),
        ("file.encoding", "UTF-8"),
        ("sun.jnu.encoding", "UTF-8"),
        ("native.encoding", "UTF-8"),
        // The interpreter has no `sun.misc.Unsafe`, so JOML must take its
        // plain-field matrix copies rather than raw memory ones.
        ("joml.nounsafe", "true"),
        // JOML formats floats through `DecimalFormat`, which needs the JDK's
        // CLDR locale bundles; the runtime reads classes from the JDK image,
        // not its resource bundles. JOML's own switch turns that formatting off
        // and prints the plain number, as `-Djoml.format=false` does on a JVM.
        ("joml.format", "false"),
    ] {
        properties.insert(key.to_string(), value.to_string());
    }
    if let Ok(home) = std::env::var("HOME") {
        properties.insert("user.home".to_string(), home);
    }
    if let Ok(name) = std::env::var("USER") {
        properties.insert("user.name".to_string(), name);
    }
    properties
}

impl Heap {
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
            interned_strings: HashMap::new(),
            interned_classes: HashMap::new(),
            loaded_mods: HashMap::new(),
            translations: HashMap::new(),
            singletons: HashMap::new(),
            textures: HashMap::new(),
            gui_texture_sizes: HashMap::new(),
            fs: crate::vfs::Vfs::default(),
            properties: default_properties(),
            identifiers: HashMap::new(),
            registries: crate::registry::Registries::default(),
            host_requests: Vec::new(),
            clipboard: String::new(),
            registered_items: Vec::new(),
            registered_blocks: Vec::new(),
            registered_block_items: Vec::new(),
            registry_entries: Vec::new(),
            intrusive_holders: HashMap::new(),
            block_facts: Vec::new(),
            block_ids: std::collections::HashSet::new(),
            block_facts_shapes: false,
            block_state_objects: HashMap::new(),
            thread_locals: HashMap::new(),
            unsafe_fields: Vec::new(),
            thread_local_initializers: HashMap::new(),
            unsupported_item_properties: Vec::new(),
            creative_tab_additions: Vec::new(),
            registered_creative_tabs: Vec::new(),
            creative_tab_exports: Vec::new(),
            unsupported_creative_tab_calls: Vec::new(),
            scope: None,
            escaped: false,
        }
    }

    pub fn get_singleton(&self, key: &str) -> Option<usize> {
        self.singletons.get(key).copied()
    }

    pub fn set_singleton(&mut self, key: &str, id: usize) {
        self.singletons.insert(key.to_string(), id);
    }

    /// Add a collection of translations to the heap.
    pub fn add_translations(&mut self, entries: impl IntoIterator<Item = (String, String)>) {
        self.translations.extend(entries);
    }

    /// Get a translation by key.
    pub fn get_translation(&self, key: &str) -> Option<&str> {
        self.translations.get(key).map(|s| s.as_str())
    }

    /// Check if a translation key exists.
    pub fn has_translation(&self, key: &str) -> bool {
        self.translations.contains_key(key)
    }

    /// Register a loaded Fabric mod's metadata.
    pub fn register_mod(&mut self, meta: crate::bridge::FabricModMetadata) {
        self.loaded_mods.insert(meta.id.clone(), meta);
    }

    /// Check if a mod with the given ID is loaded.
    pub fn is_mod_loaded(&self, id: &str) -> bool {
        self.loaded_mods.contains_key(id)
            || matches!(id, "fabric" | "fabricloader" | "minecraft" | "java")
    }

    /// Look up a mod's metadata by ID.
    pub fn get_mod(&self, id: &str) -> Option<&crate::bridge::FabricModMetadata> {
        self.loaded_mods.get(id)
    }

    /// Number of heap slots, allocated or not; tests use it to see growth.
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Open a scope at the current heap end: everything allocated from now
    /// until [`Heap::end_scope`] may be freed if nothing kept a reference.
    pub fn begin_scope(&mut self) {
        self.scope = Some(self.objects.len());
        self.escaped = false;
    }

    /// The open scope's first id, when one is open.
    pub fn scope_mark(&self) -> Option<usize> {
        self.scope
    }

    /// Something outside the heap's own reference scan kept a scoped object.
    pub fn note_escape(&mut self) {
        if self.scope.is_some() {
            self.escaped = true;
        }
    }

    /// Whether something a scan cannot see marked the scope as escaped.
    pub fn scope_escaped(&self) -> bool {
        self.escaped
    }

    /// Close the open scope, truncating the objects allocated inside it unless
    /// `escaped` (or an earlier [`Heap::note_escape`]) says something kept
    /// one. Returns whether it truncated.
    pub fn end_scope(&mut self, escaped: bool) -> bool {
        let Some(mark) = self.scope.take() else {
            return false;
        };
        let escaped = escaped || self.escaped;
        self.escaped = false;
        if !escaped {
            self.objects.truncate(mark);
        }
        !escaped
    }

    /// Whether an id `>= mark` is reachable from what the heap keeps between
    /// calls (interned ids, singletons, registrations, the block state cache)
    /// or from any object older than `mark`.
    pub fn refs_into(&self, mark: usize) -> bool {
        let value = |value: &Value| value.as_obj().is_some_and(|id| id >= mark);
        if self.interned_strings.values().any(|id| *id >= mark)
            || self.interned_classes.values().any(|id| *id >= mark)
            || self.singletons.values().any(|id| *id >= mark)
            || self.identifiers.values().any(|id| *id >= mark)
            || self
                .registered_blocks
                .iter()
                .any(|block| block.object >= mark)
            || self.registered_items.iter().any(|item| item.object >= mark)
            || self
                .registered_block_items
                .iter()
                .any(|(_, block, item)| *block >= mark || *item >= mark)
            || self
                .block_state_objects
                .values()
                .any(|states| states.values().any(value))
        {
            return true;
        }
        self.objects[..mark.min(self.objects.len())]
            .iter()
            .flatten()
            .any(|object| object_refs_into(object, mark))
    }

    /// Allocate a new object and return its ID.
    pub fn alloc(&mut self, obj: JavaObject) -> usize {
        let id = self.objects.len();
        self.objects.push(Some(obj));
        id
    }

    /// Allocate a native object.
    pub fn alloc_native<T: NativeObject + 'static>(&mut self, obj: T) -> usize {
        self.alloc(JavaObject::Native(Box::new(obj)))
    }

    /// The one `Identifier` object for `namespace:path`. Identifiers are values:
    /// two with the same text must compare equal in a Set or Map, which the
    /// runtime's collections decide by object identity.
    pub fn alloc_identifier(&mut self, namespace: &str, path: &str) -> usize {
        let key = format!("{namespace}:{path}");
        if let Some(id) = self.identifiers.get(&key) {
            return *id;
        }
        let mut fields = HashMap::new();
        let ns = self.alloc_string(namespace);
        let p = self.alloc_string(path);
        fields.insert("namespace".to_string(), Value::Object(Some(ns)));
        fields.insert("path".to_string(), Value::Object(Some(p)));
        let id = self.alloc(JavaObject::Instance {
            class_name: "net/minecraft/resources/Identifier".into(),
            fields,
        });
        self.identifiers.insert(key, id);
        id
    }

    /// Intern a string and return its heap ID.
    pub fn alloc_string(&mut self, s: impl Into<String>) -> usize {
        let s = s.into();
        if let Some(&id) = self.interned_strings.get(&s) {
            return id;
        }
        let id = self.alloc(JavaObject::String(s.clone()));
        self.interned_strings.insert(s, id);
        id
    }

    /// Intern a class and return its heap ID.
    pub fn alloc_class(&mut self, s: impl Into<String>) -> usize {
        let s = s.into().replace('.', "/");
        if let Some(&id) = self.interned_classes.get(&s) {
            return id;
        }
        let id = self.alloc(JavaObject::Class(s.clone()));
        self.interned_classes.insert(s, id);
        id
    }

    /// Get an object reference by ID.
    pub fn get(&self, id: usize) -> Option<&JavaObject> {
        self.objects.get(id).and_then(Option::as_ref)
    }

    /// Get a mutable object reference by ID.
    pub fn get_mut(&mut self, id: usize) -> Option<&mut JavaObject> {
        self.objects.get_mut(id).and_then(Option::as_mut)
    }

    /// Get a string value by ID.
    pub fn get_string(&self, id: usize) -> Option<&str> {
        match self.get(id)? {
            JavaObject::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Downcast a native object reference.
    pub fn get_native<T: 'static>(&self, id: usize) -> Option<&T> {
        match self.get(id)? {
            JavaObject::Native(n) => n.as_any().downcast_ref::<T>(),
            _ => None,
        }
    }

    /// Mutably downcast a native object reference.
    pub fn get_native_mut<T: 'static>(&mut self, id: usize) -> Option<&mut T> {
        match self.get_mut(id)? {
            JavaObject::Native(n) => n.as_any_mut().downcast_mut::<T>(),
            _ => None,
        }
    }

    /// Convenience for creating a Value::Object.
    pub fn obj_val(&mut self, obj: JavaObject) -> Value {
        Value::Object(Some(self.alloc(obj)))
    }

    /// Allocate a Java List object.
    pub fn alloc_list(&mut self, items: Vec<Value>) -> usize {
        self.alloc(JavaObject::List(items))
    }

    /// Allocate a Java Map object.
    pub fn alloc_map(&mut self, entries: Vec<(Value, Value)>) -> usize {
        self.alloc(JavaObject::Map(entries))
    }

    /// Allocate a Java Set object.
    pub fn alloc_set(&mut self, items: Vec<Value>) -> usize {
        self.alloc(JavaObject::Set(items))
    }

    /// Allocate a Java Stream object.
    pub fn alloc_stream(&mut self, items: Vec<Value>) -> usize {
        self.alloc(JavaObject::Stream(items))
    }

    /// Allocate a Java Iterator object.
    pub fn alloc_iterator(&mut self, items: Vec<Value>) -> usize {
        self.alloc(JavaObject::Iterator {
            items,
            cursor: 0,
            source: None,
        })
    }
}

/// Whether `object` refers to a heap id `>= mark`.
fn object_refs_into(object: &JavaObject, mark: usize) -> bool {
    let value = |value: &Value| value.as_obj().is_some_and(|id| id >= mark);
    match object {
        JavaObject::Instance { fields, .. } => fields.values().any(value),
        JavaObject::Array { elements, .. } => elements.iter().any(value),
        JavaObject::List(items) | JavaObject::Set(items) | JavaObject::Stream(items) => {
            items.iter().any(value)
        }
        JavaObject::Map(entries) => entries
            .iter()
            .any(|(key, value_)| value(key) || value(value_)),
        JavaObject::Iterator { items, .. } => items.iter().any(value),
        JavaObject::Lambda { captured, .. } => captured.iter().any(value),
        JavaObject::Native(native) => native_refs_into(native.as_any(), mark),
        JavaObject::String(_) | JavaObject::Class(_) => false,
    }
}

/// The heap ids the runtime's own native objects hold.
fn native_refs_into(object: &dyn std::any::Any, mark: usize) -> bool {
    use crate::minecraft::{BlockHitResult, EventResult, GuiGraphics, Level, Player};
    use crate::place_builtins::PlaceContext;
    let value = |value: &Value| value.as_obj().is_some_and(|id| id >= mark);
    if let Some(level) = object.downcast_ref::<Level>() {
        return level.mod_states.values().any(|id| *id >= mark)
            || level.placed.iter().any(|(_, id)| *id >= mark)
            || level.scheduled.iter().any(|(_, id, _)| *id >= mark);
    }
    if let Some(player) = object.downcast_ref::<Player>() {
        return player.level_id >= mark
            || player.uuid.is_some_and(|id| id >= mark)
            || player.hand_stack.is_some_and(|id| id >= mark)
            || player.hand_replaced.flatten().is_some_and(|id| id >= mark);
    }
    if let Some(context) = object.downcast_ref::<PlaceContext>() {
        return [
            &context.level,
            &context.player,
            &context.stack,
            &context.pos,
            &context.hit_pos,
            &context.face,
            &context.click_location,
            &context.hit_result,
            &context.horizontal,
            &context.vertical,
        ]
        .into_iter()
        .any(value)
            || context.nearest.iter().any(value);
    }
    if let Some(hit) = object.downcast_ref::<BlockHitResult>() {
        return hit.location.is_some_and(|id| id >= mark);
    }
    if let Some(result) = object.downcast_ref::<EventResult>() {
        return value(&result.value);
    }
    if let Some(graphics) = object.downcast_ref::<GuiGraphics>() {
        return graphics.pose.is_some_and(|id| id >= mark);
    }
    false
}

#[cfg(test)]
mod scope_tests {
    use super::{Heap, JavaObject};
    use std::collections::HashMap;

    fn instance() -> JavaObject {
        JavaObject::Instance {
            class_name: "example.Temp".into(),
            fields: HashMap::new(),
        }
    }

    #[test]
    fn a_scope_frees_what_nothing_kept() {
        let mut heap = Heap::new();
        let before = heap.object_count();
        heap.begin_scope();
        let mark = heap.scope_mark().unwrap();
        assert_eq!(mark, before);
        heap.alloc(instance());
        heap.alloc(JavaObject::String("temp".into()));
        assert_eq!(heap.object_count(), mark + 2);
        assert!(!heap.refs_into(mark));
        assert!(heap.end_scope(false));
        assert_eq!(heap.object_count(), mark);
    }

    #[test]
    fn a_scoped_object_stored_in_an_older_one_keeps_the_scope() {
        let mut heap = Heap::new();
        let old = heap.alloc(instance());
        heap.begin_scope();
        let mark = heap.scope_mark().unwrap();
        let fresh = heap.alloc(instance());
        // A write the runtime's older object would keep after the call.
        if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(old) {
            fields.insert("kept".into(), super::Value::Object(Some(fresh)));
        }
        assert!(heap.refs_into(mark));
        assert!(!heap.end_scope(true));
        assert_eq!(heap.object_count(), mark + 1);
    }

    #[test]
    fn an_interned_id_kept_by_the_heap_keeps_the_scope() {
        let mut heap = Heap::new();
        heap.begin_scope();
        let mark = heap.scope_mark().unwrap();
        heap.alloc_string("fresh string");
        // The intern table hands the id out later, so it must survive.
        assert!(heap.refs_into(mark));
        assert!(!heap.end_scope(true));
    }
}
