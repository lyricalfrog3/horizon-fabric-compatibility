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
    /// The player's own inventory as the host last reported it, slot by slot
    /// (`(item, count)`, `None` for an empty slot). `Inventory.getItem` reads
    /// it: a screen's player slots are built from an `Inventory` the runtime
    /// allocates itself, so they must not depend on one carrying a player.
    pub inventory: Vec<Option<(String, i32)>>,
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
    /// The block entities standing in the host's world, by position and block
    /// id: the heap id of the mod's own `BlockEntity` object, made through the
    /// block's `newBlockEntity` the first time it is asked for. They outlive a
    /// call: a pot keeps its contents between right clicks.
    pub(crate) block_entities: HashMap<([i32; 3], String), usize>,
    /// Blocks whose `newBlockEntity` threw or answered nothing, logged once
    /// each so a clicked block does not flood the log.
    pub(crate) block_entity_failures: std::collections::HashSet<String>,
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
    /// Objects older than the open scope that were written since it opened
    /// ([`Heap::note_write`]). An id `>= mark` did not exist when the scope
    /// opened, so an older object can only hold one if this scope wrote it
    /// there: scanning the written objects is the same question as scanning
    /// the whole heap, without walking every object the game ever allocated.
    dirty: Vec<usize>,
    /// The scope generation each object was last written in, parallel to
    /// `objects`. A write notes the id only once per scope, so the barrier is
    /// an array store rather than a hash lookup.
    write_stamp: Vec<u32>,
    /// The generation [`Heap::begin_scope`] gave the open scope.
    write_epoch: u32,
    /// How many objects the last [`Heap::refs_into`] examined, for tests that
    /// pin the scan to the writes rather than to the heap size.
    scanned: std::cell::Cell<usize>,
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
            inventory: Vec::new(),
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
            block_entities: HashMap::new(),
            block_entity_failures: std::collections::HashSet::new(),
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
            dirty: Vec::new(),
            write_stamp: Vec::new(),
            write_epoch: 1,
            scanned: std::cell::Cell::new(0),
        }
    }

    /// The heap id of a registered block's own object, for tests and the
    /// bridge that check what the mod's code sees.
    #[doc(hidden)]
    pub fn registered_block_object(&self, id: &str) -> Option<usize> {
        self.registered_blocks
            .iter()
            .find(|b| b.id == id)
            .map(|b| b.object)
    }

    /// The `BlockState` object of a registered block's state, for tests.
    #[doc(hidden)]
    pub fn state_object(&self, id: &str, key: &str) -> Option<Value> {
        self.block_state_objects.get(id)?.get(key).cloned()
    }

    /// The block entities kept between calls, `(position, block id)` to the
    /// object's heap id, for tests.
    #[doc(hidden)]
    pub fn kept_block_entities(&self) -> &HashMap<([i32; 3], String), usize> {
        &self.block_entities
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
        self.dirty.clear();
        // A fresh generation invalidates every older write's stamp, so the
        // clears that `end_scope` would otherwise need a walk for.
        self.write_epoch = self.write_epoch.wrapping_add(1);
        if self.write_epoch == 0 {
            // The generation counter wrapped: every stamp is stale, so forget
            // them rather than mistake an old write for this scope's.
            self.write_stamp.fill(0);
            self.write_epoch = 1;
        }
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

    /// The number of objects the last [`Heap::refs_into`] examined: the writes
    /// since the scope opened, not the whole heap. A test pins it so a
    /// regression back to the full scan is visible.
    #[doc(hidden)]
    pub fn refs_scanned(&self) -> usize {
        self.scanned.get()
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
            self.write_stamp.truncate(mark);
        }
        self.dirty.clear();
        !escaped
    }

    /// Whether an id `>= mark` is reachable from what the heap keeps between
    /// calls (interned ids, singletons, registrations, the block state cache)
    /// or from an object older than `mark`.
    ///
    /// An id `>= mark` was allocated after the scope opened, so the only way
    /// an older object can hold one is a write this scope made into it. The
    /// scan therefore walks the objects written since the scope opened (the
    /// write barrier in [`Heap::get_mut`]) instead of every object on the
    /// heap: with a large modpack the full walk cost milliseconds per tick.
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
            || self.block_entities.values().any(|id| *id >= mark)
            // A `ThreadLocal`'s value outlives the call that set it: a
            // recursion guard (`ThreadLocal<Deque>` holding the call's own
            // frames) is the common case, and truncating its contents leaves
            // the id pointing at whatever the next scope allocates there —
            // type confusion rather than a missed value. Both the values and
            // the `withInitial` suppliers are roots.
            || self.thread_locals.values().any(value)
            || self.thread_local_initializers.values().any(value)
            // A mod's `Registry.register` value and the `Holder.Reference`
            // made for it are read back long after the call: a registry that
            // lost its contents would answer a stale id for the same reason.
            || self
                .registry_entries
                .iter()
                .any(|entry| value(&entry.value) || entry.holder >= mark)
            || self
                .intrusive_holders
                .iter()
                .any(|(object, holder)| *object >= mark || *holder >= mark)
        {
            return true;
        }
        // A write into an object allocated after `mark` needs no scan: the
        // whole object is freed with the scope unless the checks above keep it.
        let mut scanned = 0;
        let found = self.dirty.iter().filter(|id| **id < mark).any(|id| {
            scanned += 1;
            self.objects
                .get(*id)
                .and_then(Option::as_ref)
                .is_some_and(|object| object_refs_into(object, mark))
        });
        self.scanned.set(scanned);
        found
    }

    /// Allocate a new object and return its ID.
    pub fn alloc(&mut self, obj: JavaObject) -> usize {
        let id = self.objects.len();
        self.objects.push(Some(obj));
        // Keep the write-stamp array parallel to `objects`: a slot allocated
        // in the open scope is freed with it, so it only needs a neutral stamp.
        self.write_stamp.push(0);
        id
    }

    /// Allocate a native object.
    pub fn alloc_native<T: NativeObject + 'static>(&mut self, obj: T) -> usize {
        self.alloc(JavaObject::Native(Box::new(obj)))
    }

    /// The one block-state `Property` object for `name` and its value range.
    /// Properties are values vanilla compares by identity: a state built with
    /// `BlockStateProperties.AGE_7` and a `setValue` call naming a property of
    /// the same name must see the same object, or `StateHolder.valueIndex`
    /// answers -1 and the state rejects the write.
    pub fn alloc_property(&mut self, property: crate::minecraft::IntegerProperty) -> usize {
        let key = format!(
            "property:{}:{}:{}",
            property.name, property.min, property.max
        );
        if let Some(id) = self.identifiers.get(&key) {
            return *id;
        }
        let id = self.alloc_native(property);
        self.identifiers.insert(key, id);
        id
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

    /// Allocate a string that is **not** interned: a fresh object with a new
    /// heap id on every call.
    ///
    /// A JVM interns only string literals (and `String.intern()`); every other
    /// `String` is a distinct object, so `u + "" == u` is false even though the
    /// text is equal. `alloc_string` interns by text, which is right for a
    /// constant-pool literal but wrong for a string a program computes.
    /// `StringBuilder.toString()`, a `new String(...)` and the `invokedynamic`
    /// string-concatenation result all go through here, so identity behaves as
    /// Java's does. Callers that already know they want the interned literal
    /// must keep using [`Heap::alloc_string`].
    pub fn alloc_string_uninterned(&mut self, s: impl Into<String>) -> usize {
        self.alloc(JavaObject::String(s.into()))
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
    ///
    /// This is the heap's write barrier: a mutable borrow is the only way to
    /// store a scoped object into an older one, so the id is noted and
    /// [`Heap::refs_into`] scans it when the scope closes.
    pub fn get_mut(&mut self, id: usize) -> Option<&mut JavaObject> {
        self.note_write(id);
        self.objects.get_mut(id).and_then(Option::as_mut)
    }

    /// Note that `id` may have been written, so the reference scan examines it
    /// when the open scope closes. Reads go through [`Heap::get`] and are not
    /// noted. An id written twice in one scope is noted once: the stamp is a
    /// generation number, not a flag that needs clearing.
    pub fn note_write(&mut self, id: usize) {
        let Some(mark) = self.scope else {
            return;
        };
        // An object allocated inside the scope goes away with it unless
        // something outside keeps it, and reaching such an object from outside
        // means passing through an older one this scan would see. So only
        // writes into older objects can change the answer.
        if id >= mark {
            return;
        }
        if self.write_stamp.len() <= id {
            self.write_stamp.resize(self.objects.len(), 0);
        }
        if self.write_stamp[id] == self.write_epoch {
            return;
        }
        self.write_stamp[id] = self.write_epoch;
        self.dirty.push(id);
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
        JavaObject::Native(native) => native_refs_into(native.as_ref(), mark),
        JavaObject::String(_) | JavaObject::Class(_) => false,
    }
}

/// Whether a native object holds a heap id `>= mark`.
///
/// The ids come from [`NativeObject::held_ids`], which every implementor whose
/// fields can hold a [`Value`], a heap id or a collection of either overrides.
/// Keeping this generic rather than down-casting a fixed list of types is what
/// stops a newly added native from silently escaping the scan: before the
/// trait method, only six types were covered and a callback info holding a
/// `return_value`, a place context holding eleven `Value`s and a `Level`
/// holding placed states were all invisible, so a scoped value they kept could
/// be truncated and its id reused for an unrelated object.
fn native_refs_into(object: &dyn NativeObject, mark: usize) -> bool {
    object.held_ids().into_iter().any(|id| id >= mark)
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

    /// The scan is bounded by what the scope wrote, not by the heap: a heap of
    /// many untouched objects is as cheap to close as a small one. This is the
    /// regression guard for the full-heap walk that cost ~8 ms per block
    /// entity tick in a modpack.
    #[test]
    fn the_scan_examines_the_writes_not_the_whole_heap() {
        let mut heap = Heap::new();
        for _ in 0..10_000 {
            heap.alloc(instance());
        }
        heap.begin_scope();
        let mark = heap.scope_mark().unwrap();
        heap.alloc(instance());
        assert!(!heap.refs_into(mark));
        assert_eq!(
            heap.refs_scanned(),
            0,
            "the scan looked at {} untouched objects",
            heap.refs_scanned()
        );

        // One write into an old object is examined and does reach the scope.
        let fresh = heap.alloc(instance());
        if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(7) {
            fields.insert("kept".into(), super::Value::Object(Some(fresh)));
        }
        assert!(heap.refs_into(mark));
        assert_eq!(
            heap.refs_scanned(),
            1,
            "the scan examined {} objects, not just the write",
            heap.refs_scanned()
        );
        assert!(!heap.end_scope(true));
    }

    /// A write into an object allocated inside the scope is not scanned: the
    /// object is freed with the scope anyway, and reaching it from outside
    /// would mean passing through an older object the scan does see.
    #[test]
    fn a_write_into_a_scoped_object_needs_no_scan() {
        let mut heap = Heap::new();
        let old = heap.alloc(instance());
        heap.begin_scope();
        let mark = heap.scope_mark().unwrap();
        let fresh = heap.alloc(instance());
        let also = heap.alloc(instance());
        if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(fresh) {
            fields.insert("kept".into(), super::Value::Object(Some(also)));
        }
        assert!(!heap.refs_into(mark));
        assert_eq!(heap.refs_scanned(), 0);
        assert!(heap.end_scope(false));
        assert_eq!(heap.object_count(), old + 1);
    }

    /// The write barrier only applies while a scope is open: outside one there
    /// is nothing to scan and nothing to remember.
    #[test]
    fn a_write_outside_a_scope_is_not_recorded() {
        let mut heap = Heap::new();
        let old = heap.alloc(instance());
        let fresh = heap.alloc(instance());
        if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(old) {
            fields.insert("kept".into(), super::Value::Object(Some(fresh)));
        }
        assert!(heap.dirty.is_empty());
        // Nor does a later scope inherit it: the write predates the scope, so
        // no id it holds was allocated inside it.
        heap.begin_scope();
        let mark = heap.scope_mark().unwrap();
        heap.alloc(instance());
        assert!(!heap.refs_into(mark));
        assert_eq!(heap.refs_scanned(), 0);
    }

    /// A `ThreadLocal` value is a heap root: a recursion guard's deque lives
    /// past the call that pushed onto it, and truncating it would leave the
    /// id pointing at whatever the next scope allocates. This is the
    /// regression guard for the four containers that used to be invisible to
    /// the scan.
    #[test]
    fn a_thread_local_value_keeps_the_scope() {
        let mut heap = Heap::new();
        let guard = heap.alloc(instance());
        heap.begin_scope();
        let mark = heap.scope_mark().unwrap();
        let value = heap.alloc(instance());
        heap.thread_locals
            .insert(guard, super::Value::Object(Some(value)));
        assert!(heap.refs_into(mark));
        assert!(!heap.end_scope(true));
        assert_eq!(
            heap.thread_locals.get(&guard),
            Some(&super::Value::Object(Some(value)))
        );
    }

    /// A `ThreadLocal.withInitial` supplier is a root too: it runs later, when
    /// its captured values must still be there.
    #[test]
    fn a_thread_local_initializer_keeps_the_scope() {
        let mut heap = Heap::new();
        let guard = heap.alloc(instance());
        heap.begin_scope();
        let mark = heap.scope_mark().unwrap();
        let supplier = heap.alloc(instance());
        heap.thread_local_initializers
            .insert(guard, super::Value::Object(Some(supplier)));
        assert!(heap.refs_into(mark));
        assert!(!heap.end_scope(true));
    }

    /// A registry entry's value and holder outlive the registering call: a
    /// mod reads the registry back after `init`.
    #[test]
    fn a_registry_entry_keeps_the_scope() {
        let mut heap = Heap::new();
        heap.begin_scope();
        let mark = heap.scope_mark().unwrap();
        let value = heap.alloc(instance());
        let holder = heap.alloc(instance());
        heap.registry_entries.push(super::RegistryEntry {
            kind: "SOUND_EVENT".into(),
            id: "example:sound".into(),
            value: super::Value::Object(Some(value)),
            holder,
        });
        assert!(heap.refs_into(mark));
        assert!(!heap.end_scope(true));
        assert_eq!(heap.registry_entries[0].holder, holder);
    }

    /// An intrusive holder is two heap ids that must agree with the object the
    /// registry registered.
    #[test]
    fn an_intrusive_holder_keeps_the_scope() {
        let mut heap = Heap::new();
        let object = heap.alloc(instance());
        heap.begin_scope();
        let mark = heap.scope_mark().unwrap();
        let holder = heap.alloc(instance());
        heap.intrusive_holders.insert(object, holder);
        assert!(heap.refs_into(mark));
        assert!(!heap.end_scope(true));
    }

    /// A native object reports the ids it holds through
    /// [`super::NativeObject::held_ids`], not through a fixed downcast list:
    /// a callback info's `return_value` is the case a scope must not lose.
    #[test]
    fn a_native_objects_held_value_keeps_the_scope() {
        let mut heap = Heap::new();
        // The native is allocated *before* the scope, as a real one is (a
        // callback info exists before the callback runs); the value it is
        // written with during the scope is the one the scan must find.
        let info = heap.alloc_native(crate::minecraft::CallbackInfo::returnable(
            "target",
            true,
            super::Value::NULL,
        ));
        heap.begin_scope();
        let mark = heap.scope_mark().unwrap();
        let returned = heap.alloc(instance());
        if let Some(callback) = heap.get_native_mut::<crate::minecraft::CallbackInfo>(info) {
            callback.return_value = super::Value::Object(Some(returned));
        }
        assert!(heap.refs_into(mark));
        assert!(!heap.end_scope(true));
    }

    /// A native that holds nothing reports no ids, so it neither keeps a scope
    /// nor makes the scan do work.
    #[test]
    fn a_native_with_no_values_keeps_nothing() {
        let mut heap = Heap::new();
        heap.begin_scope();
        let mark = heap.scope_mark().unwrap();
        heap.alloc_native(crate::minecraft::BlockPos { x: 1, y: 2, z: 3 });
        assert!(!heap.refs_into(mark));
        assert!(heap.end_scope(false));
    }

    /// A write outside a scope that a *later* scope repeats is seen by that
    /// scope: the barrier records by generation, so each scope gets its own
    /// entry for the same object.
    #[test]
    fn the_same_object_written_in_a_later_scope_is_scanned_then() {
        let mut heap = Heap::new();
        let old = heap.alloc(instance());
        heap.begin_scope();
        let first = heap.scope_mark().unwrap();
        let fresh = heap.alloc(instance());
        if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(old) {
            fields.insert("kept".into(), super::Value::Object(Some(fresh)));
        }
        assert!(heap.refs_into(first));
        assert_eq!(heap.refs_scanned(), 1);
        assert!(!heap.end_scope(true));

        heap.begin_scope();
        let second = heap.scope_mark().unwrap();
        assert!(
            !heap.refs_into(second),
            "a closed scope's writes leaked into the next one"
        );
        assert_eq!(heap.refs_scanned(), 0);
        heap.get_mut(old).unwrap();
        // The write is old enough to have existed in the earlier scope, but
        // this scope has to note it again: the answer is per scope.
        assert!(!heap.refs_into(second));
        assert_eq!(
            heap.refs_scanned(),
            1,
            "the next scope did not note the write"
        );
        heap.end_scope(false);
    }
}
