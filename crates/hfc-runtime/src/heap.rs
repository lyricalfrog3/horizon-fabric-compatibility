//! Heap memory storage for JVM objects.

use crate::value::{JavaObject, NativeObject, Value};
use std::collections::HashMap;

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
            identifiers: HashMap::new(),
            registries: crate::registry::Registries::default(),
            host_requests: Vec::new(),
            clipboard: String::new(),
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
        self.alloc(JavaObject::Iterator { items, cursor: 0 })
    }
}
