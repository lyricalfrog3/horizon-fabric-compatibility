//! Bytecode interpreter for JVM classes and lambda closures.

use crate::builtins::{BuiltinResult, dispatch_builtin};
use crate::class::{BootstrapMethodInfo, CodeInfo, Opcode};
use crate::heap::Heap;
use crate::loader::ClassLoader;
use crate::minecraft::*;
use crate::value::{JavaObject, Value};
use hfc_core::{Error, Result};
use std::collections::HashMap;

/// `jdk.internal.access.SharedSecrets`, which the JVM's bootstrap fills in.
const SHARED_SECRETS: &str = "jdk/internal/access/SharedSecrets";

/// Marks a class initializer on the catch-site stack (not a class name).
const CLINIT_BOUNDARY: &str = "<clinit>";

/// JDK bootstrap classes whose statics the runtime supplies instead of running
/// their initializers: they build the module/CDS class-loader hierarchy of a
/// JVM, and the runtime is a single loader ([`crate::builtins`] answers
/// `ClassLoader.getSystemClassLoader` and friends with it). Like `Language`
/// and `Item.Properties`, their initializer would build JVM internals the
/// runtime does not have.
fn runtime_supplied_state(class_name: &str) -> bool {
    matches!(
        class_name,
        // Its statics are synthesized (`has_synthetic_statics`); running the
        // real initializer would build Minecraft's whole registry, which the
        // runtime answers from `Heap::registry_entries` instead.
        "net/minecraft/core/registries/BuiltInRegistries"
            | "net/minecraft/world/item/Items"
            | "net/minecraft/world/level/block/Blocks"
            | "net/minecraft/core/component/DataComponents"
            | "net/minecraft/core/particles/ParticleTypes"
            | "net/minecraft/stats/Stats"
            | "net/minecraft/tags/ItemTags"
            | "net/minecraft/tags/BlockTags"
            | "jdk/internal/loader/ClassLoaders"
            | "jdk/internal/loader/ClassLoaders$PlatformClassLoader"
            | "jdk/internal/loader/ClassLoaders$AppClassLoader"
            | "jdk/internal/loader/ArchivedClassLoaders"
            | "jdk/internal/loader/BuiltinClassLoader"
            | "jdk/internal/loader/BootLoader"
    )
}

/// Classes whose static fields the runtime supplies itself
/// ([`Interpreter::create_static_field_val`]). Running their real `<clinit>`
/// builds Minecraft's whole item/block/tag registry, which takes tens of
/// seconds in the interpreter and is thrown away by the synthetic field a
/// moment later. A mod reading `Items.WHEAT` must not pay for it.
fn has_synthetic_statics(class_name: &str) -> bool {
    matches!(
        class_name,
        "net/minecraft/world/item/Items"
            | "net/minecraft/world/level/block/Blocks"
            | "net/minecraft/tags/ItemTags"
            | "net/minecraft/tags/BlockTags"
            | "net/minecraft/core/component/DataComponents"
            | "net/minecraft/core/registries/Registries"
            | "net/minecraft/stats/Stats"
            | "net/minecraft/core/registries/BuiltInRegistries"
            | "net/minecraft/core/particles/ParticleTypes"
    )
}

/// The first byte of each parameter's type in a method descriptor
/// (`I`, `J`, `L`, `[` ...), in order.
fn parameter_kinds(descriptor: &str) -> Vec<u8> {
    let bytes = descriptor.as_bytes();
    let mut kinds = Vec::new();
    let mut i = 1;
    while i < bytes.len() && bytes[i] != b')' {
        let start = bytes[i];
        while bytes[i] == b'[' {
            i += 1;
        }
        if bytes[i] == b'L' {
            while bytes[i] != b';' {
                i += 1;
            }
        }
        i += 1;
        // An array of anything is a reference.
        kinds.push(if start == b'[' { b'[' } else { start });
    }
    kinds
}

/// The value a field of the given descriptor holds before anything writes it.
fn default_for_descriptor(descriptor: &str) -> Value {
    match descriptor.as_bytes().first() {
        Some(b'Z' | b'B' | b'C' | b'S' | b'I') => Value::Int(0),
        Some(b'J') => Value::Long(0),
        Some(b'F') => Value::Float(0.0),
        Some(b'D') => Value::Double(0.0),
        _ => Value::NULL,
    }
}

/// The bytecode interpreter running loaded mod classes and Minecraft shims.
pub struct Interpreter {
    pub loader: ClassLoader,
    pub heap: Heap,
    pub static_singletons: HashMap<(String, String), Value>,
    /// When set, every class or method the runtime could not resolve is recorded
    /// here instead of being silently skipped. Mods that "initialize" without
    /// touching their API surface show up as entries in this list.
    pub trace_missing: bool,
    pub missing: Vec<String>,
    /// Tasks given to `MinecraftServer.execute`, run by [`Self::run_server_tasks`].
    pub server_tasks: std::collections::VecDeque<Value>,
    /// Optional instruction budget. `None` runs without a limit, as a JVM does.
    /// A finite budget keeps a mis-decoded branch from hanging the host.
    pub step_budget: Option<u64>,
    pub call_depth: usize,
    /// When set, every entered method is pushed onto [`Self::call_stack`] so a
    /// budget exhaustion can name the Java frames it happened under. Off in
    /// normal runs: the bookkeeping allocates a string per call.
    pub trace_calls: bool,
    /// How many calls the runtime could not resolve, and the last of them: an
    /// initializer that fails with one of these in flight failed because the
    /// runtime lacks a symbol, not because the mod is broken.
    pub(crate) unresolved_calls: u64,
    pub(crate) last_unresolved: Option<String>,
    /// The Java exception in flight: a method returned by throwing and the
    /// caller has not handled it yet. The first frame whose handler covers the
    /// call site catches it; when none does it reaches the entrypoint, which
    /// reports it.
    pub(crate) pending_exception: Option<Value>,
    /// The `catch` types of the handlers that cover each pending call site on
    /// the interpreted stack: whether a fault the interpreter detects would be
    /// caught by some caller.
    catch_sites: Vec<String>,
    /// Set while a property factory runs, so its own call does not intern again.
    interning_property: bool,

    /// How many class initializers are running: a class initializing another
    /// one re-enters the interpreter.
    clinit_depth: usize,
    /// The classes whose initializer is running, innermost last. A `getstatic`
    /// of one of them sees the field's real value (`null`/zero before the
    /// initializer assigns it), never a synthesized placeholder: the JVM has
    /// the field, it is just not assigned yet.
    initializing: Vec<String>,
    /// The class each vanilla block constant is constructed as, by block id.
    vanilla_block_classes: HashMap<String, Option<String>>,
    /// Classes whose `<clinit>` threw, by class, with the original cause: a JVM
    /// answers `ExceptionInInitializerError` once and `NoClassDefFoundError`
    /// every time after.
    pub(crate) failed_initializers: HashMap<String, Value>,
    /// Set while an exception object is being built, so a failed initializer
    /// of an exception class cannot recurse into building another one.
    building_exception: bool,
    /// The class of each interpreted frame, innermost last: what
    /// `Reflection.getCallerClass` and `StackWalker.getCallerClass` read.
    frame_classes: Vec<String>,
    /// The method name of each interpreted frame, parallel to `frame_classes`:
    /// an exception's report names the frames it crossed.
    frame_methods: Vec<String>,
    /// How many of `frame_classes` are live frames.
    frame_depth: usize,
    pub call_stack: Vec<String>,
    /// The `value`/`coder` fields of runtime strings, by heap id: JDK `String`
    /// bytecode reads them.
    string_fields: HashMap<usize, (usize, i32)>,
    /// Menu widget injections are applied only while the bridge runs a host
    /// menu's layout method, never to ordinary gameplay code.
    pub menu_injection: bool,
    /// Depth inside a mixin callback; layout additions made here are the
    /// widgets a menu contribution may draw.
    pub injection_depth: usize,
    /// Widget heap ids added by the running menu injectors.
    pub menu_contributions: Vec<usize>,
    /// `@Accessor` / `@Invoker` mixin methods by (interface, name, descriptor):
    /// a call naming the interface is performed on the class it is mixed into.
    pub mixin_accessors: HashMap<(String, String, String), crate::mixin::MixinAccessor>,
    /// Interfaces mixins add to a target class, by target.
    pub mixin_interfaces: HashMap<String, Vec<String>>,
    /// Interface methods mixins add, by (interface, method, descriptor).
    pub mixin_interface_methods:
        HashMap<(String, String, String), Vec<crate::mixin::MixinInterfaceMethod>>,
    /// The class declaring each static field reached through a subclass,
    /// by (named class, field); a class hierarchy never changes once loaded.
    static_field_owners: HashMap<(String, String), Option<String>>,
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpreter {
    pub fn new() -> Self {
        Self {
            loader: ClassLoader::new(),
            heap: Heap::new(),
            static_singletons: HashMap::new(),
            trace_missing: false,
            server_tasks: std::collections::VecDeque::new(),
            missing: Vec::new(),
            step_budget: None,
            call_depth: 0,
            trace_calls: false,
            pending_exception: None,
            catch_sites: Vec::new(),
            interning_property: false,
            clinit_depth: 0,
            initializing: Vec::new(),
            vanilla_block_classes: HashMap::new(),
            unresolved_calls: 0,
            last_unresolved: None,
            failed_initializers: HashMap::new(),
            building_exception: false,
            frame_classes: Vec::new(),
            frame_methods: Vec::new(),
            frame_depth: 0,
            call_stack: Vec::new(),
            string_fields: HashMap::new(),
            menu_injection: false,
            injection_depth: 0,
            menu_contributions: Vec::new(),
            mixin_accessors: HashMap::new(),
            mixin_interfaces: HashMap::new(),
            mixin_interface_methods: HashMap::new(),
            static_field_owners: HashMap::new(),
        }
    }

    pub fn with_loader(loader: ClassLoader) -> Self {
        Self {
            loader,
            heap: Heap::new(),
            static_singletons: HashMap::new(),
            trace_missing: false,
            server_tasks: std::collections::VecDeque::new(),
            missing: Vec::new(),
            step_budget: None,
            call_depth: 0,
            trace_calls: false,
            pending_exception: None,
            catch_sites: Vec::new(),
            interning_property: false,
            clinit_depth: 0,
            initializing: Vec::new(),
            vanilla_block_classes: HashMap::new(),
            unresolved_calls: 0,
            last_unresolved: None,
            failed_initializers: HashMap::new(),
            building_exception: false,
            frame_classes: Vec::new(),
            frame_methods: Vec::new(),
            frame_depth: 0,
            call_stack: Vec::new(),
            string_fields: HashMap::new(),
            menu_injection: false,
            injection_depth: 0,
            menu_contributions: Vec::new(),
            mixin_accessors: HashMap::new(),
            mixin_interfaces: HashMap::new(),
            mixin_interface_methods: HashMap::new(),
            static_field_owners: HashMap::new(),
        }
    }

    /// Run `f` under a fresh instruction budget, restoring the previous one
    /// afterwards. A callback that runs out of budget leaves the interpreter
    /// with none, which made every later callback fail at its first
    /// instruction; this keeps one failure from poisoning the next.
    pub(crate) fn with_budget<R>(&mut self, budget: u64, f: impl FnOnce(&mut Self) -> R) -> R {
        let previous = std::mem::replace(&mut self.step_budget, Some(budget));
        let result = f(self);
        self.step_budget = previous;
        result
    }

    /// What a call's receiver is, for the "method not found" debug line.
    fn describe_receiver(&self, virtual_dispatch: bool, args: &[Value]) -> String {
        if !virtual_dispatch {
            return "static".into();
        }
        match args.first() {
            None => "none".into(),
            Some(value) => match value.as_obj() {
                None => format!("{value:?}"),
                Some(id) => match self.heap.get(id) {
                    None => "dangling".into(),
                    Some(JavaObject::Instance { class_name, .. }) => {
                        format!("instance {class_name}")
                    }
                    Some(JavaObject::Lambda {
                        interface_class,
                        target_class,
                        target_method,
                        target_desc,
                        ..
                    }) => format!(
                        "lambda {interface_class} -> {target_class}.{target_method}{target_desc}"
                    ),
                    Some(other) => format!("{other:?}"),
                },
            },
        }
    }

    fn note_missing(&mut self, what: String) {
        self.unresolved_calls += 1;
        self.last_unresolved = Some(what.clone());
        if self.trace_missing {
            let caller = if self.trace_calls {
                let n = self.call_stack.len();
                self.call_stack[n.saturating_sub(3)..]
                    .iter()
                    .rev()
                    .map(|frame| format!(" <- {frame}"))
                    .collect::<String>()
            } else {
                String::new()
            };
            self.missing.push(format!("{what}{caller}"));
        }
    }

    /// Allocate a `Multianewarray` value: `counts[0]` outer length, and so on.
    fn build_multidim(&mut self, elem: &str, counts: &[usize]) -> Value {
        let (len, rest) = match counts.split_first() {
            Some((len, rest)) => (*len, rest),
            None => return Value::NULL,
        };
        let mut elements = Vec::with_capacity(len);
        for _ in 0..len {
            elements.push(if rest.is_empty() {
                Value::NULL
            } else {
                self.build_multidim(elem, rest)
            });
        }
        let id = self.heap.alloc(JavaObject::Array {
            elem_type: elem.to_string(),
            elements,
        });
        Value::Object(Some(id))
    }

    /// Ensure `<clinit>` of a class has run.
    pub fn ensure_initialized(&mut self, class_name: &str) -> Result<()> {
        // A class whose initializer threw: `ExceptionInInitializerError` was
        // raised once (see below), and every later use answers
        // `NoClassDefFoundError`, as a JVM does.
        if self.failed_initializers.contains_key(class_name) {
            if !self.building_exception {
                let message = format!(
                    "Could not initialize class {}",
                    class_name.replace('/', ".")
                );
                let exception = self.new_exception("java/lang/NoClassDefFoundError", &message);
                self.pending_exception = Some(exception);
            }
            return Ok(());
        }
        let needs_init = if let Some(class) = self.loader.get(class_name) {
            !class.static_initialized
        } else if let Some(class) = self.loader.get_or_load(class_name)? {
            !class.static_initialized
        } else {
            false
        };

        if needs_init {
            if let Some(class) = self.loader.get_mut(class_name) {
                class.static_initialized = true;
            }
            // Only an exception this initializer raises belongs to the class:
            // one already in flight came from the caller's own frame.
            let outer_exception = self.pending_exception.take();
            let unresolved_before = self.unresolved_calls;
            if let Some(class) = self.loader.get(class_name) {
                // `Language` is replaced wholesale (translations come from the
                // host), so its initializer, which reads en_us.json through JDK
                // stream decoders, must not run.
                // `Item$Properties` is replaced wholesale too (`item_builtins.rs`).
                if class_name != "net/minecraft/locale/Language"
                    && class_name != "net/minecraft/world/item/Item$Properties"
                    && !runtime_supplied_state(class_name)
                    && class
                        .methods
                        .contains_key(&("<clinit>".into(), "()V".into()))
                {
                    // A throwing initializer leaves the class half-initialized (a JVM
                    // would raise ExceptionInInitializerError on the next use); say so.
                    // Inside the initializer, an exception that escapes it
                    // reaches the callers as `ExceptionInInitializerError`.
                    let mark = self.catch_sites.len();
                    self.catch_sites.push(CLINIT_BOUNDARY.to_string());
                    let nested = self.clinit_depth > 0;
                    self.clinit_depth += 1;
                    self.initializing.push(class_name.to_string());
                    let ran = self.execute_special_method(class_name, "<clinit>", "()V", &[]);
                    self.initializing.pop();
                    self.clinit_depth -= 1;
                    self.catch_sites.truncate(mark);
                    let _ = nested;
                    if let Err(error) = ran {
                        log::warn!("static initializer of {class_name} failed: {error}");
                    }
                    // A throwing initializer fails the class. The first use
                    // raises `ExceptionInInitializerError` around the cause; the
                    // runtime then answers `NoClassDefFoundError`.
                    if let Some(cause) = self.pending_exception.take() {
                        // A symbol the runtime lacks, not the mod's own failure:
                        // a JVM would never have taken this path, so the class
                        // keeps the statics it got and the failure is reported
                        // rather than poisoning every later use with
                        // `NoClassDefFoundError`.
                        if self.unresolved_calls > unresolved_before {
                            let what = self
                                .last_unresolved
                                .clone()
                                .unwrap_or_else(|| "an unresolved call".into());
                            log::warn!(
                                "{class_name}'s initializer hit {what}, which the runtime lacks; its statics are partial"
                            );
                            self.pending_exception = outer_exception;
                            return Ok(());
                        }
                        let wrapped = self.new_exception_with(
                            "java/lang/ExceptionInInitializerError",
                            "(Ljava/lang/Throwable;)V",
                            std::slice::from_ref(&cause),
                        );
                        self.failed_initializers
                            .insert(class_name.to_string(), cause);
                        self.pending_exception = Some(wrapped);
                    } else {
                        self.pending_exception = outer_exception;
                    }
                }
            }
            if class_name == SHARED_SECRETS {
                self.install_java_lang_access();
            }
            if class_name == "jdk/internal/misc/VM" {
                self.install_vm_state();
            }
        }
        Ok(())
    }

    /// Do what `System.initPhase1` does for `SharedSecrets`: hand it the
    /// JDK's own `JavaLangAccess` (the anonymous class `System` declares).
    /// No JVM bootstrap runs here, so without this `getJavaLangAccess()`
    /// answers null, and `EnumMap`/`EnumSet`, which read an enum's constants
    /// through it, are built over no constants.
    /// What a JVM's `System.initPhase1` leaves in `jdk.internal.misc.VM`: the
    /// saved properties and the init level. The JDK's own classes read both
    /// (`VM.getSavedProperties()` throws "Not yet initialized" without them),
    /// and the runtime does not run the boot sequence that sets them.
    fn install_vm_state(&mut self) {
        let properties = crate::builtins::system_properties(&mut self.heap);
        if let Some(class) = self.loader.get_mut("jdk/internal/misc/VM") {
            class
                .static_fields
                .insert("savedProps".to_string(), Value::Object(Some(properties)));
            // JAVA_LANG_SYSTEM_INITED | MODULE_SYSTEM_INITED | SYSTEM_BOOTED.
            class
                .static_fields
                .insert("initLevel".to_string(), Value::Int(3));
        }
    }

    fn install_java_lang_access(&mut self) {
        const ACCESS: &str = "jdk/internal/access/JavaLangAccess";
        let assigned = self
            .loader
            .get(SHARED_SECRETS)
            .and_then(|c| c.static_fields.get("javaLangAccess"))
            .is_some_and(|v| !v.is_null());
        if assigned {
            return;
        }
        // `System`'s anonymous classes are numbered by the compiler; the one
        // implementing the interface is the implementation.
        let implementation = (1..64)
            .map(|n| format!("java/lang/System${n}"))
            .find(|name| {
                matches!(self.loader.get_or_load(name), Ok(Some(class))
                if class.interfaces.iter().any(|i| i == ACCESS)
                    && class.methods.contains_key(&("<init>".into(), "()V".into())))
            });
        let Some(implementation) = implementation else {
            log::warn!("the JDK image has no System class implementing {ACCESS}");
            return;
        };
        if let Err(error) = self.ensure_initialized(&implementation) {
            log::warn!("{implementation} could not be initialized: {error}");
            return;
        }
        let object = Value::Object(Some(self.alloc_uninitialized(&implementation)));
        let installed = self
            .execute_special_method(
                &implementation,
                "<init>",
                "()V",
                std::slice::from_ref(&object),
            )
            .and_then(|_| {
                self.execute_method(
                    SHARED_SECRETS,
                    "setJavaLangAccess",
                    &format!("(L{ACCESS};)V"),
                    &[object],
                )
            });
        if let Err(error) = installed {
            log::warn!("SharedSecrets.setJavaLangAccess failed: {error}");
        }
    }

    /// Replace each argument that is a collection implemented in bytecode (not
    /// by the heap) with a heap list of its elements, in iteration order.
    fn adopt_collection_args(&mut self, args: &[Value], first: usize) -> Result<Vec<Value>> {
        let mut out = Vec::with_capacity(args.len());
        for (index, arg) in args.iter().enumerate() {
            let real = index >= first
                && arg.as_obj().is_some_and(|id| {
                    matches!(self.heap.get(id), Some(JavaObject::Instance { .. }))
                })
                && self.is_instance_of(arg, "java/util/Collection");
            if !real {
                out.push(arg.clone());
                continue;
            }
            let iterator = self.execute_method(
                "java/util/Collection",
                "iterator",
                "()Ljava/util/Iterator;",
                std::slice::from_ref(arg),
            )?;
            let mut items = Vec::new();
            while self
                .execute_method(
                    "java/util/Iterator",
                    "hasNext",
                    "()Z",
                    std::slice::from_ref(&iterator),
                )?
                .as_int()
                != 0
            {
                items.push(self.execute_method(
                    "java/util/Iterator",
                    "next",
                    "()Ljava/lang/Object;",
                    std::slice::from_ref(&iterator),
                )?);
            }
            out.push(Value::Object(Some(self.heap.alloc_list(items))));
        }
        Ok(out)
    }

    /// Check if a value is an instance of the target type.
    pub fn is_instance_of(&mut self, obj_val: &Value, target_type: &str) -> bool {
        // A boxed number is a plain value; it is an instance of its wrapper.
        if !matches!(obj_val, Value::Object(_)) {
            return match obj_val {
                // A boxed `int` is also what a `char`, `boolean`, `byte` or
                // `short` box to: the runtime's boxed numbers are plain values
                // and do not carry which wrapper made them.
                Value::Int(_) => matches!(
                    target_type,
                    "java/lang/Integer"
                        | "java/lang/Character"
                        | "java/lang/Boolean"
                        | "java/lang/Short"
                        | "java/lang/Byte"
                        | "java/lang/Number"
                        | "java/lang/Comparable"
                ),
                Value::Long(_) => matches!(
                    target_type,
                    "java/lang/Long" | "java/lang/Number" | "java/lang/Comparable"
                ),
                Value::Float(_) => matches!(
                    target_type,
                    "java/lang/Float" | "java/lang/Number" | "java/lang/Comparable"
                ),
                Value::Double(_) => matches!(
                    target_type,
                    "java/lang/Double" | "java/lang/Number" | "java/lang/Comparable"
                ),
                _ => false,
            } || target_type == "java/lang/Object";
        }
        let Some(id) = obj_val.as_obj() else {
            return false;
        };
        if target_type == "java/lang/Object" {
            return true;
        }
        let Some(obj) = self.heap.get(id) else {
            return false;
        };
        match obj {
            JavaObject::String(_) => {
                matches!(
                    target_type,
                    "java/lang/String" | "java/lang/CharSequence" | "java/lang/Comparable"
                )
            }
            // A modelled collection stands for a JDK class (`ArrayList`,
            // `HashMap`, ...): answer from that class's own hierarchy, so the
            // interfaces, the `Abstract*` classes and the concrete type all
            // match, and a cast to an unrelated type does not.
            JavaObject::List(_)
            | JavaObject::Map(_)
            | JavaObject::Set(_)
            | JavaObject::Stream(_)
            | JavaObject::Iterator { .. } => {
                let modelled = obj.class_name().to_string();
                // The runtime's list, map and set objects do not record which
                // JDK implementation made them (`ArrayList` or `LinkedList`,
                // `HashMap` or `ConcurrentHashMap`), so a cast to any type of
                // that collection's family succeeds; the family is what the
                // builtins answer for.
                modelled_collection_assignable(&modelled, target_type)
                    || self.class_is_assignable_to(&modelled, target_type)
            }
            JavaObject::Class(_) => matches!(
                target_type,
                "java/lang/Class"
                    | "java/lang/reflect/Type"
                    | "java/lang/reflect/AnnotatedElement"
                    | "java/lang/Object"
            ),
            JavaObject::Array { .. } => {
                target_type.starts_with('[') || target_type == "java/lang/Object"
            }
            // A lambda is an instance of the interface it implements, and of
            // that interface's supertypes.
            JavaObject::Lambda {
                interface_class,
                intersections,
                ..
            } => {
                let implemented = interface_class.clone();
                let added = intersections.clone();
                let wanted = target_type.to_string();
                implemented == wanted
                    || added.iter().any(|name| *name == wanted)
                    || self.class_is_assignable_to(&implemented, &wanted)
            }
            JavaObject::Native(n) => {
                // A vanilla block constant is the class `Blocks` constructs it
                // as (`FIRE` is a `FireBlock`), with the interfaces mixins add
                // to that class (`FireBlockHooks`).
                if let Some(block) = n.as_any().downcast_ref::<crate::minecraft::Block>() {
                    let id = block.id.clone();
                    if let Some(class) = self.vanilla_block_class(&id) {
                        if class != "net/minecraft/world/level/block/Block"
                            && self.class_is_assignable_to(&class, target_type)
                        {
                            return true;
                        }
                    }
                }
                let Some(JavaObject::Native(n)) = self.heap.get(id) else {
                    return false;
                };
                let c = n.class_name();
                c == target_type
                    || (c == "net/minecraft/server/level/ServerPlayer"
                        && matches!(
                            target_type,
                            "net/minecraft/world/entity/player/Player"
                                | "net/minecraft/world/entity/LivingEntity"
                                | "net/minecraft/world/entity/Entity"
                        ))
                    || c.contains(target_type)
                    || (target_type.contains("CropBlock")
                        && (c.contains("CropBlock")
                            || c.contains("NetherWartBlock")
                            || c.contains("CocoaBlock")))
            }
            JavaObject::Instance { class_name, .. } => {
                let cls = class_name.clone();
                // The runtime models every Minecraft registry as one object that
                // answers what all of them answer (lookup, registration, keys),
                // so a cast between the registry interfaces succeeds — vanilla
                // code casts `BuiltInRegistries.SOUND_EVENT` to `WritableRegistry`
                // and back.
                if is_registry_model_class(&cls) && is_registry_model_class(target_type) {
                    return true;
                }
                self.class_is_assignable_to(&cls, target_type)
            }
            _ => false,
        }
    }

    /// Run the tasks queued through `MinecraftServer.execute`, including the
    /// ones they queue in turn.
    pub fn run_server_tasks(&mut self) {
        let mut ran = 0;
        while let Some(task) = self.server_tasks.pop_front() {
            ran += 1;
            if ran > 100_000 {
                log::warn!("server task queue not draining; dropping the rest");
                self.server_tasks.clear();
                break;
            }
            if let Err(error) = self.execute_method("java/lang/Runnable", "run", "()V", &[task]) {
                log::warn!("server task failed: {error}");
            }
        }
    }

    /// Public fields of host-backed objects that mods read directly.
    fn native_field(&mut self, id: usize, field_name: &str) -> Value {
        // The JDK's own `String` bytecode (`repeat`, `strip`, `split`, ...)
        // reads the object's `value` bytes and `coder`. The runtime's strings
        // keep their text, so those fields are built from it — LATIN1 when
        // every char fits, UTF-16BE otherwise, which is how a JVM stores them.
        if let Some(text) = self.heap.get_string(id).map(str::to_string) {
            let (value_id, coder) = self.string_bytes(id, &text);
            return match field_name {
                "value" => Value::Object(Some(value_id)),
                "coder" => Value::Int(coder),
                // A JVM caches the hash in the object; 0 means "not computed".
                "hash" => Value::Int(0),
                "hashIsZero" => Value::from_bool(false),
                _ => Value::NULL,
            };
        }
        // `Player.containerMenu`: the menu the player has open. A JVM field
        // reads through the player's own class; a host-backed `Player` is a
        // native object, so a mod's bytecode (`handleContainerInput` reads
        // `player.containerMenu`) has to find it here.
        if field_name == "containerMenu" {
            if let Some(player) = self.heap.get_native::<Player>(id) {
                return player
                    .container_menu
                    .map(|menu| Value::Object(Some(menu)))
                    .unwrap_or(Value::NULL);
            }
        }
        if field_name == "gameMode" {
            if let Some(player) = self.heap.get_native::<Player>(id) {
                let level_id = player.level_id;
                let mut fields = HashMap::new();
                fields.insert("level".to_string(), Value::Object(Some(level_id)));
                fields.insert("player".to_string(), Value::Object(Some(id)));
                let mode = self.heap.alloc(JavaObject::Instance {
                    class_name: "net/minecraft/server/level/ServerPlayerGameMode".into(),
                    fields,
                });
                return Value::Object(Some(mode));
            }
        }
        Value::NULL
    }

    /// The `value` byte array and `coder` of a runtime string, built once and
    /// kept: a Java string never changes.
    fn string_bytes(&mut self, id: usize, text: &str) -> (usize, i32) {
        if let Some((value, coder)) = self.string_fields.get(&id) {
            return (*value, *coder);
        }
        let latin1 = text.chars().all(|c| (c as u32) <= 0xff);
        let mut bytes = Vec::with_capacity(text.len() * if latin1 { 1 } else { 2 });
        if latin1 {
            for c in text.chars() {
                bytes.push(Value::Int(c as u32 as i32));
            }
        } else {
            for unit in text.encode_utf16() {
                bytes.push(Value::Int((unit >> 8) as i32));
                bytes.push(Value::Int((unit & 0xff) as i32));
            }
        }
        let value = self.heap.alloc(JavaObject::Array {
            elem_type: "byte".into(),
            elements: bytes,
        });
        let coder = i32::from(!latin1);
        self.string_fields.insert(id, (value, coder));
        (value, coder)
    }

    /// Check if `from_class` is assignable to `to_type` traversing class hierarchy.
    pub fn class_is_assignable_to(&mut self, from_class: &str, to_type: &str) -> bool {
        if from_class == to_type || to_type == "java/lang/Object" {
            return true;
        }
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(from_class.to_string());
        let mut visited = std::collections::HashSet::new();

        while let Some(curr) = queue.pop_front() {
            if curr == to_type {
                return true;
            }
            if !visited.insert(curr.clone()) {
                continue;
            }
            // Interfaces mixins add to a class count even before it loads.
            if let Some(added) = self.mixin_interfaces.get(&curr) {
                queue.extend(added.iter().cloned());
            }
            let _ = self.loader.get_or_load(&curr);
            let Some(c) = self.loader.get(&curr) else {
                continue;
            };
            if let Some(s) = &c.super_class {
                queue.push_back(s.clone());
            }
            for iface in &c.interfaces {
                queue.push_back(iface.clone());
            }
        }
        false
    }

    /// Execute a method using virtual dispatch.
    pub fn execute_method(
        &mut self,
        class_name: &str,
        method_name: &str,
        descriptor: &str,
        args: &[Value],
    ) -> Result<Value> {
        self.execute_method_dispatch(class_name, method_name, descriptor, args, true)
    }

    /// Execute a method using non-virtual (special/static) dispatch.
    pub fn execute_special_method(
        &mut self,
        class_name: &str,
        method_name: &str,
        descriptor: &str,
        args: &[Value],
    ) -> Result<Value> {
        self.execute_method_dispatch(class_name, method_name, descriptor, args, false)
    }

    fn execute_method_dispatch(
        &mut self,
        class_name: &str,
        method_name: &str,
        descriptor: &str,
        args: &[Value],
        virtual_dispatch: bool,
    ) -> Result<Value> {
        if self.call_depth > 100 {
            let stack = if self.trace_calls {
                let from = self.call_stack.len().saturating_sub(12);
                format!(
                    "; stack: {}",
                    self.call_stack[from..]
                        .iter()
                        .rev()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" <- ")
                )
            } else {
                String::new()
            };
            return Err(Error::new(
                "interpreter",
                format!("call stack overflow at {class_name}.{method_name}{descriptor}{stack}"),
            ));
        }
        self.call_depth += 1;
        let traced = self.trace_calls;
        if traced {
            self.call_stack
                .push(format!("{class_name}.{method_name}{descriptor}"));
        }
        let entry = self.call_depth == 1;
        let unresolved_before = self.unresolved_calls;
        let res =
            self.execute_method_inner(class_name, method_name, descriptor, args, virtual_dispatch);
        if traced {
            self.call_stack.pop();
        }
        self.call_depth -= 1;
        // A call the host made (not one frame calling another) has no caller to
        // unwind into: an exception it leaves behind is the call failing, and an
        // entrypoint is told so instead of being handed a default as a result.
        // The one exception is a failure the runtime caused itself (a symbol it
        // lacks): the entrypoint is not at fault, and reporting it as one would
        // blame a mod for the runtime's own gap.
        if entry {
            if let Some(exception) = self.pending_exception.take() {
                if self.unresolved_calls > unresolved_before {
                    let what = self
                        .last_unresolved
                        .clone()
                        .unwrap_or_else(|| "an unresolved call".into());
                    log::warn!(
                        "{class_name}.{method_name} failed on {what}, which the runtime lacks: \
                         {}",
                        self.describe_exception(&exception)
                    );
                } else {
                    let described = self.describe_exception(&exception);
                    return match res {
                        Ok(_) => Err(Error::new(
                            "interpreter",
                            format!("{class_name}.{method_name} threw {described}"),
                        )),
                        Err(error) => Err(error),
                    };
                }
            }
        }
        res
    }

    /// Whether `class_name` or one of its supertypes has a body for
    /// `method_name`/`descriptor`.
    fn has_concrete_method(
        &mut self,
        class_name: &str,
        method_name: &str,
        descriptor: &str,
    ) -> bool {
        let key = (method_name.to_string(), descriptor.to_string());
        let mut queue = vec![class_name.to_string()];
        let mut seen = std::collections::HashSet::new();
        while let Some(current) = queue.pop() {
            if current == "java/lang/Object" || !seen.insert(current.clone()) {
                continue;
            }
            let _ = self.loader.get_or_load(&current);
            let Some(class) = self.loader.get(&current) else {
                continue;
            };
            if class.methods.get(&key).is_some_and(|m| m.code.is_some()) {
                return true;
            }
            queue.extend(class.super_class.iter().cloned());
            queue.extend(class.interfaces.iter().cloned());
        }
        false
    }

    /// Whether `class_name` declares `method_name`/`descriptor` as a static method.
    fn is_static_method(&mut self, class_name: &str, method_name: &str, descriptor: &str) -> bool {
        let _ = self.loader.get_or_load(class_name);
        self.loader.get(class_name).is_some_and(|class| {
            class
                .methods
                .get(&(method_name.to_string(), descriptor.to_string()))
                .is_some_and(|method| method.is_static)
        })
    }

    /// A fresh, uninitialized instance of `cls`, as `new` produces before the
    /// constructor runs.
    #[inline(never)]
    pub fn alloc_uninitialized(&mut self, cls: &str) -> usize {
        match cls {
            "java/util/ArrayList" | "java/util/LinkedList" => {
                self.heap.alloc(JavaObject::List(Vec::new()))
            }
            // `TreeMap`/`TreeSet` run as the JDK's own bytecode too, and so do
            // `EnumMap`/`EnumSet`: their order is the enum's, and their
            // `typeCheck` compares `getClass()` with the `keyType` their own
            // constructor stored, which only real bytecode keeps.
            "java/util/HashMap"
            | "java/util/LinkedHashMap"
            | "java/util/concurrent/ConcurrentHashMap" => {
                self.heap.alloc(JavaObject::Map(Vec::new()))
            }
            "java/util/HashSet" | "java/util/LinkedHashSet" => {
                self.heap.alloc(JavaObject::Set(Vec::new()))
            }
            _ => self.heap.alloc(JavaObject::Instance {
                class_name: cls.to_string(),
                fields: HashMap::new(),
            }),
        }
    }

    /// `Class.getConstructor` / `getDeclaredConstructor` and
    /// `Constructor.newInstance`: reflection that has to run a constructor, so
    /// it lives here rather than with the heap-only builtins. A `Constructor`
    /// is an instance remembering its class and parameter types.
    #[inline(never)]
    fn reflect_constructor(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[Value],
    ) -> Option<Result<Value>> {
        match (class_name, method_name) {
            ("java/lang/Class", "getConstructor" | "getDeclaredConstructor") => {
                let class_obj = args.first()?.clone();
                let params = args.get(1).cloned().unwrap_or(Value::NULL);
                let mut fields = HashMap::new();
                fields.insert("class".to_string(), class_obj);
                fields.insert("parameterTypes".to_string(), params);
                let id = self.heap.alloc(JavaObject::Instance {
                    class_name: "java/lang/reflect/Constructor".into(),
                    fields,
                });
                Some(Ok(Value::Object(Some(id))))
            }
            ("java/lang/reflect/Constructor", "newInstance") => {
                let (target, params) = match self.heap.get(args.first()?.as_obj()?) {
                    Some(JavaObject::Instance { fields, .. }) => (
                        fields.get("class")?.clone(),
                        fields.get("parameterTypes")?.clone(),
                    ),
                    _ => return None,
                };
                let target_name = match self.heap.get(target.as_obj()?) {
                    Some(JavaObject::Class(name)) => name.clone(),
                    _ => return None,
                };
                let mut descriptor = String::from("(");
                if let Some(JavaObject::Array { elements, .. }) =
                    params.as_obj().and_then(|id| self.heap.get(id))
                {
                    for element in elements {
                        if let Some(JavaObject::Class(name)) =
                            element.as_obj().and_then(|id| self.heap.get(id))
                        {
                            descriptor.push_str(&class_descriptor(name));
                        }
                    }
                }
                descriptor.push_str(")V");
                let call_args: Vec<Value> = match args.get(1).and_then(|v| v.as_obj()) {
                    Some(id) => match self.heap.get(id) {
                        Some(JavaObject::Array { elements, .. }) => elements.clone(),
                        _ => Vec::new(),
                    },
                    None => Vec::new(),
                };
                Some((|| {
                    self.ensure_initialized(&target_name)?;
                    let id = self.alloc_uninitialized(&target_name);
                    let this = Value::Object(Some(id));
                    let mut full = vec![this.clone()];
                    full.extend(call_args);
                    self.execute_method(&target_name, "<init>", &descriptor, &full)?;
                    Ok(this)
                })())
            }
            _ => None,
        }
    }

    /// A call on an `@Accessor` / `@Invoker` mixin interface is a call on the
    /// class Mixin merges the interface into: an invoker calls the named target
    /// method, an accessor reads or writes the named field.
    ///
    /// Mixin resolves each accessor against its target class and generates a
    /// bridge whose invocation has the same parameters and return type as the
    /// accessor (`AccessorGeneratorMethodProxy`), so the redirect resolves the
    /// same way: the member may be private or protected, may be declared by a
    /// superclass, and either the accessor or its target may be static. A
    /// target that does not exist is Mixin's `InvalidAccessorException` at load
    /// time; here it is an error the call fails with, never a default.
    #[inline(never)]
    fn redirect_mixin_accessor(
        &mut self,
        class_name: &str,
        method_name: &str,
        descriptor: &str,
        args: &[Value],
    ) -> Option<Result<Value>> {
        if self.mixin_accessors.is_empty() {
            return None;
        }
        let accessor = self
            .mixin_accessors
            .get(&(
                class_name.to_string(),
                method_name.to_string(),
                descriptor.to_string(),
            ))?
            .clone();
        Some(self.perform_mixin_accessor(&accessor, args))
    }

    /// Perform one registered accessor method. `args` are the accessor's
    /// arguments as its caller passed them: `[receiver, ...]` for an instance
    /// accessor, the plain arguments for a static one.
    fn perform_mixin_accessor(
        &mut self,
        accessor: &crate::mixin::MixinAccessor,
        args: &[Value],
    ) -> Result<Value> {
        use crate::mixin::AccessorKind;
        match &accessor.kind {
            AccessorKind::Invoker { method } => {
                // Mixin generates the bridge with the accessor's own
                // descriptor, so the target is that name and descriptor.
                let target = self.mixin_accessor_target(accessor, args);
                let found = self.mixin_target_method(&target, method, &accessor.descriptor);
                let Some((declaring, is_static)) = found else {
                    return Err(self.mixin_accessor_missing(method, &target));
                };
                // Mixin requires the accessor's static-ness to match its
                // target's. Carrying on with either would call the wrong thing:
                // a static target handed a receiver takes it as its first
                // argument, an instance target without one reads a local that
                // was never set.
                if is_static != accessor.is_static {
                    return Err(self.mixin_accessor_mismatch(
                        method,
                        is_static,
                        accessor.is_static,
                    ));
                }
                // A static target takes exactly the accessor's arguments; an
                // instance target takes the receiver in front of them.
                let call_args: Vec<Value> = if is_static {
                    args.to_vec()
                } else {
                    let receiver = args.first().cloned().unwrap_or(Value::NULL);
                    let mut all = Vec::with_capacity(args.len());
                    all.push(receiver);
                    all.extend(args.iter().skip(1).cloned());
                    all
                };
                // `Bytecode.invokeMethod`: `invokestatic` for a static target,
                // `invokespecial` for a private one, `invokeinterface` when the
                // declaring class is an interface, else `invokevirtual`. The
                // declaring class is what the call names, so a private or
                // protected superclass member is reached there rather than on
                // a subclass that overrides it.
                let interface = self
                    .loader
                    .get(&declaring)
                    .is_some_and(|class| class.is_interface);
                let private =
                    self.mixin_method_is_private(&declaring, method, &accessor.descriptor);
                if is_static || private || interface {
                    self.execute_special_method(
                        &declaring,
                        method,
                        &accessor.descriptor,
                        &call_args,
                    )
                } else {
                    self.execute_method(&declaring, method, &accessor.descriptor, &call_args)
                }
            }
            AccessorKind::Getter { field } => {
                let target = self.mixin_accessor_target(accessor, args);
                let Some((owner, field_descriptor, is_static)) =
                    self.mixin_target_field(&target, field)
                else {
                    return Err(self.mixin_accessor_missing(field, &target));
                };
                if accessor.is_static != is_static {
                    return Err(self.mixin_accessor_mismatch(field, is_static, accessor.is_static));
                }
                if is_static {
                    // Reading a static field is an active use of the class that
                    // declares it: a JVM runs its initializer first, and that
                    // initializer is what assigns the field. This is `getstatic`
                    // without the instruction, so it initializes the same classes
                    // a `getstatic` would (the target as named, and the owner that
                    // declares the field) and skips the same synthetic ones.
                    for subject in [&target, &owner] {
                        if self.initializing.iter().any(|name| name == subject) {
                            continue;
                        }
                        if has_synthetic_statics(subject) || runtime_supplied_state(subject) {
                            continue;
                        }
                        self.ensure_initialized(subject)?;
                    }
                    return Ok(self.resolve_static_field(&owner, field));
                }
                // Mixin's field bridge starts with `aload 0`; an instance
                // accessor always has a receiver.
                let Some(id) = args.first().and_then(Value::as_obj) else {
                    return Err(self.mixin_accessor_null_receiver(field));
                };
                match self.heap.get(id) {
                    Some(JavaObject::Instance { fields, .. }) => Ok(fields
                        .get(field)
                        .cloned()
                        // A field nothing has written reads as its type's
                        // zero, as `getfield` does.
                        .unwrap_or_else(|| default_for_descriptor(&field_descriptor))),
                    // A host stand-in keeps its fields outside the heap map;
                    // ask the same reader `getfield` uses.
                    Some(_) => Ok(self.native_field(id, field)),
                    None => Err(self.mixin_accessor_missing(field, &target)),
                }
            }
            AccessorKind::Setter { field } => {
                let target = self.mixin_accessor_target(accessor, args);
                let Some((owner, _, is_static)) = self.mixin_target_field(&target, field) else {
                    return Err(self.mixin_accessor_missing(field, &target));
                };
                if accessor.is_static != is_static {
                    return Err(self.mixin_accessor_mismatch(field, is_static, accessor.is_static));
                }
                if is_static {
                    if !has_synthetic_statics(&owner) && !runtime_supplied_state(&owner) {
                        self.ensure_initialized(&owner)?;
                    }
                    // The value is the accessor's only argument: a static
                    // setter has no receiver in front of it.
                    let value = args.first().cloned().unwrap_or(Value::NULL);
                    // `putstatic` writes the declaring class's own slot (an
                    // inherited field belongs to the class that declares it),
                    // and a placeholder handed out before the write must not
                    // survive it.
                    if let Some(class) = self.loader.get_mut(&owner) {
                        class.static_fields.insert(field.clone(), value);
                    }
                    self.static_singletons
                        .remove(&(owner.clone(), field.clone()));
                    return Ok(Value::NULL);
                }
                // The receiver is `args[0]` and the value `args[1]`; Mixin's
                // setter bridge is `aload 0; ...; putfield`.
                let Some(id) = args.first().and_then(Value::as_obj) else {
                    return Err(self.mixin_accessor_null_receiver(field));
                };
                let value = args.get(1).cloned().unwrap_or(Value::NULL);
                if let Some(JavaObject::Instance { fields, .. }) = self.heap.get_mut(id) {
                    fields.insert(field.clone(), value);
                } else {
                    // A host stand-in keeps its state in the host object, not
                    // in a heap field map, and `putfield` on one is equally a
                    // no-op: say so rather than report a write that did not
                    // happen (`putfield`'s `native_field` reader has no setter
                    // counterpart to call).
                    let class = self
                        .heap
                        .get(id)
                        .map(|object| object.class_name().to_string())
                        .unwrap_or_default();
                    log::debug!(
                        "the mixin setter for {field} on {class} writes a host object's state, \
                         which the runtime stores outside the heap; the write was not applied"
                    );
                }
                Ok(Value::NULL)
            }
        }
    }

    /// The target class of an accessor for a given call: the first class the
    /// mixin names that the receiver is an instance of, else the first named.
    /// A static accessor has no receiver, but an explicit multi-target mixin
    /// still resolves against each class it names.
    fn mixin_accessor_target(
        &mut self,
        accessor: &crate::mixin::MixinAccessor,
        args: &[Value],
    ) -> String {
        let actual = if accessor.is_static {
            None
        } else {
            args.first()
                .and_then(Value::as_obj)
                .and_then(|id| match self.heap.get(id) {
                    Some(JavaObject::Instance { class_name, .. }) => Some(class_name.clone()),
                    _ => None,
                })
        };
        if let Some(actual) = actual {
            if let Some(target) = accessor
                .targets
                .iter()
                .find(|target| self.class_is_assignable_to(&actual, target))
            {
                return target.clone();
            }
        }
        accessor.target_class.clone()
    }

    /// The class declaring `method` in `target`'s hierarchy and whether it is
    /// static, or `None` when no class declares it. Mixin resolves an accessor
    /// target through the target class's superclasses, which is how an
    /// accessor reaches a member the target class only inherits.
    fn mixin_target_method(
        &mut self,
        target: &str,
        method: &str,
        descriptor: &str,
    ) -> Option<(String, bool)> {
        let mut queue = std::collections::VecDeque::from([target.to_string()]);
        let mut visited = std::collections::HashSet::new();
        while let Some(current) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            let _ = self.loader.get_or_load(&current);
            let Some(class) = self.loader.get(&current) else {
                continue;
            };
            if let Some(found) = class
                .methods
                .get(&(method.to_string(), descriptor.to_string()))
            {
                return Some((current, found.is_static));
            }
            if let Some(super_class) = &class.super_class {
                queue.push_back(super_class.clone());
            }
            for interface in &class.interfaces {
                queue.push_back(interface.clone());
            }
        }
        None
    }

    /// The class declaring `field` in `target`'s hierarchy, its descriptor and
    /// whether it is static, or `None` when no class declares it.
    fn mixin_target_field(&mut self, target: &str, field: &str) -> Option<(String, String, bool)> {
        let mut queue = std::collections::VecDeque::from([target.to_string()]);
        let mut visited = std::collections::HashSet::new();
        while let Some(current) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            let _ = self.loader.get_or_load(&current);
            let Some(class) = self.loader.get(&current) else {
                continue;
            };
            if let Some(found) = class.fields.get(field) {
                return Some((current, found.descriptor.clone(), found.is_static));
            }
            if let Some(super_class) = &class.super_class {
                queue.push_back(super_class.clone());
            }
            for interface in &class.interfaces {
                queue.push_back(interface.clone());
            }
        }
        None
    }

    /// Whether the method `declaring` exposes is private: Mixin's bridge then
    /// uses `invokespecial` rather than `invokevirtual`.
    fn mixin_method_is_private(&self, declaring: &str, method: &str, descriptor: &str) -> bool {
        const ACC_PRIVATE: u16 = 0x0002;
        self.loader.get(declaring).is_some_and(|class| {
            class
                .methods
                .get(&(method.to_string(), descriptor.to_string()))
                .is_some_and(|found| found.access & ACC_PRIVATE != 0)
        })
    }

    /// An accessor whose target member does not exist. Mixin rejects such a
    /// mixin at load time (`InvalidAccessorException`); the runtime applied it,
    /// so the call fails here instead of answering a default.
    fn mixin_accessor_missing(&self, member: &str, target: &str) -> Error {
        Error::new(
            "interpreter",
            format!(
                "the mixin accessor's target {}.{member} does not exist (Mixin rejects this \
                 mixin at load time)",
                target.replace('/', ".")
            ),
        )
    }

    /// An accessor whose static-ness differs from its target member's. Mixin
    /// rejects this too (`AccessorGenerator.checkModifiers`), and calling it
    /// either way passes the receiver as an argument or drops it.
    fn mixin_accessor_mismatch(&self, member: &str, target: bool, accessor: bool) -> Error {
        Error::new(
            "interpreter",
            format!(
                "the mixin accessor for {member} is {} but its target is {} (Mixin rejects this \
                 mixin at load time)",
                if accessor {
                    "static"
                } else {
                    "an instance member"
                },
                if target { "static" } else { "not static" }
            ),
        )
    }

    /// An instance accessor called with no receiver object.
    fn mixin_accessor_null_receiver(&self, member: &str) -> Error {
        Error::new(
            "interpreter",
            format!(
                "java.lang.NullPointerException: the mixin accessor for {member} has no receiver"
            ),
        )
    }

    /// A call to an interface method a mixin added to the receiver's class runs
    /// the mixin's bytecode with the receiver as `this`. A receiver whose own
    /// class (or a superclass) implements the method keeps that implementation.
    fn redirect_mixin_interface_method(
        &mut self,
        class_name: &str,
        method_name: &str,
        descriptor: &str,
        args: &[Value],
    ) -> Option<Result<Value>> {
        if self.mixin_interface_methods.is_empty() {
            return None;
        }
        let candidates = self
            .mixin_interface_methods
            .get(&(
                class_name.to_string(),
                method_name.to_string(),
                descriptor.to_string(),
            ))?
            .clone();
        let receiver = match self.heap.get(args.first()?.as_obj()?)? {
            JavaObject::Instance { class_name, .. } => class_name.clone(),
            _ => return None,
        };
        let mut current = Some(receiver.clone());
        while let Some(name) = current {
            let _ = self.loader.get_or_load(&name);
            let Some(class) = self.loader.get(&name) else {
                break;
            };
            if class
                .methods
                .get(&(method_name.to_string(), descriptor.to_string()))
                .is_some_and(|method| method.code.is_some())
            {
                return None;
            }
            current = class.super_class.clone();
        }
        let added = candidates
            .into_iter()
            .find(|added| self.class_is_assignable_to(&receiver, &added.target_class))?;
        Some(self.execute_special_method(&added.mixin_class, method_name, descriptor, args))
    }

    fn execute_method_inner(
        &mut self,
        class_name: &str,
        method_name: &str,
        descriptor: &str,
        args: &[Value],
        virtual_dispatch: bool,
    ) -> Result<Value> {
        log::trace!("Execute: {class_name}.{method_name}{descriptor}");

        // A mixin menu callback runs against host-synthesized layout objects;
        // the only layout operation that matters is adding a widget, which is
        // recorded as a menu contribution.
        if self.injection_depth > 0
            && args.len() >= 2
            && matches!(
                method_name,
                "addChild"
                    | "addRenderableWidget"
                    | "addWidget"
                    | "addDrawableChild"
                    | "addRenderableOnly"
            )
        {
            self.record_menu_child(method_name, args);
            return Ok(args[1].clone());
        }

        // If this invocation is on a Lambda object, unpack and delegate to the lambda
        // target. A static method that merely takes a lambda as its first argument
        // (`Intrinsics.checkNotNullParameter(lambda, name)`) must not run it.
        if let Some(first_arg) = args.first() {
            if let Some(this_id) = first_arg.as_obj() {
                // The heap check comes first: it is cheap, and most calls have
                // no lambda receiver, so the method lookup is rarely needed.
                let implements = match self.heap.get(this_id) {
                    Some(JavaObject::Lambda {
                        interface_method, ..
                    }) => Some(interface_method == method_name),
                    _ => None,
                };
                if let Some(implements) = implements
                    && !matches!(method_name, "getClass" | "toString" | "hashCode" | "equals")
                    && !self.is_static_method(class_name, method_name, descriptor)
                    // A method other than the one the lambda implements runs as
                    // itself when it has a body: a default method
                    // (`Comparator.thenComparing`) or another lambda's
                    // implementation taking this lambda as its first argument.
                    && (implements || !self.has_concrete_method(class_name, method_name, descriptor))
                {
                    if let Some(JavaObject::Lambda {
                        target_class,
                        target_method,
                        target_desc,
                        captured,
                        ..
                    }) = self.heap.get(this_id)
                    {
                        let t_class = target_class.clone();
                        let t_method = target_method.clone();
                        let t_desc = target_desc.clone();
                        let mut all_args = captured.clone();
                        for a in &args[1..] {
                            all_args.push(a.clone());
                        }
                        // A constructor reference (`Foo::new`) makes the object
                        // the constructor then initializes.
                        if t_method == "<init>" {
                            self.ensure_initialized(&t_class)?;
                            let id = self.alloc_uninitialized(&t_class);
                            let this = Value::Object(Some(id));
                            all_args.insert(0, this.clone());
                            self.execute_method(&t_class, "<init>", &t_desc, &all_args)?;
                            return Ok(this);
                        }
                        return self.execute_method(&t_class, &t_method, &t_desc, &all_args);
                    }
                }
            }
        }

        if let Some(value) = self.reflect_call(class_name, method_name, args) {
            return Ok(value);
        }

        // `Appendable.append(CharSequence)` of a library's own `CharSequence`
        // (Gson's `AppendableWriter` passes a `CurrentWrite`) appends the text of
        // its `toString()`, not a placeholder.
        if method_name == "append"
            && matches!(
                class_name,
                "java/lang/Appendable" | "java/lang/StringBuilder" | "java/lang/StringBuffer"
            )
            && args.len() >= 2
            && descriptor.contains("Ljava/lang/CharSequence;")
        {
            let is_library_instance = args[1]
                .as_obj()
                .and_then(|id| self.heap.get(id))
                .is_some_and(|o| matches!(o, JavaObject::Instance { class_name, .. } if !class_name.starts_with("java/")));
            if is_library_instance {
                let text = self.concat_text(&args[1]);
                let mut new_args = args.to_vec();
                new_args[1] = Value::Object(Some(self.heap.alloc_string(text)));
                return self.execute_method_inner(
                    class_name,
                    method_name,
                    descriptor,
                    &new_args,
                    virtual_dispatch,
                );
            }
        }

        // `String.valueOf(Object)` and `StringBuilder.append(Object)` call the
        // object's own `toString()`.
        if method_name == "valueOf"
            && class_name == "java/lang/String"
            && descriptor == "(Ljava/lang/Object;)Ljava/lang/String;"
            && args.len() == 1
            && !args[0].is_null()
        {
            let text = self.concat_text(&args[0]);
            return Ok(Value::Object(Some(self.heap.alloc_string(text))));
        }

        // `Event.invoker()` of an event made by `EventFactory.createArrayBacked`
        // is the combined callback its factory builds from the listeners
        // (for a Fabric event that is "all listeners, short-circuiting"), so a
        // call with no listener registered still gets the factory's empty
        // result.
        if method_name == "invoker"
            && args.len() == 1
            && matches!(
                class_name,
                "net/fabricmc/fabric/api/event/Event"
                    | "net/fabricmc/fabric/impl/base/event/ArrayBackedEvent"
            )
        {
            let parts = args[0].as_obj().and_then(|id| match self.heap.get(id) {
                Some(JavaObject::Instance { fields, .. }) => Some((
                    fields.get("invokerFactory").cloned(),
                    fields.get("listeners").cloned(),
                )),
                _ => None,
            });
            if let Some((Some(factory), Some(listeners))) = parts {
                if !factory.is_null() && !listeners.is_null() {
                    return self.execute_method(
                        "java/util/function/Function",
                        "apply",
                        "(Ljava/lang/Object;)Ljava/lang/Object;",
                        &[factory, listeners],
                    );
                }
            }
        }

        // A task given to the server executor while the server is already
        // running a task (an event dispatch is one) is queued, not run inline:
        // the dispatcher runs the queue when the event returns.
        if class_name == "net/minecraft/server/MinecraftServer"
            && matches!(method_name, "execute" | "submit" | "tell" | "submitAsync")
            && args.len() >= 2
        {
            self.server_tasks.push_back(args[1].clone());
            return Ok(Value::NULL);
        }

        // The interpreter has no threads. Coroutines launched on a pooled
        // dispatcher run on the dispatching thread instead, through the
        // library's own Unconfined dispatcher: they run eagerly until their
        // first real suspension.
        if class_name == "kotlinx/coroutines/Dispatchers"
            && matches!(method_name, "getDefault" | "getIO")
        {
            return self.execute_method(
                "kotlinx/coroutines/Dispatchers",
                "getUnconfined",
                "()Lkotlinx/coroutines/CoroutineDispatcher;",
                &[],
            );
        }

        // The interpreter has no Java stack frames, and a JVM is allowed to
        // report none: `walk`'s function receives an empty stream, `forEach`
        // runs nothing; `getCallerClass` reads the interpreter's frames. Log4j's
        // `StackLocator` and anything else that only wants "who called me"
        // then completes instead of reading a null the native walk left.
        // `Reflection.getCallerClass()`: the class of the frame that called
        // the method asking (`ClassLoader.registerAsParallelCapable` asks for
        // the loader class whose `<clinit>` called it).
        if class_name == "jdk/internal/reflect/Reflection"
            && method_name == "getCallerClass"
            && args.is_empty()
        {
            return Ok(self.caller_class());
        }

        if class_name == "java/lang/StackWalker" && !args.is_empty() {
            match method_name {
                "walk" if args.len() >= 2 => {
                    let stream =
                        Value::Object(Some(self.heap.alloc(JavaObject::Stream(Vec::new()))));
                    return self.execute_method(
                        "java/util/function/Function",
                        "apply",
                        "(Ljava/lang/Object;)Ljava/lang/Object;",
                        &[args[1].clone(), stream],
                    );
                }
                "forEach" if args.len() >= 2 => return Ok(Value::NULL),
                "getCallerClass" => return Ok(self.caller_class()),
                _ => {}
            }
        }

        // Optional combinators run a modifier's own lambda. A heap builtin
        // cannot call back into the interpreter, so `filter`, `map` and
        // `ifPresent` are handled here. ContainerEventHandler.mouseScrolled
        // routes the wheel through Optional.filter: without this the Fabric
        // selection lists never scroll.
        if class_name == "java/util/Optional" && !args.is_empty() {
            let value = args[0].as_obj().and_then(|id| match self.heap.get(id) {
                Some(JavaObject::Instance { fields, .. }) => fields.get("value").cloned(),
                _ => None,
            });
            let value = value.filter(|value| !value.is_null());
            let empty = |heap: &mut Heap| {
                Value::Object(Some(heap.alloc(JavaObject::Instance {
                    class_name: "java/util/Optional".into(),
                    fields: HashMap::new(),
                })))
            };
            match method_name {
                "filter" if args.len() >= 2 => {
                    let Some(value) = value else {
                        return Ok(empty(&mut self.heap));
                    };
                    let keep = self
                        .execute_method(
                            "java/util/function/Predicate",
                            "test",
                            "(Ljava/lang/Object;)Z",
                            &[args[1].clone(), value],
                        )?
                        .as_bool();
                    if keep {
                        return Ok(args[0].clone());
                    }
                    return Ok(empty(&mut self.heap));
                }
                "map" if args.len() >= 2 => {
                    let Some(value) = value else {
                        return Ok(empty(&mut self.heap));
                    };
                    let mapped = self.execute_method(
                        "java/util/function/Function",
                        "apply",
                        "(Ljava/lang/Object;)Ljava/lang/Object;",
                        &[args[1].clone(), value],
                    )?;
                    if mapped.is_null() {
                        return Ok(empty(&mut self.heap));
                    }
                    let mut fields = HashMap::new();
                    fields.insert("value".into(), mapped);
                    return Ok(Value::Object(Some(self.heap.alloc(JavaObject::Instance {
                        class_name: "java/util/Optional".into(),
                        fields,
                    }))));
                }
                "ifPresent" if args.len() >= 2 => {
                    if let Some(value) = value {
                        self.execute_method(
                            "java/util/function/Consumer",
                            "accept",
                            "(Ljava/lang/Object;)V",
                            &[args[1].clone(), value],
                        )?;
                    }
                    return Ok(Value::NULL);
                }
                _ => {}
            }
        }

        // Map operations that call back into Java.
        if matches!(
            class_name,
            "java/util/Map"
                | "java/util/HashMap"
                | "java/util/LinkedHashMap"
                | "java/util/TreeMap"
                | "java/util/concurrent/ConcurrentHashMap"
                | "java/util/concurrent/ConcurrentMap"
        ) && matches!(
            method_name,
            "computeIfAbsent" | "computeIfPresent" | "compute" | "merge" | "forEach"
        ) && args.len() >= 2
        {
            if let Some(map_id) = args[0].as_obj() {
                if let Some(JavaObject::Map(entries)) = self.heap.get(map_id) {
                    let entries = entries.clone();
                    let existing = |interp: &Self, key: &Value| {
                        let pos = crate::builtins::map_key_position(&interp.heap, map_id, key)?;
                        match interp.heap.get(map_id) {
                            Some(JavaObject::Map(entries)) => Some(entries[pos].1.clone()),
                            _ => None,
                        }
                        .filter(|v| !v.is_null())
                    };
                    let store = |interp: &mut Self, key: Value, value: Value| {
                        let pos = crate::builtins::map_key_position(&interp.heap, map_id, &key);
                        if let Some(JavaObject::Map(entries)) = interp.heap.get_mut(map_id) {
                            match pos {
                                Some(pos) => entries[pos].1 = value,
                                None => entries.push((key, value)),
                            }
                        }
                    };
                    let remove = |interp: &mut Self, key: &Value| {
                        let pos = crate::builtins::map_key_position(&interp.heap, map_id, key);
                        if let (Some(pos), Some(JavaObject::Map(entries))) =
                            (pos, interp.heap.get_mut(map_id))
                        {
                            entries.remove(pos);
                        }
                    };
                    const FUNCTION: &str = "(Ljava/lang/Object;)Ljava/lang/Object;";
                    const BIFUNCTION: &str =
                        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;";
                    match method_name {
                        "computeIfAbsent" if args.len() >= 3 => {
                            if let Some(value) = existing(self, &args[1]) {
                                return Ok(value);
                            }
                            let value = self.execute_method(
                                "java/util/function/Function",
                                "apply",
                                FUNCTION,
                                &[args[2].clone(), args[1].clone()],
                            )?;
                            if !value.is_null() {
                                store(self, args[1].clone(), value.clone());
                            }
                            return Ok(value);
                        }
                        "computeIfPresent" | "compute" if args.len() >= 3 => {
                            let old = existing(self, &args[1]);
                            if method_name == "computeIfPresent" && old.is_none() {
                                return Ok(Value::NULL);
                            }
                            let value = self.execute_method(
                                "java/util/function/BiFunction",
                                "apply",
                                BIFUNCTION,
                                &[args[2].clone(), args[1].clone(), old.unwrap_or(Value::NULL)],
                            )?;
                            if value.is_null() {
                                remove(self, &args[1]);
                            } else {
                                store(self, args[1].clone(), value.clone());
                            }
                            return Ok(value);
                        }
                        "merge" if args.len() >= 4 => {
                            let value = match existing(self, &args[1]) {
                                Some(old) => self.execute_method(
                                    "java/util/function/BiFunction",
                                    "apply",
                                    BIFUNCTION,
                                    &[args[3].clone(), old, args[2].clone()],
                                )?,
                                None => args[2].clone(),
                            };
                            if value.is_null() {
                                remove(self, &args[1]);
                            } else {
                                store(self, args[1].clone(), value.clone());
                            }
                            return Ok(value);
                        }
                        "forEach" => {
                            for (key, value) in entries {
                                self.execute_method(
                                    "java/util/function/BiConsumer",
                                    "accept",
                                    "(Ljava/lang/Object;Ljava/lang/Object;)V",
                                    &[args[1].clone(), key, value],
                                )?;
                            }
                            return Ok(Value::NULL);
                        }
                        _ => {}
                    }
                }
            }
        }

        // Handle forEach on collections (List, Set, Array)
        if method_name == "forEach" && args.len() >= 2 {
            let target_obj = args[0].as_obj();
            let consumer = args[1].clone();
            let elements: Option<Vec<Value>> = target_obj.and_then(|id| match self.heap.get(id) {
                Some(JavaObject::List(items)) => Some(items.clone()),
                Some(JavaObject::Set(items)) => Some(items.clone()),
                Some(JavaObject::Array { elements, .. }) => Some(elements.clone()),
                _ => None,
            });
            if let Some(items) = elements {
                for item in items {
                    let _ = self.execute_method(
                        "java/util/function/Consumer",
                        "accept",
                        "(Ljava/lang/Object;)V",
                        &[consumer.clone(), item],
                    );
                }
                return Ok(Value::NULL);
            }
        }

        // Java streams: keep the common lazy-looking operations eager. Fabric and
        // Minecraft GUI code builds lists with filter/map/collect; dropping them
        // silently empties screens and config models.
        if matches!(
            class_name,
            "java/util/stream/Stream"
                | "java/util/stream/IntStream"
                | "java/util/stream/LongStream"
                | "java/util/stream/DoubleStream"
        ) && !args.is_empty()
        {
            let stream_id = args[0].as_obj();
            let items = stream_id.and_then(|id| match self.heap.get(id) {
                Some(JavaObject::Stream(items)) => Some(items.clone()),
                _ => None,
            });
            if let Some(items) = items {
                match method_name {
                    "filter" | "anyMatch" | "allMatch" | "noneMatch" => {
                        let predicate = args.get(1).cloned().unwrap_or(Value::NULL);
                        let mut kept = Vec::new();
                        let (mut any, mut all) = (false, true);
                        for item in items {
                            let result = self.execute_method(
                                "java/util/function/Predicate",
                                "test",
                                "(Ljava/lang/Object;)Z",
                                &[predicate.clone(), item.clone()],
                            )?;
                            let pass = result.as_bool();
                            any |= pass;
                            all &= pass;
                            if method_name == "filter" && pass {
                                kept.push(item);
                            }
                        }
                        return Ok(match method_name {
                            "filter" => {
                                Value::Object(Some(self.heap.alloc(JavaObject::Stream(kept))))
                            }
                            "anyMatch" => Value::from_bool(any),
                            "allMatch" => Value::from_bool(all),
                            _ => Value::from_bool(!any),
                        });
                    }
                    "boxed" | "asLongStream" | "asDoubleStream" | "parallel" | "sequential" => {
                        return Ok(args[0].clone());
                    }
                    "sum" => {
                        let long = items.iter().any(|v| matches!(v, Value::Long(_)));
                        let double = items.iter().any(|v| matches!(v, Value::Double(_)));
                        return Ok(if double {
                            Value::Double(items.iter().map(|v| v.as_double()).sum())
                        } else if long {
                            Value::Long(items.iter().map(|v| v.as_long()).sum())
                        } else {
                            Value::Int(items.iter().map(|v| v.as_int()).sum())
                        });
                    }
                    "toArray" if args.len() == 1 => {
                        return Ok(Value::Object(Some(self.heap.alloc(JavaObject::Array {
                            elem_type: "int".into(),
                            elements: items,
                        }))));
                    }
                    "toArray" if args.len() == 2 => {
                        // `toArray(IntFunction<A[]>)`: the generator makes the
                        // typed array, the stream's elements fill it.
                        let array = self.execute_method(
                            "java/util/function/IntFunction",
                            "apply",
                            "(I)Ljava/lang/Object;",
                            &[args[1].clone(), Value::Int(items.len() as i32)],
                        )?;
                        if let Some(id) = array.as_obj()
                            && let Some(JavaObject::Array { elements, .. }) = self.heap.get_mut(id)
                        {
                            *elements = items;
                            return Ok(array);
                        }
                    }
                    "map" | "mapToObj" | "mapToInt" | "mapToLong" | "mapToDouble" => {
                        let function = args.get(1).cloned().unwrap_or(Value::NULL);
                        let mut mapped = Vec::new();
                        for item in items {
                            mapped.push(self.execute_method(
                                "java/util/function/Function",
                                "apply",
                                "(Ljava/lang/Object;)Ljava/lang/Object;",
                                &[function.clone(), item],
                            )?);
                        }
                        return Ok(Value::Object(Some(
                            self.heap.alloc(JavaObject::Stream(mapped)),
                        )));
                    }
                    "reduce" if args.len() == 2 || args.len() == 3 => {
                        // `reduce(op)` is an Optional; `reduce(identity, op)` the value.
                        let (op, mut acc, rest) = if args.len() == 2 {
                            let mut it = items.into_iter();
                            (args[1].clone(), it.next(), it.collect::<Vec<_>>())
                        } else {
                            (args[2].clone(), Some(args[1].clone()), items)
                        };
                        for item in rest {
                            let current = acc.take().unwrap_or(Value::NULL);
                            acc = Some(self.execute_method(
                                "java/util/function/BinaryOperator",
                                "apply",
                                "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                                &[op.clone(), current, item],
                            )?);
                        }
                        if args.len() == 3 {
                            return Ok(acc.unwrap_or(Value::NULL));
                        }
                        let mut fields = HashMap::new();
                        if let Some(value) = acc {
                            fields.insert("value".to_string(), value);
                        }
                        return Ok(Value::Object(Some(self.heap.alloc(JavaObject::Instance {
                            class_name: "java/util/Optional".into(),
                            fields,
                        }))));
                    }
                    "sorted" => {
                        let comparator = args.get(1).cloned().unwrap_or(Value::NULL);
                        let mut sorted = items;
                        // Insertion sort: the comparator may be interpreted Java, so
                        // it cannot run inside a Rust `sort_by` closure that borrows self.
                        for i in 1..sorted.len() {
                            let mut j = i;
                            while j > 0 {
                                let order = if comparator.is_null() {
                                    natural_order(&sorted[j - 1], &sorted[j])
                                } else {
                                    self.execute_method(
                                        "java/util/Comparator",
                                        "compare",
                                        "(Ljava/lang/Object;Ljava/lang/Object;)I",
                                        &[
                                            comparator.clone(),
                                            sorted[j - 1].clone(),
                                            sorted[j].clone(),
                                        ],
                                    )?
                                    .as_int()
                                };
                                if order <= 0 {
                                    break;
                                }
                                sorted.swap(j - 1, j);
                                j -= 1;
                            }
                        }
                        return Ok(Value::Object(Some(
                            self.heap.alloc(JavaObject::Stream(sorted)),
                        )));
                    }
                    "distinct" => {
                        let mut unique: Vec<Value> = Vec::new();
                        for item in items {
                            if !unique.contains(&item) {
                                unique.push(item);
                            }
                        }
                        return Ok(Value::Object(Some(
                            self.heap.alloc(JavaObject::Stream(unique)),
                        )));
                    }
                    "forEach" => {
                        let consumer = args.get(1).cloned().unwrap_or(Value::NULL);
                        for item in items {
                            let _ = self.execute_method(
                                "java/util/function/Consumer",
                                "accept",
                                "(Ljava/lang/Object;)V",
                                &[consumer.clone(), item],
                            );
                        }
                        return Ok(Value::NULL);
                    }
                    "count" => return Ok(Value::Long(items.len() as i64)),
                    // `max`/`min(comparator)`: the first greatest (least)
                    // element, as `BinaryOperator.maxBy`/`minBy` reduce.
                    "max" | "min" if args.len() == 2 => {
                        let mut best: Option<Value> = None;
                        for item in items {
                            let Some(current) = best.clone() else {
                                best = Some(item);
                                continue;
                            };
                            let order = self
                                .execute_method(
                                    "java/util/Comparator",
                                    "compare",
                                    "(Ljava/lang/Object;Ljava/lang/Object;)I",
                                    &[args[1].clone(), current, item.clone()],
                                )?
                                .as_int();
                            if (method_name == "max" && order < 0)
                                || (method_name == "min" && order > 0)
                            {
                                best = Some(item);
                            }
                        }
                        let mut fields = HashMap::new();
                        if let Some(best) = best {
                            fields.insert("value".into(), best);
                        }
                        let id = self.heap.alloc(JavaObject::Instance {
                            class_name: "java/util/Optional".into(),
                            fields,
                        });
                        return Ok(Value::Object(Some(id)));
                    }
                    "toList" => {
                        return Ok(Value::Object(Some(self.heap.alloc_list(items))));
                    }
                    "findFirst" | "findAny" => {
                        let mut fields = HashMap::new();
                        if let Some(first) = items.first() {
                            fields.insert("value".into(), first.clone());
                        }
                        let id = self.heap.alloc(JavaObject::Instance {
                            class_name: "java/util/Optional".into(),
                            fields,
                        });
                        return Ok(Value::Object(Some(id)));
                    }
                    // `collect(supplier, accumulator, combiner)`: the sequential
                    // result container, which every element is accumulated into.
                    "collect" if args.len() == 4 => {
                        let container = self.execute_method(
                            "java/util/function/Supplier",
                            "get",
                            "()Ljava/lang/Object;",
                            &[args[1].clone()],
                        )?;
                        for item in items {
                            self.execute_method(
                                "java/util/function/BiConsumer",
                                "accept",
                                "(Ljava/lang/Object;Ljava/lang/Object;)V",
                                &[args[2].clone(), container.clone(), item],
                            )?;
                        }
                        return Ok(container);
                    }
                    // `collect(collector)`: what a sequential `ReferencePipeline`
                    // does with any `Collector` — the JDK's own `Collectors` or a
                    // library's `Collector.of` — so grouping, joining and
                    // downstream collectors run as their bytecode.
                    "collect" if args.len() == 2 => {
                        let collector = args[1].clone();
                        let part = |interp: &mut Self, name: &str, descriptor: &str| {
                            interp.execute_method(
                                "java/util/stream/Collector",
                                name,
                                descriptor,
                                std::slice::from_ref(&collector),
                            )
                        };
                        let supplier = part(self, "supplier", "()Ljava/util/function/Supplier;")?;
                        let accumulator =
                            part(self, "accumulator", "()Ljava/util/function/BiConsumer;")?;
                        let finisher = part(self, "finisher", "()Ljava/util/function/Function;")?;
                        let container = self.execute_method(
                            "java/util/function/Supplier",
                            "get",
                            "()Ljava/lang/Object;",
                            &[supplier],
                        )?;
                        for item in items {
                            self.execute_method(
                                "java/util/function/BiConsumer",
                                "accept",
                                "(Ljava/lang/Object;Ljava/lang/Object;)V",
                                &[accumulator.clone(), container.clone(), item],
                            )?;
                        }
                        // An `IDENTITY_FINISH` collector's finisher is the
                        // identity, so applying it always is what the JDK answers.
                        return self.execute_method(
                            "java/util/function/Function",
                            "apply",
                            "(Ljava/lang/Object;)Ljava/lang/Object;",
                            &[finisher, container],
                        );
                    }
                    _ => {}
                }
            }
        }

        if class_name == "java/lang/Thread" && (method_name == "start" || method_name == "start0") {
            return Ok(Value::NULL);
        }

        // `MethodHandles.lookup()` is a trusted lookup in the calling class, and
        // the JDK's own `Lookup` code reads `lookupClass` (its constructor is
        // the only thing that sets it), so the object carries one.
        if class_name == "java/lang/invoke/MethodHandles"
            && method_name == "lookup"
            && args.is_empty()
        {
            let caller = self.caller_class();
            let mut fields = HashMap::new();
            fields.insert("lookupClass".to_string(), caller);
            fields.insert("allowedModes".to_string(), Value::Int(95));
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: "java/lang/invoke/MethodHandles$Lookup".into(),
                fields,
            });
            return Ok(Value::Object(Some(id)));
        }

        // `ThreadLocal`: the interpreter runs one thread, so each
        // `ThreadLocal` object holds exactly the one value a JVM's per-thread
        // map would. `withInitial`'s supplier runs when nothing is set.
        // `CropBlock.getAgeProperty()` (and `Block`'s) answers
        // `BlockStateProperties.AGE_<max>`, the constant the block's state
        // definition was built with. Vanilla compares properties by identity, so
        // a freshly built property of the same name would not be the one the
        // state's `propertyKeys` holds.
        // `Level.getFluidState(pos)` / `BlockState.getFluidState()`: the blocks
        // the runtime models are not fluids, so the state is the empty one,
        // whose type is `Fluids.EMPTY` (so `== Fluids.EMPTY` holds).
        let state_fluid_state = method_name == "getFluidState"
            && args.len() == 1
            && matches!(
                class_name,
                "net/minecraft/world/level/block/state/BlockState"
                    | "net/minecraft/world/level/block/state/BlockBehaviour$BlockStateBase"
                    | "net/minecraft/world/level/block/state/StateHolder"
            );
        let level_fluid_state = method_name == "getFluidState"
            && matches!(
                class_name,
                "net/minecraft/world/level/Level"
                    | "net/minecraft/world/level/BlockGetter"
                    | "net/minecraft/server/level/ServerLevel"
                    | "net/minecraft/world/level/LevelReader"
            )
            && args.len() == 2
            && args[0].as_obj().is_some_and(|id| {
                self.heap
                    .get_native::<crate::minecraft::Level>(id)
                    .is_some()
            });
        if state_fluid_state || level_fluid_state {
            let empty =
                self.resolve_static_field("net/minecraft/world/level/material/Fluids", "EMPTY");
            let mut fields = HashMap::new();
            fields.insert("type".to_string(), empty);
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/world/level/material/FluidState".into(),
                fields,
            });
            return Ok(Value::Object(Some(id)));
        }

        // `ThreadLocal`: the interpreter runs one thread, so each `ThreadLocal`
        // holds exactly the one value a JVM's per-thread map would.
        // `withInitial`'s supplier runs when nothing is set.
        if class_name == "java/lang/ThreadLocal" {
            if method_name == "withInitial" && args.len() == 1 {
                let id = self.heap.alloc(JavaObject::Instance {
                    class_name: "java/lang/ThreadLocal".into(),
                    fields: HashMap::new(),
                });
                self.heap
                    .thread_local_initializers
                    .insert(id, args[0].clone());
                return Ok(Value::Object(Some(id)));
            }
            if let Some(this_id) = args.first().and_then(Value::as_obj) {
                match method_name {
                    "set" if args.len() == 2 => {
                        self.heap.thread_locals.insert(this_id, args[1].clone());
                        return Ok(Value::NULL);
                    }
                    "remove" => {
                        self.heap.thread_locals.remove(&this_id);
                        return Ok(Value::NULL);
                    }
                    "get" => {
                        if let Some(value) = self.heap.thread_locals.get(&this_id) {
                            return Ok(value.clone());
                        }
                        let initializer =
                            self.heap.thread_local_initializers.get(&this_id).cloned();
                        let value = match initializer {
                            Some(initializer) => self.execute_method(
                                "java/util/function/Supplier",
                                "get",
                                "()Ljava/lang/Object;",
                                std::slice::from_ref(&initializer),
                            )?,
                            // `ThreadLocal.initialValue()` of a subclass, or null.
                            None => self.execute_method(
                                "java/lang/ThreadLocal",
                                "initialValue",
                                "()Ljava/lang/Object;",
                                std::slice::from_ref(&args[0]),
                            )?,
                        };
                        self.heap.thread_locals.insert(this_id, value.clone());
                        return Ok(value);
                    }
                    _ => {}
                }
            }
        }

        // A block-state `Property` is a value vanilla compares by identity
        // (`StateHolder.valueIndex` uses `if_acmpne`): a block's state
        // definition and a `setValue` naming "the same" property must receive
        // one object. Vanilla builds them through `IntegerProperty.create(name,
        // …)`, so the factory is interned by class and name and every later call
        // answers the object the first one made.
        if method_name == "create" && is_property_class(class_name) && !self.interning_property {
            if let Some(name) = args
                .first()
                .and_then(Value::as_obj)
                .and_then(|id| self.heap.get_string(id))
                .map(str::to_string)
            {
                // A property's identity is its class, name and value set: two
                // `EnumProperty.create("axis", …)` calls with different values
                // are different properties, and reusing one for the other gives
                // it the wrong allowed values.
                let extra = args
                    .iter()
                    .skip(2)
                    .map(|v| format!("{v:?}"))
                    .collect::<Vec<_>>()
                    .join(",");
                let key = format!("property:{class_name}:{name}:{extra}");
                if let Some(existing) = self.heap.get_singleton(&key) {
                    return Ok(Value::Object(Some(existing)));
                }
                // The factory itself calls back into this method; the flag
                // keeps the intercept from recursing.
                self.interning_property = true;
                let made = self.execute_special_method(class_name, method_name, descriptor, args);
                self.interning_property = false;
                let made = made?;
                if let Some(id) = made.as_obj() {
                    self.heap.set_singleton(&key, id);
                }
                return Ok(made);
            }
        }

        // `Unsafe.ensureClassInitialized(Class)` runs that class's `<clinit>`;
        // `SharedSecrets` relies on it to install a lazily set access object.
        if matches!(class_name, "jdk/internal/misc/Unsafe" | "sun/misc/Unsafe")
            && matches!(
                method_name,
                "ensureClassInitialized" | "ensureClassInitialized0"
            )
        {
            let target =
                args.last()
                    .and_then(Value::as_obj)
                    .and_then(|id| match self.heap.get(id) {
                        Some(JavaObject::Class(name)) => Some(name.replace('.', "/")),
                        _ => None,
                    });
            if let Some(target) = target {
                self.ensure_initialized(&target)?;
            }
            return Ok(Value::NULL);
        }

        if class_name == "java/util/concurrent/FutureTask"
            && method_name == "awaitDone"
            && !args.is_empty()
        {
            let task_obj = args[0].clone();
            let _ =
                self.execute_method("java/util/concurrent/FutureTask", "run", "()V", &[task_obj]);
            return Ok(Value::Int(2));
        }

        if let Some(result) =
            self.redirect_mixin_accessor(class_name, method_name, descriptor, args)
        {
            return result;
        }
        if let Some(result) =
            self.redirect_mixin_interface_method(class_name, method_name, descriptor, args)
        {
            return result;
        }
        if let Some(result) = self.reflect_constructor(class_name, method_name, args) {
            return result;
        }

        // `ItemStack.getRarity()` reads the item's `DataComponents.RARITY`
        // component, which comes from the host's item data here. Resolving the
        // enum field (rather than allocating a look-alike) keeps reference
        // comparison with `Rarity.EPIC` in mod code working, which is how
        // selectors such as Iceberg's `!epic` choose a tooltip frame.
        if class_name == "net/minecraft/world/item/ItemStack" && method_name == "getRarity" {
            let rarity = args
                .first()
                .and_then(|value| value.as_obj())
                .and_then(|id| self.heap.get_native::<ItemStack>(id))
                .and_then(|stack| stack.rarity.clone());
            let field = match rarity.as_deref() {
                Some("uncommon") => "UNCOMMON",
                Some("rare") => "RARE",
                Some("epic") => "EPIC",
                _ => "COMMON",
            };
            let _ = self.ensure_initialized("net/minecraft/world/item/Rarity");
            return Ok(self.resolve_static_field("net/minecraft/world/item/Rarity", field));
        }

        // A `FormattedCharSequence` the runtime built carries plain text; its
        // `accept` feeds each codepoint to a `FormattedCharSink`, which is how
        // library code (Iceberg's `StringRecomposer`) reads a tooltip line.
        if class_name == "net/minecraft/util/FormattedCharSequence"
            && method_name == "accept"
            && args.len() >= 2
        {
            let text = crate::builtins::text_of_value(&self.heap, &args[0]);
            let style = self.resolve_static_field("net/minecraft/network/chat/Style", "EMPTY");
            for (index, character) in text.chars().enumerate() {
                let keep_going = self.execute_method(
                    "net/minecraft/util/FormattedCharSink",
                    "accept",
                    "(ILnet/minecraft/network/chat/Style;I)Z",
                    &[
                        args[1].clone(),
                        Value::Int(index as i32),
                        style.clone(),
                        Value::Int(character as i32),
                    ],
                )?;
                if !keep_going.as_bool() {
                    break;
                }
            }
            return Ok(Value::from_bool(true));
        }

        // `GuiGraphicsExtractor.pose()` is a real JOML `Matrix3x2fStack`, built
        // once per graphics object so its push/translate/rotate run as JOML
        // bytecode. The recorder reads its matrix when a draw is pushed.
        if class_name == "net/minecraft/client/gui/GuiGraphicsExtractor"
            && method_name == "pose"
            && let Some(graphics) = args.first().and_then(|v| v.as_obj())
            && self.heap.get_native::<GuiGraphics>(graphics).is_some()
        {
            let existing = self
                .heap
                .get_native::<GuiGraphics>(graphics)
                .and_then(|g| g.pose);
            let pose = match existing {
                Some(pose) => pose,
                None => {
                    const STACK: &str = "org/joml/Matrix3x2fStack";
                    self.ensure_initialized(STACK)?;
                    let id = self.alloc_uninitialized(STACK);
                    let object = Value::Object(Some(id));
                    self.execute_special_method(
                        STACK,
                        "<init>",
                        "(I)V",
                        &[object, Value::Int(64)],
                    )?;
                    if let Some(g) = self.heap.get_native_mut::<GuiGraphics>(graphics) {
                        g.pose = Some(id);
                    }
                    id
                }
            };
            return Ok(Value::Object(Some(pose)));
        }

        // `Objects.toString(Object)` and `StringBuilder.append(Object)` render
        // an object with its own `toString`. A runtime collection or a JDK
        // collection's bytecode renders the way the JDK does, so a list of
        // lists or a map of lists prints its elements, not their heap ids.
        let rendered;
        let args = match (class_name, method_name, descriptor) {
            ("java/util/Objects", "toString", "(Ljava/lang/Object;)Ljava/lang/String;")
                if self.renders_as_jdk_collection(&args[0]) =>
            {
                let text = self.concat_text(&args[0]);
                return Ok(Value::Object(Some(self.heap.alloc_string(text))));
            }
            (
                "java/lang/StringBuilder" | "java/lang/AbstractStringBuilder",
                "append",
                "(Ljava/lang/Object;)Ljava/lang/StringBuilder;"
                | "(Ljava/lang/Object;)Ljava/lang/AbstractStringBuilder;",
            ) if args.len() == 2 && self.renders_as_jdk_collection(&args[1]) => {
                let text = self.concat_text(&args[1]);
                rendered = [
                    args[0].clone(),
                    Value::Object(Some(self.heap.alloc_string(text))),
                ];
                &rendered[..]
            }
            _ => args,
        };

        // A heap-modelled list, set or map copies from a collection that is
        // real bytecode (a Guava `ImmutableList`, `Lists.cartesianProduct`):
        // read its elements through `Iterator` first.
        let adopted;
        let args = if collection_copy_call(class_name, method_name) {
            // `copyOf` is static; the others take the receiver first.
            adopted = self.adopt_collection_args(args, usize::from(method_name != "copyOf"))?;
            adopted.as_slice()
        } else {
            args
        };

        if let Some(value) = self.register_item(class_name, method_name, args) {
            return Ok(value);
        }
        if let Some(value) = self.register_block(class_name, method_name, args) {
            return Ok(value);
        }
        if let Some(value) = self.register_creative_tab(class_name, method_name, args) {
            return Ok(value);
        }

        // 1. Built-in shims
        match dispatch_builtin(
            &mut self.heap,
            class_name,
            method_name,
            descriptor,
            args,
            virtual_dispatch,
        ) {
            BuiltinResult::Handled(v) => {
                log::trace!("  Builtin returned: {:?}", v);
                return Ok(v);
            }
            BuiltinResult::NotHandled => {}
        }

        // 2. Find method in class, actual instance class, or superclasses/interfaces
        let mut queue = std::collections::VecDeque::new();
        // When the call site names an interface or superclass, the receiver's
        // own class hierarchy must win over that declaration's default method.
        // Queue the declared class only as a fallback once the receiver chain is
        // exhausted; otherwise `GuiEventListener.isMouseOver` (always false)
        // shadows `AbstractWidget.isMouseOver` for every widget.
        let mut declared_fallback = None;
        if virtual_dispatch && method_name != "<init>" {
            if let Some(first) = args.first() {
                if let Some(id) = first.as_obj() {
                    let actual = match self.heap.get(id) {
                        Some(JavaObject::Instance {
                            class_name: actual, ..
                        }) => Some(actual.as_str()),
                        // A runtime string is a `String` whatever interface the
                        // call site names: `CharSequence.subSequence` must run
                        // `String`'s implementation, not answer the abstract
                        // interface method with null.
                        Some(JavaObject::String(_)) => Some("java/lang/String"),
                        // The runtime's own list, set and map stand for a JDK
                        // collection: whatever the builtins do not answer runs
                        // as that collection's skeleton class (`toString`,
                        // `listIterator`, `equals`, `removeIf`, ...), whose
                        // bytecode is written against `size`/`get`/`iterator`/
                        // `entrySet`, which the builtins do answer.
                        Some(other) => collection_skeleton(other),
                        None => None,
                    };
                    if let Some(actual) = actual {
                        if actual != class_name {
                            queue.push_back(actual.to_string());
                            declared_fallback = Some(class_name.to_string());
                        }
                    }
                }
            }
        }
        if declared_fallback.is_none() {
            queue.push_back(class_name.to_string());
        }

        let mut visited = std::collections::HashSet::new();
        let (this_class, bsm, code) = 'lookup: loop {
            let Some(curr) = queue.pop_front() else {
                if let Some(fallback) = declared_fallback.take() {
                    queue.push_back(fallback);
                    continue;
                }
                if self.loader.get(class_name).is_none() {
                    self.note_missing(format!("class {class_name}"));
                } else {
                    self.note_missing(format!("{class_name}.{method_name}{descriptor}"));
                }
                log::debug!(
                    "Method not found in hierarchy of {class_name}: {method_name}{descriptor} (receiver {})",
                    self.describe_receiver(virtual_dispatch, args)
                );
                return Ok(Value::NULL);
            };

            if !visited.insert(curr.clone()) {
                continue;
            }

            let _ = self.loader.get_or_load(&curr);
            let Some(c) = self.loader.get(&curr) else {
                continue;
            };

            let method = c
                .methods
                .get(&(method_name.to_string(), descriptor.to_string()));

            if let Some(m) = method {
                if let Some(code) = &m.code {
                    break 'lookup (
                        c.this_class.clone(),
                        c.bootstrap_methods.clone(),
                        std::sync::Arc::clone(code),
                    );
                }
            }

            if let Some(super_name) = &c.super_class {
                queue.push_back(super_name.clone());
            }
            for iface in &c.interfaces {
                queue.push_back(iface.clone());
            }
        };

        self.interpret_code(&this_class, method_name, descriptor, &bsm, &code, args)
    }

    /// Record a widget a mixin callback added to a layout or screen.
    fn record_menu_child(&mut self, method_name: &str, args: &[Value]) {
        if !self.menu_injection || self.injection_depth == 0 {
            return;
        }
        if !matches!(
            method_name,
            "addChild"
                | "addRenderableWidget"
                | "addWidget"
                | "addDrawableChild"
                | "addRenderableOnly"
                | "add"
        ) {
            return;
        }
        let Some(id) = args.get(1).and_then(Value::as_obj) else {
            return;
        };
        if !self.menu_contributions.contains(&id) {
            self.menu_contributions.push(id);
        }
    }

    /// Whether the field a `getfield`/`putfield` names is really declared by a
    /// class the loader has: only then is a null receiver the JVM's fault. The
    /// runtime models some objects with natively stored fields (`Thread`'s
    /// `threadStatus`, `daemon`), and a null there is a model gap, not a throw.
    fn field_is_checkable(&mut self, class: &str, field: &str) -> bool {
        matches!(self.loader.get_or_load(class), Ok(Some(class)) if class.fields.contains_key(field))
    }

    /// Whether a `checkcast` of `value` to `target` can be decided. Only an
    /// object of real bytecode (an instance of a class the loader has, or a
    /// string) cast to a class the loader can find is checked. The runtime's
    /// stand-ins (native Minecraft objects, modelled collections, lambdas,
    /// synthesized constants) carry no real class identity, and an unknown
    /// target class cannot be compared with: those casts pass, as before.
    fn cast_is_checkable(&mut self, value: &Value, target: &str) -> bool {
        let Some(id) = value.as_obj() else {
            return false;
        };
        if target.starts_with('[') {
            return false;
        }
        let class = match self.heap.get(id) {
            Some(JavaObject::String(_)) => "java/lang/String".to_string(),
            Some(JavaObject::Instance { class_name, .. }) => class_name.clone(),
            _ => return false,
        };
        if !matches!(self.loader.get_or_load(&class), Ok(Some(_))) {
            return false;
        }
        matches!(self.loader.get_or_load(target), Ok(Some(_)))
    }

    /// The class `Blocks.<clinit>` constructs vanilla block `id` as: the
    /// constructor reference (`FireBlock::new`) it registers before storing the
    /// constant. Read from the bytecode once per id.
    fn vanilla_block_class(&mut self, id: &str) -> Option<String> {
        if let Some(known) = self.vanilla_block_classes.get(id) {
            return known.clone();
        }
        let field = id.strip_prefix("minecraft:").map(str::to_ascii_uppercase);
        let found = field.and_then(|field| {
            const BLOCKS: &str = "net/minecraft/world/level/block/Blocks";
            let _ = self.loader.get_or_load(BLOCKS);
            let class = self.loader.get(BLOCKS)?;
            let code = class
                .methods
                .get(&("<clinit>".to_string(), "()V".to_string()))?
                .code
                .clone()?;
            let bootstraps = class.bootstrap_methods.clone();
            let mut constructor: Option<String> = None;
            for (_, opcode) in &code.opcodes {
                match opcode {
                    Opcode::Invokedynamic { bootstrap_idx, .. } => {
                        if let Some(b) = bootstraps.get(*bootstrap_idx as usize) {
                            if b.target_method == "<init>" {
                                constructor = Some(b.target_class.clone());
                            }
                        }
                    }
                    Opcode::Putstatic {
                        class_name,
                        field_name,
                        ..
                    } if class_name == BLOCKS => {
                        if *field_name == field {
                            return constructor;
                        }
                        constructor = None;
                    }
                    _ => {}
                }
            }
            None
        });
        self.vanilla_block_classes
            .insert(id.to_string(), found.clone());
        found
    }

    /// The class of the frame below the innermost one, as a `Class`; null at
    /// the bottom of the stack.
    fn caller_class(&mut self) -> Value {
        // At the bottom of the stack there is no caller frame; a JVM always
        // has one (its launcher), and code calls `getCallerClass()` expecting a
        // class, so the innermost frame names itself.
        let depth = self.frame_depth;
        let Some(index) = depth
            .checked_sub(2)
            .or_else(|| depth.checked_sub(1))
            .filter(|index| *index < depth)
        else {
            return Value::NULL;
        };
        let class = self.frame_classes[index].clone();
        Value::Object(Some(self.heap.alloc_class(class)))
    }

    /// Interpret a code block with the given arguments as initial locals.
    fn interpret_code(
        &mut self,
        this_class: &str,
        method_name: &str,
        descriptor: &str,
        bootstrap_methods: &[BootstrapMethodInfo],
        code: &CodeInfo,
        args: &[Value],
    ) -> Result<Value> {
        // The buffers stay allocated between calls; only the depth moves.
        match self.frame_classes.get_mut(self.frame_depth) {
            Some(slot) => {
                slot.clear();
                slot.push_str(this_class);
            }
            None => self.frame_classes.push(this_class.to_string()),
        }
        match self.frame_methods.get_mut(self.frame_depth) {
            Some(slot) => {
                slot.clear();
                slot.push_str(method_name);
            }
            None => self.frame_methods.push(method_name.to_string()),
        }
        self.frame_depth += 1;
        let catch_mark = self.catch_sites.len();
        let result = self.interpret_frame(
            this_class,
            method_name,
            descriptor,
            bootstrap_methods,
            code,
            args,
        );
        self.catch_sites.truncate(catch_mark);
        self.frame_depth -= 1;
        result
    }

    fn interpret_frame(
        &mut self,
        this_class: &str,
        method_name: &str,
        descriptor: &str,
        bootstrap_methods: &[BootstrapMethodInfo],
        code: &CodeInfo,
        args: &[Value],
    ) -> Result<Value> {
        let mut locals = vec![Value::NULL; code.max_locals as usize];
        // JVM locals are typed slots: `long` and `double` arguments occupy two
        // consecutive slots. Advancing one slot per Rust value would store the
        // second parameter over the high half of the first and leave every
        // following argument in the wrong slot, so hit-tests and other
        // double-taking methods compare against zero.
        // The descriptor, not the Rust value's type, says how wide a parameter is:
        // a builtin or an untyped constant may hand a `double` parameter an Int.
        let params = parameter_kinds(descriptor);
        let skip = args.len().saturating_sub(params.len());
        let mut slot = 0usize;
        for (index, arg) in args.iter().enumerate() {
            let kind = index.checked_sub(skip).and_then(|i| params.get(i)).copied();
            let value = match (kind, arg) {
                (Some(b'D'), v) if !matches!(v, Value::Double(_)) => Value::Double(v.as_double()),
                (Some(b'J'), v) if !matches!(v, Value::Long(_)) => Value::Long(v.as_long()),
                (Some(b'F'), v) if !matches!(v, Value::Float(_)) => Value::Float(v.as_float()),
                _ => arg.clone(),
            };
            if slot < locals.len() {
                locals[slot] = value;
            }
            slot += if matches!(kind, Some(b'J' | b'D')) {
                2
            } else {
                1
            };
        }

        let mut stack: Vec<Value> = Vec::with_capacity(code.max_stack as usize);
        let mut pc = 0usize;
        self.run_code(
            this_class,
            method_name,
            descriptor,
            bootstrap_methods,
            code,
            &mut locals,
            &mut stack,
            &mut pc,
        )
    }

    /// The handler (byte offset) of the first exception-table entry covering
    /// `thrown_at` that catches `exception`.
    fn find_handler(
        &mut self,
        code: &CodeInfo,
        thrown_at: usize,
        exception: &Value,
    ) -> Option<usize> {
        for entry in &code.handlers {
            if thrown_at < entry.start || thrown_at >= entry.end {
                continue;
            }
            match &entry.catch_type {
                None => return Some(entry.handler),
                Some(class) => {
                    let _ = self.loader.get_or_load(class);
                    if self.is_instance_of(exception, class) {
                        return Some(entry.handler);
                    }
                }
            }
        }
        None
    }

    /// A `Throwable` of `class` with `message`, built by the JDK's own
    /// `(String)` constructor so `getMessage`, `getCause`, `toString` and
    /// `getClass().getSimpleName()` are the real class's answers.
    pub(crate) fn new_exception(&mut self, class: &str, message: &str) -> Value {
        let text = Value::Object(Some(self.heap.alloc_string(message)));
        let value = self.new_exception_with(class, "(Ljava/lang/String;)V", &[text.clone()]);
        // A class with no usable constructor still keeps its message where
        // `Throwable.getMessage` reads it.
        if let Some(JavaObject::Instance { fields, .. }) =
            value.as_obj().and_then(|id| self.heap.get_mut(id))
        {
            if !fields.contains_key("detailMessage") {
                fields.insert("detailMessage".to_string(), text);
            }
        }
        value
    }

    /// A `Throwable` of `class` built by the constructor `descriptor` with
    /// `ctor_args` (this is added in front). An exception the construction
    /// raises is dropped: the object exists either way.
    fn new_exception_with(&mut self, class: &str, descriptor: &str, ctor_args: &[Value]) -> Value {
        let saved = self.pending_exception.take();
        let outer = std::mem::replace(&mut self.building_exception, true);
        let _ = self.ensure_initialized(class);
        let id = self.alloc_uninitialized(class);
        let this = Value::Object(Some(id));
        let mut call = Vec::with_capacity(ctor_args.len() + 1);
        call.push(this.clone());
        call.extend_from_slice(ctor_args);
        let _ = self.execute_method(class, "<init>", descriptor, &call);
        self.building_exception = outer;
        self.pending_exception = saved;
        this
    }

    /// Route a thrown value: the first handler of this method that covers
    /// `thrown_at` and accepts it catches it (the pc to continue at, with the
    /// stack holding the exception). With no such handler it becomes the
    /// pending exception, which ends this frame and unwinds into the caller.
    fn throw_at(
        &mut self,
        code: &CodeInfo,
        thrown_at: usize,
        exception: Value,
        stack: &mut Vec<Value>,
    ) -> Option<usize> {
        match self.find_handler(code, thrown_at, &exception) {
            Some(handler) => {
                stack.clear();
                stack.push(exception);
                Some(find_target_pc(&code.opcodes, handler, 0))
            }
            None => {
                // The first exception in flight is the one that matters: while it
                // unwinds, anything raised later is a consequence, not the cause.
                if self.pending_exception.is_none() {
                    self.pending_exception = Some(exception);
                }
                None
            }
        }
    }

    /// `throw_at` for an exception the runtime raises itself.
    fn raise(
        &mut self,
        code: &CodeInfo,
        thrown_at: usize,
        class: &str,
        message: &str,
        stack: &mut Vec<Value>,
    ) -> Option<usize> {
        if !self.fault_is_caught(code, thrown_at, class) {
            // Nothing on the stack catches it. On a JVM the fault would end the
            // call; here the null, the stand-in or the missing value behind it
            // almost always comes from something the runtime does not model,
            // so the instruction keeps its lenient answer instead of ending
            // the entrypoint (see `docs/architecture/fabric-runtime.md`).
            return None;
        }
        let exception = self.new_exception(class, message);
        self.throw_at(code, thrown_at, exception, stack)
    }

    /// Whether a handler of this frame (at `at`) or of a caller's pending call
    /// site catches an exception of `class`. A `finally` (catch-all) handler
    /// rethrows, so it does not count as catching.
    fn fault_is_caught(&mut self, code: &CodeInfo, at: usize, class: &str) -> bool {
        let local: Vec<String> = code
            .handlers
            .iter()
            .filter(|entry| at >= entry.start && at < entry.end)
            .filter_map(|entry| entry.catch_type.clone())
            .collect();
        if local
            .iter()
            .any(|catch| self.class_is_assignable_to(class, catch))
        {
            return true;
        }
        // Innermost caller first. Leaving a class initializer turns anything
        // but an `Error` into `ExceptionInInitializerError`.
        let callers = self.catch_sites.clone();
        let mut class = class.to_string();
        for catch in callers.iter().rev() {
            if catch == CLINIT_BOUNDARY {
                if !self.class_is_assignable_to(&class, "java/lang/Error") {
                    class = "java/lang/ExceptionInInitializerError".to_string();
                }
                continue;
            }
            if self.class_is_assignable_to(&class, catch) {
                return true;
            }
        }
        false
    }

    /// Record the catch types of the handlers of `code` that cover the call at
    /// `at`, for the duration of that call. Returns the mark to restore.
    fn enter_call(&mut self, code: &CodeInfo, at: usize) -> usize {
        let mark = self.catch_sites.len();
        for entry in &code.handlers {
            if at >= entry.start && at < entry.end {
                if let Some(catch) = &entry.catch_type {
                    self.catch_sites.push(catch.clone());
                }
            }
        }
        mark
    }

    /// After a call returned: catch the exception it threw here when this frame
    /// has a handler for it, otherwise leave it pending for the caller.
    fn unwind_here(
        &mut self,
        code: &CodeInfo,
        call_at: usize,
        stack: &mut Vec<Value>,
    ) -> Option<usize> {
        let exception = self.pending_exception.take()?;
        self.throw_at(code, call_at, exception, stack)
    }

    /// `class: message` of an exception value, for logs and reports.
    pub fn describe_exception(&self, value: &Value) -> String {
        let Some(id) = value.as_obj() else {
            return "null".into();
        };
        let class = match self.heap.get(id) {
            Some(JavaObject::Instance { class_name, .. }) => class_name.clone(),
            Some(other) => other.class_name().to_string(),
            None => return "null".into(),
        };
        let message = match self.heap.get(id) {
            Some(JavaObject::Instance { fields, .. }) => fields
                .get("detailMessage")
                .and_then(Value::as_obj)
                .and_then(|id| self.heap.get_string(id))
                .map(str::to_string),
            _ => None,
        };
        match message {
            Some(message) => format!("{class}: {message}"),
            None => class.replace('/', "."),
        }
    }

    /// Take the exception that escaped the last top-level call, if any, as
    /// `class: message`. A boundary that calls into Java uses this to report
    /// the failure instead of treating the call as a success.
    pub fn take_escaped_exception(&mut self) -> Option<String> {
        let value = self.pending_exception.take()?;
        Some(self.describe_exception(&value))
    }

    /// Whether an exception is in flight.
    pub fn has_pending_exception(&self) -> bool {
        self.pending_exception.is_some()
    }

    /// Run `code` from `*pc_slot` until it returns or throws. `*pc_slot` always
    /// holds the index of the opcode being executed, so a caller can find the
    /// handler for an exception that escapes.
    fn run_code(
        &mut self,
        this_class: &str,
        method_name: &str,
        descriptor: &str,
        bootstrap_methods: &[BootstrapMethodInfo],
        code: &CodeInfo,
        locals: &mut Vec<Value>,
        stack: &mut Vec<Value>,
        pc_slot: &mut usize,
    ) -> Result<Value> {
        let mut pc = *pc_slot;
        let opcodes = &code.opcodes;
        let catch_mark = self.catch_sites.len();
        while pc < opcodes.len() {
            // The catch types covering the previous call site no longer apply.
            self.catch_sites.truncate(catch_mark);
            if let Some(budget) = &mut self.step_budget {
                if *budget == 0 {
                    let stack = if self.trace_calls && !self.call_stack.is_empty() {
                        let from = self.call_stack.len().saturating_sub(12);
                        format!(
                            "; depth {}; stack: {}",
                            self.call_depth,
                            self.call_stack[from..].join(" <- ")
                        )
                    } else {
                        String::new()
                    };
                    return Err(hfc_core::Error::new(
                        "interpreter",
                        format!(
                            "instruction budget exhausted in {}.{}{} at byte {}{stack}",
                            this_class, method_name, descriptor, opcodes[pc].0
                        ),
                    ));
                }
                *budget -= 1;
            }
            *pc_slot = pc;
            let (byte_offset, ref op) = opcodes[pc];
            pc += 1;
            // An instruction that runs other code (a call, or a class
            // initializer) makes this frame's covering handlers visible to it.
            if !code.handlers.is_empty()
                && matches!(
                    op,
                    Opcode::Invokestatic { .. }
                        | Opcode::Invokespecial { .. }
                        | Opcode::Invokevirtual { .. }
                        | Opcode::Invokeinterface { .. }
                        | Opcode::New(_)
                        | Opcode::Getstatic { .. }
                        | Opcode::Putstatic { .. }
                )
            {
                self.enter_call(code, byte_offset);
            }

            match op {
                Opcode::Nop => {}
                Opcode::AconstNull => stack.push(Value::NULL),
                Opcode::Iconst(i) => stack.push(Value::Int(*i)),
                Opcode::Fconst(f) => stack.push(Value::Float(*f)),
                Opcode::Lconst(l) => stack.push(Value::Long(*l)),
                Opcode::Dconst(d) => stack.push(Value::Double(*d)),
                Opcode::Bipush(b) => stack.push(Value::Int(*b as i32)),
                Opcode::Sipush(s) => stack.push(Value::Int(*s as i32)),

                Opcode::LdcInt(i) => stack.push(Value::Int(*i)),
                Opcode::LdcFloat(f) => stack.push(Value::Float(*f)),
                Opcode::LdcLong(l) => stack.push(Value::Long(*l)),
                Opcode::LdcDouble(d) => stack.push(Value::Double(*d)),
                Opcode::LdcString(s) => {
                    let id = self.heap.alloc_string(s.clone());
                    stack.push(Value::Object(Some(id)));
                }
                Opcode::LdcClass(c) => {
                    let id = self.heap.alloc_class(c.clone());
                    stack.push(Value::Object(Some(id)));
                }

                Opcode::Aload(i)
                | Opcode::Iload(i)
                | Opcode::Fload(i)
                | Opcode::Lload(i)
                | Opcode::Dload(i) => {
                    let val = locals.get(*i as usize).cloned().unwrap_or(Value::NULL);
                    stack.push(val);
                }
                Opcode::Astore(i)
                | Opcode::Istore(i)
                | Opcode::Fstore(i)
                | Opcode::Lstore(i)
                | Opcode::Dstore(i) => {
                    let val = stack.pop().unwrap_or(Value::NULL);
                    let idx = *i as usize;
                    if idx >= locals.len() {
                        locals.resize(idx + 1, Value::NULL);
                    }
                    locals[idx] = val;
                }

                Opcode::Dup => {
                    if let Some(top) = stack.last().cloned() {
                        stack.push(top);
                    }
                }
                Opcode::DupX1 => {
                    let len = stack.len();
                    if len >= 2 {
                        let top = stack[len - 1].clone();
                        stack.insert(len - 2, top);
                    }
                }
                // A `long` or `double` is one entry on this operand stack but a
                // category-2 value to the JVM: the `dup2`/`pop2` family moves
                // one of them where it would move two category-1 values.
                Opcode::DupX2 => {
                    let len = stack.len();
                    let depth = if len >= 2 && is_wide(&stack[len - 2]) {
                        2
                    } else {
                        3
                    };
                    if len >= depth {
                        let top = stack[len - 1].clone();
                        stack.insert(len - depth, top);
                    }
                }
                Opcode::Dup2 => {
                    let len = stack.len();
                    if len >= 1 && is_wide(&stack[len - 1]) {
                        let a = stack[len - 1].clone();
                        stack.push(a);
                    } else if len >= 2 {
                        let a = stack[len - 2].clone();
                        let b = stack[len - 1].clone();
                        stack.push(a);
                        stack.push(b);
                    }
                }
                Opcode::Dup2X1 => {
                    let len = stack.len();
                    if len >= 2 && is_wide(&stack[len - 1]) {
                        let top = stack[len - 1].clone();
                        stack.insert(len - 2, top);
                    } else if len >= 3 {
                        let a = stack[len - 2].clone();
                        let b = stack[len - 1].clone();
                        stack.insert(len - 3, b);
                        stack.insert(len - 3, a);
                    }
                }
                Opcode::Dup2X2 => {
                    let len = stack.len();
                    if len >= 1 && is_wide(&stack[len - 1]) {
                        // v1 is wide: under one wide value or two narrow ones.
                        let depth = if len >= 2 && is_wide(&stack[len - 2]) {
                            2
                        } else {
                            3
                        };
                        if len >= depth {
                            let top = stack[len - 1].clone();
                            stack.insert(len - depth, top);
                        }
                    } else if len >= 3 {
                        // v2, v1 narrow: under one wide value or two narrow ones.
                        let depth = if is_wide(&stack[len - 3]) { 3 } else { 4 };
                        if len >= depth {
                            let a = stack[len - 2].clone();
                            let b = stack[len - 1].clone();
                            stack.insert(len - depth, b);
                            stack.insert(len - depth, a);
                        }
                    }
                }
                Opcode::Pop => {
                    stack.pop();
                }
                Opcode::Pop2 => {
                    if !stack.last().is_some_and(is_wide) {
                        stack.pop();
                    }
                    stack.pop();
                }
                Opcode::Swap => {
                    let len = stack.len();
                    if len >= 2 {
                        stack.swap(len - 1, len - 2);
                    }
                }

                Opcode::Iadd => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Int(a.wrapping_add(b)));
                }
                Opcode::Isub => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Int(a.wrapping_sub(b)));
                }
                Opcode::Imul => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Int(a.wrapping_mul(b)));
                }
                Opcode::Idiv => {
                    let b = stack.pop().unwrap_or(Value::Int(1)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if b == 0 {
                        if let Some(target) = self.raise(
                            code,
                            byte_offset,
                            "java/lang/ArithmeticException",
                            "/ by zero",
                            stack,
                        ) {
                            pc = target;
                            continue;
                        }
                        if self.pending_exception.is_some() {
                            return Ok(Value::NULL);
                        }
                        stack.push(Value::Int(0));
                    } else {
                        stack.push(Value::Int(a.wrapping_div(b)));
                    }
                }
                Opcode::Irem => {
                    let b = stack.pop().unwrap_or(Value::Int(1)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if b == 0 {
                        if let Some(target) = self.raise(
                            code,
                            byte_offset,
                            "java/lang/ArithmeticException",
                            "/ by zero",
                            stack,
                        ) {
                            pc = target;
                            continue;
                        }
                        if self.pending_exception.is_some() {
                            return Ok(Value::NULL);
                        }
                        stack.push(Value::Int(0));
                    } else {
                        stack.push(Value::Int(a.wrapping_rem(b)));
                    }
                }
                Opcode::Ineg => {
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Int(-a));
                }
                Opcode::Iinc(idx, c) => {
                    let i = *idx as usize;
                    if i < locals.len() {
                        let cur = locals[i].as_int();
                        locals[i] = Value::Int(cur.wrapping_add(*c));
                    }
                }

                Opcode::Fadd => {
                    let b = match stack.pop() {
                        Some(Value::Float(f)) => f,
                        _ => 0.0,
                    };
                    let a = match stack.pop() {
                        Some(Value::Float(f)) => f,
                        _ => 0.0,
                    };
                    stack.push(Value::Float(a + b));
                }
                Opcode::Fsub => {
                    let b = match stack.pop() {
                        Some(Value::Float(f)) => f,
                        _ => 0.0,
                    };
                    let a = match stack.pop() {
                        Some(Value::Float(f)) => f,
                        _ => 0.0,
                    };
                    stack.push(Value::Float(a - b));
                }
                Opcode::Fmul => {
                    let b = match stack.pop() {
                        Some(Value::Float(f)) => f,
                        _ => 0.0,
                    };
                    let a = match stack.pop() {
                        Some(Value::Float(f)) => f,
                        _ => 0.0,
                    };
                    stack.push(Value::Float(a * b));
                }
                Opcode::Fdiv => {
                    let b = match stack.pop() {
                        Some(Value::Float(f)) => f,
                        _ => 1.0,
                    };
                    let a = match stack.pop() {
                        Some(Value::Float(f)) => f,
                        _ => 0.0,
                    };
                    stack.push(Value::Float(a / b));
                }
                Opcode::Fneg => {
                    let a = stack.pop().unwrap_or(Value::Float(0.0)).as_float();
                    stack.push(Value::Float(-a));
                }

                Opcode::Ishl => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Int(a.wrapping_shl(b as u32 & 31)));
                }
                Opcode::Ishr => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Int(a.wrapping_shr(b as u32 & 31)));
                }
                Opcode::Iushr => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Int(((a as u32) >> (b as u32 & 31)) as i32));
                }
                Opcode::Iand => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Int(a & b));
                }
                Opcode::Ior => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Int(a | b));
                }
                Opcode::Ixor => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Int(a ^ b));
                }

                Opcode::Ladd => {
                    let b = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Long(a.wrapping_add(b)));
                }
                Opcode::Lsub => {
                    let b = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Long(a.wrapping_sub(b)));
                }
                Opcode::Lmul => {
                    let b = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Long(a.wrapping_mul(b)));
                }
                Opcode::Ldiv => {
                    let b = stack.pop().unwrap_or(Value::Long(1)).as_long();
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    if b == 0 {
                        if let Some(target) = self.raise(
                            code,
                            byte_offset,
                            "java/lang/ArithmeticException",
                            "/ by zero",
                            stack,
                        ) {
                            pc = target;
                            continue;
                        }
                        if self.pending_exception.is_some() {
                            return Ok(Value::NULL);
                        }
                        stack.push(Value::Long(0));
                    } else {
                        stack.push(Value::Long(a.wrapping_div(b)));
                    }
                }
                Opcode::Lrem => {
                    let b = stack.pop().unwrap_or(Value::Long(1)).as_long();
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    if b == 0 {
                        if let Some(target) = self.raise(
                            code,
                            byte_offset,
                            "java/lang/ArithmeticException",
                            "/ by zero",
                            stack,
                        ) {
                            pc = target;
                            continue;
                        }
                        if self.pending_exception.is_some() {
                            return Ok(Value::NULL);
                        }
                        stack.push(Value::Long(0));
                    } else {
                        stack.push(Value::Long(a.wrapping_rem(b)));
                    }
                }
                Opcode::Lneg => {
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Long(a.wrapping_neg()));
                }
                Opcode::Lshl => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Long(a.wrapping_shl(b as u32 & 63)));
                }
                Opcode::Lshr => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Long(a.wrapping_shr(b as u32 & 63)));
                }
                Opcode::Lushr => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Long(((a as u64) >> (b as u32 & 63)) as i64));
                }
                Opcode::Land => {
                    let b = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Long(a & b));
                }
                Opcode::Lor => {
                    let b = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Long(a | b));
                }
                Opcode::Lxor => {
                    let b = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Long(a ^ b));
                }

                Opcode::Dadd => {
                    let b = stack.pop().unwrap_or(Value::Double(0.0)).as_double();
                    let a = stack.pop().unwrap_or(Value::Double(0.0)).as_double();
                    stack.push(Value::Double(a + b));
                }
                Opcode::Dsub => {
                    let b = stack.pop().unwrap_or(Value::Double(0.0)).as_double();
                    let a = stack.pop().unwrap_or(Value::Double(0.0)).as_double();
                    stack.push(Value::Double(a - b));
                }
                Opcode::Dmul => {
                    let b = stack.pop().unwrap_or(Value::Double(0.0)).as_double();
                    let a = stack.pop().unwrap_or(Value::Double(0.0)).as_double();
                    stack.push(Value::Double(a * b));
                }
                Opcode::Ddiv => {
                    let b = stack.pop().unwrap_or(Value::Double(1.0)).as_double();
                    let a = stack.pop().unwrap_or(Value::Double(0.0)).as_double();
                    stack.push(Value::Double(a / b));
                }
                Opcode::Drem => {
                    let b = stack.pop().unwrap_or(Value::Double(1.0)).as_double();
                    let a = stack.pop().unwrap_or(Value::Double(0.0)).as_double();
                    stack.push(Value::Double(a % b));
                }
                Opcode::Dneg => {
                    let a = stack.pop().unwrap_or(Value::Double(0.0)).as_double();
                    stack.push(Value::Double(-a));
                }

                Opcode::Lcmp => {
                    let b = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Int(a.cmp(&b) as i32));
                }
                Opcode::Fcmpl | Opcode::Fcmpg => {
                    let b = stack.pop().unwrap_or(Value::Float(0.0)).as_float();
                    let a = stack.pop().unwrap_or(Value::Float(0.0)).as_float();
                    let v = if a.is_nan() || b.is_nan() {
                        if matches!(op, Opcode::Fcmpg) { 1 } else { -1 }
                    } else if a < b {
                        -1
                    } else if a > b {
                        1
                    } else {
                        0
                    };
                    stack.push(Value::Int(v));
                }
                Opcode::Dcmpl | Opcode::Dcmpg => {
                    let b = stack.pop().unwrap_or(Value::Double(0.0)).as_double();
                    let a = stack.pop().unwrap_or(Value::Double(0.0)).as_double();
                    let v = if a.is_nan() || b.is_nan() {
                        if matches!(op, Opcode::Dcmpg) { 1 } else { -1 }
                    } else if a < b {
                        -1
                    } else if a > b {
                        1
                    } else {
                        0
                    };
                    stack.push(Value::Int(v));
                }

                Opcode::I2l => {
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Long(a as i64));
                }
                Opcode::I2f => {
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Float(a as f32));
                }
                Opcode::I2d => {
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Double(a as f64));
                }
                Opcode::L2i => {
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Int(a as i32));
                }
                Opcode::L2f => {
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Float(a as f32));
                }
                Opcode::L2d => {
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Double(a as f64));
                }
                Opcode::F2i => {
                    let a = stack.pop().unwrap_or(Value::Float(0.0)).as_float();
                    stack.push(Value::Int(a as i32));
                }
                Opcode::F2l => {
                    let a = stack.pop().unwrap_or(Value::Float(0.0)).as_float();
                    stack.push(Value::Long(a as i64));
                }
                Opcode::F2d => {
                    let a = stack.pop().unwrap_or(Value::Float(0.0)).as_float();
                    stack.push(Value::Double(a as f64));
                }
                Opcode::D2i => {
                    let a = stack.pop().unwrap_or(Value::Double(0.0)).as_double();
                    stack.push(Value::Int(a as i32));
                }
                Opcode::D2l => {
                    let a = stack.pop().unwrap_or(Value::Double(0.0)).as_double();
                    stack.push(Value::Long(a as i64));
                }
                Opcode::D2f => {
                    let a = stack.pop().unwrap_or(Value::Double(0.0)).as_double();
                    stack.push(Value::Float(a as f32));
                }
                Opcode::I2b => {
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Int(a as i8 as i32));
                }
                Opcode::I2c => {
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Int(a as u16 as i32));
                }
                Opcode::I2s => {
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Int(a as i16 as i32));
                }

                Opcode::Monitorenter | Opcode::Monitorexit => {
                    // No monitor support: the object being locked is consumed.
                    stack.pop();
                }
                Opcode::Multianewarray(elem, dims) => {
                    let mut counts = Vec::with_capacity(*dims as usize);
                    for _ in 0..*dims {
                        counts.push(stack.pop().unwrap_or(Value::Int(0)).as_int().max(0) as usize);
                    }
                    counts.reverse();
                    let value = self.build_multidim(elem, &counts);
                    stack.push(value);
                }

                Opcode::Ifeq(off) => {
                    let val = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if val == 0 {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }
                Opcode::Ifne(off) => {
                    let val = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if val != 0 {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }
                Opcode::Iflt(off) => {
                    let val = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if val < 0 {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }
                Opcode::Ifge(off) => {
                    let val = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if val >= 0 {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }
                Opcode::Ifgt(off) => {
                    let val = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if val > 0 {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }
                Opcode::Ifle(off) => {
                    let val = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if val <= 0 {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }

                Opcode::IfIcmpeq(off) => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if a == b {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }
                Opcode::IfIcmpne(off) => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if a != b {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }
                Opcode::IfIcmplt(off) => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if a < b {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }
                Opcode::IfIcmpge(off) => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if a >= b {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }
                Opcode::IfIcmpgt(off) => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if a > b {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }
                Opcode::IfIcmple(off) => {
                    let b = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if a <= b {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }

                Opcode::IfAcmpeq(off) => {
                    let b = stack.pop().unwrap_or(Value::NULL);
                    let a = stack.pop().unwrap_or(Value::NULL);
                    if self.values_equal(&a, &b) {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }
                Opcode::IfAcmpne(off) => {
                    let b = stack.pop().unwrap_or(Value::NULL);
                    let a = stack.pop().unwrap_or(Value::NULL);
                    if !self.values_equal(&a, &b) {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }
                Opcode::IfNull(off) => {
                    let a = stack.pop().unwrap_or(Value::NULL);
                    if a.is_null() {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }
                Opcode::IfNonNull(off) => {
                    let a = stack.pop().unwrap_or(Value::NULL);
                    if !a.is_null() {
                        pc = find_target_pc(opcodes, byte_offset, *off);
                    }
                }

                Opcode::Goto(off) => {
                    pc = find_target_pc(opcodes, byte_offset, *off);
                }
                Opcode::Tableswitch {
                    default,
                    low,
                    high,
                    offsets,
                } => {
                    let val = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let target_offset = if val >= *low && val <= *high {
                        offsets
                            .get((val - low) as usize)
                            .copied()
                            .unwrap_or(*default)
                    } else {
                        *default
                    };
                    pc = find_target_pc(opcodes, byte_offset, target_offset);
                }
                Opcode::Lookupswitch { default, pairs } => {
                    let val = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let mut target_offset = *default;
                    for (k, off) in pairs {
                        if *k == val {
                            target_offset = *off;
                            break;
                        }
                    }
                    pc = find_target_pc(opcodes, byte_offset, target_offset);
                }

                Opcode::Return => return Ok(Value::NULL),
                Opcode::Ireturn
                | Opcode::Freturn
                | Opcode::Lreturn
                | Opcode::Dreturn
                | Opcode::Areturn => {
                    return Ok(stack.pop().unwrap_or(Value::NULL));
                }

                Opcode::Getstatic {
                    class_name,
                    field_name,
                    ..
                } => {
                    // Synthetic fields must not run the real class initializer:
                    // `Items.<clinit>` builds every item only for the next line
                    // to replace the value with the runtime's own.
                    // A static field reached through a subclass is declared by
                    // that subclass's superclass, and a JVM initializes the
                    // class that declares it.
                    let owner = self
                        .static_field_owner(class_name, field_name)
                        .unwrap_or_else(|| class_name.clone());
                    for subject in [class_name, &owner] {
                        if has_synthetic_statics(subject) {
                            continue;
                        }
                        self.ensure_initialized(subject)?;
                        if let Some(target) = self.unwind_here(code, byte_offset, stack) {
                            pc = target;
                            continue;
                        }
                        if self.pending_exception.is_some() {
                            return Ok(Value::NULL);
                        }
                    }
                    let val = self.resolve_static_field(class_name, field_name);
                    stack.push(val);
                }
                Opcode::Putstatic {
                    class_name,
                    field_name,
                    ..
                } => {
                    // An inherited static field belongs to the class that
                    // declares it; writing it through the subclass must not
                    // create a second slot, and that declaring class is the one
                    // a JVM initializes.
                    let owner = self
                        .static_field_owner(class_name, field_name)
                        .unwrap_or_else(|| class_name.clone());
                    for subject in [class_name, &owner] {
                        self.ensure_initialized(subject)?;
                        if let Some(target) = self.unwind_here(code, byte_offset, stack) {
                            pc = target;
                            continue;
                        }
                        if self.pending_exception.is_some() {
                            return Ok(Value::NULL);
                        }
                    }
                    let val = stack.pop().unwrap_or(Value::NULL);
                    if class_name.contains("ServerConfigurationConnectionEvents")
                        || (class_name == "java/nio/Buffer" && field_name == "UNSAFE")
                    {
                    }
                    if let Some(c) = self.loader.get_mut(&owner) {
                        c.static_fields.insert(field_name.clone(), val);
                    }
                    // The class field is now the truth: drop any placeholder
                    // handed out before the initializer ran, or two readers of
                    // the "same" static hold different objects.
                    self.static_singletons
                        .remove(&(owner.clone(), field_name.clone()));
                    self.static_singletons
                        .remove(&(class_name.clone(), field_name.clone()));
                }
                Opcode::Getfield {
                    class_name: field_class,
                    field_name,
                    descriptor,
                } => {
                    let obj_ref = stack.pop().unwrap_or(Value::NULL);
                    if obj_ref.is_null() && self.field_is_checkable(field_class, field_name) {
                        let message =
                            format!("Cannot read field \"{field_name}\" because value is null");
                        if let Some(target) = self.raise(
                            code,
                            byte_offset,
                            "java/lang/NullPointerException",
                            &message,
                            stack,
                        ) {
                            pc = target;
                            continue;
                        }
                        if self.pending_exception.is_some() {
                            return Ok(Value::NULL);
                        }
                        stack.push(Value::NULL);
                        continue;
                    }
                    let val = if let Some(id) = obj_ref.as_obj() {
                        if let Some(JavaObject::Instance { fields, .. }) = self.heap.get(id) {
                            // A field nothing has written reads as its type's
                            // zero, as in a JVM (an `int` is 0, not null).
                            fields
                                .get(field_name)
                                .cloned()
                                .unwrap_or_else(|| default_for_descriptor(descriptor))
                        } else {
                            self.native_field(id, field_name)
                        }
                    } else {
                        Value::NULL
                    };
                    stack.push(val);
                }
                Opcode::Putfield {
                    class_name: field_class,
                    field_name,
                    ..
                } => {
                    let val = stack.pop().unwrap_or(Value::NULL);
                    let obj_ref = stack.pop().unwrap_or(Value::NULL);
                    if obj_ref.is_null() && self.field_is_checkable(field_class, field_name) {
                        let message =
                            format!("Cannot assign field \"{field_name}\" because value is null");
                        if let Some(target) = self.raise(
                            code,
                            byte_offset,
                            "java/lang/NullPointerException",
                            &message,
                            stack,
                        ) {
                            pc = target;
                            continue;
                        }
                        if self.pending_exception.is_some() {
                            return Ok(Value::NULL);
                        }
                        continue;
                    }
                    if let Some(id) = obj_ref.as_obj() {
                        if let Some(JavaObject::Instance { fields, .. }) = self.heap.get_mut(id) {
                            fields.insert(field_name.clone(), val);
                        }
                    }
                }

                Opcode::New(cls) => {
                    self.ensure_initialized(cls)?;
                    if let Some(target) = self.unwind_here(code, byte_offset, stack) {
                        pc = target;
                        continue;
                    }
                    if self.pending_exception.is_some() {
                        return Ok(Value::NULL);
                    }
                    let id = self.alloc_uninitialized(cls);
                    stack.push(Value::Object(Some(id)));
                }
                Opcode::Newarray(elem) | Opcode::Anewarray(elem) => {
                    let size = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    if size < 0 {
                        if let Some(target) = self.raise(
                            code,
                            byte_offset,
                            "java/lang/NegativeArraySizeException",
                            &size.to_string(),
                            stack,
                        ) {
                            pc = target;
                            continue;
                        }
                        if self.pending_exception.is_some() {
                            return Ok(Value::NULL);
                        }
                    }
                    let count = size.max(0) as usize;
                    let default_val = match elem.as_str() {
                        "boolean" | "byte" | "short" | "char" | "int" => Value::Int(0),
                        "long" => Value::Long(0),
                        "float" => Value::Float(0.0),
                        "double" => Value::Double(0.0),
                        _ => Value::NULL,
                    };
                    let id = self.heap.alloc(JavaObject::Array {
                        elem_type: elem.clone(),
                        elements: vec![default_val; count],
                    });
                    stack.push(Value::Object(Some(id)));
                }
                Opcode::Arraylength => {
                    let arr_ref = stack.pop().unwrap_or(Value::NULL);
                    let len = arr_ref.as_obj().and_then(|id| match self.heap.get(id) {
                        Some(JavaObject::Array { elements, .. }) => Some(elements.len() as i32),
                        _ => None,
                    });
                    let len = if let Some(len) = len {
                        len
                    } else {
                        let message = "Cannot read the array length because value is null";
                        if let Some(target) = self.raise(
                            code,
                            byte_offset,
                            "java/lang/NullPointerException",
                            message,
                            stack,
                        ) {
                            pc = target;
                            continue;
                        }
                        if self.pending_exception.is_some() {
                            return Ok(Value::NULL);
                        }
                        0
                    };
                    stack.push(Value::Int(len));
                }
                Opcode::Aaload
                | Opcode::Baload
                | Opcode::Caload
                | Opcode::Saload
                | Opcode::Iaload
                | Opcode::Laload
                | Opcode::Faload
                | Opcode::Daload => {
                    let idx = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let arr_ref = stack.pop().unwrap_or(Value::NULL);
                    let (found, length) = match arr_ref.as_obj().and_then(|id| self.heap.get(id)) {
                        Some(JavaObject::Array { elements, .. }) => (
                            usize::try_from(idx)
                                .ok()
                                .and_then(|i| elements.get(i))
                                .cloned(),
                            Some(elements.len()),
                        ),
                        _ => (None, None),
                    };
                    if let Some(value) = found {
                        stack.push(value);
                    } else {
                        let (class, message) = match length {
                            Some(length) => (
                                "java/lang/ArrayIndexOutOfBoundsException",
                                format!("Index {idx} out of bounds for length {length}"),
                            ),
                            None => (
                                "java/lang/NullPointerException",
                                "Cannot load from array because it is null".to_string(),
                            ),
                        };
                        if let Some(target) = self.raise(code, byte_offset, class, &message, stack)
                        {
                            pc = target;
                            continue;
                        }
                        if self.pending_exception.is_some() {
                            return Ok(Value::NULL);
                        }
                        stack.push(Value::NULL);
                    }
                }
                Opcode::Aastore
                | Opcode::Bastore
                | Opcode::Castore
                | Opcode::Sastore
                | Opcode::Iastore
                | Opcode::Lastore
                | Opcode::Fastore
                | Opcode::Dastore => {
                    let val = stack.pop().unwrap_or(Value::NULL);
                    let idx = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    let arr_ref = stack.pop().unwrap_or(Value::NULL);
                    let mut failure = Some((
                        "java/lang/NullPointerException",
                        "Cannot store to array because it is null".to_string(),
                    ));
                    if let Some(id) = arr_ref.as_obj() {
                        if let Some(JavaObject::Array { elements, .. }) = self.heap.get_mut(id) {
                            match usize::try_from(idx).ok().filter(|i| *i < elements.len()) {
                                Some(i) => {
                                    elements[i] = val;
                                    failure = None;
                                }
                                None => {
                                    failure = Some((
                                        "java/lang/ArrayIndexOutOfBoundsException",
                                        format!(
                                            "Index {idx} out of bounds for length {}",
                                            elements.len()
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                    if let Some((class, message)) = failure {
                        if let Some(target) = self.raise(code, byte_offset, class, &message, stack)
                        {
                            pc = target;
                            continue;
                        }
                        if self.pending_exception.is_some() {
                            return Ok(Value::NULL);
                        }
                    }
                }

                Opcode::Checkcast(target_type) => {
                    // A reference that is not an instance of the target throws;
                    // `null` passes through every cast.
                    let value = stack.last().cloned().unwrap_or(Value::NULL);
                    if self.cast_is_checkable(&value, target_type)
                        && !self.is_instance_of(&value, target_type)
                    {
                        let from = self
                            .heap
                            .get(value.as_obj().unwrap_or(usize::MAX))
                            .map(|object| object.class_name().to_string())
                            .unwrap_or_else(|| "java/lang/Object".into());
                        let message = format!(
                            "class {} cannot be cast to class {}",
                            from.replace('/', "."),
                            target_type.replace('/', ".")
                        );
                        if let Some(target) = self.raise(
                            code,
                            byte_offset,
                            "java/lang/ClassCastException",
                            &message,
                            stack,
                        ) {
                            pc = target;
                            continue;
                        }
                        if self.pending_exception.is_some() {
                            return Ok(Value::NULL);
                        }
                    }
                }
                Opcode::Instanceof(target_type) => {
                    let obj_ref = stack.pop().unwrap_or(Value::NULL);
                    let is_inst = self.is_instance_of(&obj_ref, target_type);
                    stack.push(Value::from_bool(is_inst));
                }

                Opcode::Invokestatic {
                    class_name,
                    method_name,
                    descriptor,
                } => {
                    self.ensure_initialized(class_name)?;
                    let param_count = parse_descriptor_param_count(descriptor);
                    let mut call_args = Vec::with_capacity(param_count);
                    for _ in 0..param_count {
                        call_args.push(stack.pop().unwrap_or(Value::NULL));
                    }
                    call_args.reverse();

                    let ret = self.execute_special_method(
                        class_name,
                        method_name,
                        descriptor,
                        &call_args,
                    )?;
                    self.record_menu_child(method_name, &call_args);
                    if !descriptor.ends_with(")V") {
                        stack.push(ret);
                    }
                    // The callee may have thrown: this frame catches it when one
                    // of its handlers covers the call, otherwise it unwinds.
                    if let Some(target) = self.unwind_here(code, byte_offset, stack) {
                        pc = target;
                        continue;
                    }
                    if self.pending_exception.is_some() {
                        return Ok(Value::NULL);
                    }
                }

                Opcode::Invokespecial {
                    class_name,
                    method_name,
                    descriptor,
                } => {
                    let param_count = parse_descriptor_param_count(descriptor);
                    let mut call_args = Vec::with_capacity(param_count + 1);
                    for _ in 0..param_count {
                        call_args.push(stack.pop().unwrap_or(Value::NULL));
                    }
                    call_args.reverse();
                    let this_ref = stack.pop().unwrap_or(Value::NULL);
                    call_args.insert(0, this_ref);

                    let ret = self.execute_special_method(
                        class_name,
                        method_name,
                        descriptor,
                        &call_args,
                    )?;
                    self.record_menu_child(method_name, &call_args);

                    if !descriptor.ends_with(")V") {
                        stack.push(ret);
                    }
                    // The callee may have thrown: this frame catches it when one
                    // of its handlers covers the call, otherwise it unwinds.
                    if let Some(target) = self.unwind_here(code, byte_offset, stack) {
                        pc = target;
                        continue;
                    }
                    if self.pending_exception.is_some() {
                        return Ok(Value::NULL);
                    }
                }

                Opcode::Invokevirtual {
                    class_name,
                    method_name,
                    descriptor,
                }
                | Opcode::Invokeinterface {
                    class_name,
                    method_name,
                    descriptor,
                } => {
                    let param_count = parse_descriptor_param_count(descriptor);
                    let mut call_args = Vec::with_capacity(param_count + 1);
                    for _ in 0..param_count {
                        call_args.push(stack.pop().unwrap_or(Value::NULL));
                    }
                    call_args.reverse();
                    let this_ref = stack.pop().unwrap_or(Value::NULL);
                    // A call on a null receiver throws, as in a JVM.
                    if this_ref.is_null() {
                        let message = format!(
                            "Cannot invoke \"{}.{method_name}()\" because the receiver is null",
                            class_name.replace('/', ".")
                        );
                        if let Some(target) = self.raise(
                            code,
                            byte_offset,
                            "java/lang/NullPointerException",
                            &message,
                            stack,
                        ) {
                            pc = target;
                            continue;
                        }
                        if self.pending_exception.is_some() {
                            return Ok(Value::NULL);
                        }
                        // A method of `Class` on null ends the call even when
                        // nothing catches it: reflection walks
                        // (`c = c.getSuperclass()` until a field is found) end
                        // on it, and answering null instead loops forever.
                        if class_name == "java/lang/Class" {
                            return Err(Error::new(
                                "interpreter",
                                format!("java.lang.NullPointerException: {message}"),
                            ));
                        }
                    }
                    call_args.insert(0, this_ref);

                    let ret =
                        self.execute_method(class_name, method_name, descriptor, &call_args)?;
                    self.record_menu_child(method_name, &call_args);

                    if !descriptor.ends_with(")V") {
                        stack.push(ret);
                    }
                    // The callee may have thrown: this frame catches it when one
                    // of its handlers covers the call, otherwise it unwinds.
                    if let Some(target) = self.unwind_here(code, byte_offset, stack) {
                        pc = target;
                        continue;
                    }
                    if self.pending_exception.is_some() {
                        return Ok(Value::NULL);
                    }
                }

                Opcode::Invokedynamic {
                    bootstrap_idx,
                    name,
                    descriptor,
                } => {
                    let param_count = parse_descriptor_param_count(descriptor);
                    let mut captured = Vec::with_capacity(param_count);
                    for _ in 0..param_count {
                        captured.push(stack.pop().unwrap_or(Value::NULL));
                    }
                    captured.reverse();

                    let bsm = bootstrap_methods.get(*bootstrap_idx as usize);
                    let (target_class, target_method, target_desc) = if let Some(b) = bsm {
                        (
                            b.target_class.clone(),
                            b.target_method.clone(),
                            b.target_desc.clone(),
                        )
                    } else {
                        (this_class.to_string(), name.clone(), descriptor.clone())
                    };

                    // String concatenation is compiled to invokedynamic on every
                    // modern Java class. Treat the bootstrap recipe as real
                    // string construction instead of returning a lambda object:
                    // mod UIs build labels, version prefixes and keys this way.
                    if target_class == "java/lang/invoke/StringConcatFactory"
                        && (target_method == "makeConcatWithConstants"
                            || target_method == "makeConcat")
                    {
                        let strings = bsm
                            .map(|b| b.string_arguments.as_slice())
                            .unwrap_or_default();
                        // The call site's descriptor types each argument: a
                        // `boolean` prints `true`/`false` and a `char` its
                        // character, though both arrive as an int.
                        let kinds = parameter_kinds(descriptor);
                        let mut out = String::new();
                        if target_method == "makeConcatWithConstants" {
                            if let Some(recipe) = strings.first() {
                                let mut dynamic = 0usize;
                                let mut constant = 0usize;
                                for ch in recipe.chars() {
                                    if ch == '\u{1}' {
                                        if let Some(value) = captured.get(dynamic) {
                                            let text = self
                                                .concat_typed(kinds.get(dynamic).copied(), value);
                                            out.push_str(&text);
                                        }
                                        dynamic += 1;
                                    } else if ch == '\u{2}' {
                                        if let Some(value) = strings.get(constant + 1) {
                                            out.push_str(value);
                                        }
                                        constant += 1;
                                    } else {
                                        out.push(ch);
                                    }
                                }
                            }
                        } else {
                            for (index, value) in captured.iter().enumerate() {
                                let text = self.concat_typed(kinds.get(index).copied(), value);
                                out.push_str(&text);
                            }
                        }
                        let sid = self.heap.alloc_string(out);
                        stack.push(Value::Object(Some(sid)));
                        continue;
                    }

                    let return_type = extract_return_type(descriptor);
                    // `LambdaMetafactory.altMetafactory` adds interfaces to the
                    // lambda: `Serializable` (flag 1) and `markers` (flag 2).
                    let mut intersections = Vec::new();
                    if let Some(b) = bsm {
                        if b.bootstrap_method == "altMetafactory" && !b.int_arguments.is_empty() {
                            let flags = b.int_arguments[0];
                            if flags & 1 != 0 {
                                intersections.push("java/io/Serializable".to_string());
                            }
                            if flags & 2 != 0 && b.int_arguments.len() > 1 {
                                let count = b.int_arguments[1].max(0) as usize;
                                intersections.extend(b.class_arguments.iter().take(count).cloned());
                            }
                        }
                    }
                    let lambda_id = self.heap.alloc(JavaObject::Lambda {
                        interface_class: return_type,
                        interface_method: name.clone(),
                        intersections,
                        target_class,
                        target_method,
                        target_desc,
                        captured,
                    });
                    stack.push(Value::Object(Some(lambda_id)));
                }

                Opcode::Athrow => {
                    let exception = stack.pop().unwrap_or(Value::NULL);
                    if let Some(target) = self.throw_at(code, byte_offset, exception, stack) {
                        pc = target;
                        continue;
                    }
                    // Nothing here catches it: the caller unwinds.
                    return Ok(Value::NULL);
                }
            }
        }

        Ok(stack.pop().unwrap_or(Value::NULL))
    }

    /// The text concatenation appends for an argument of descriptor kind `kind`.
    fn concat_typed(&mut self, kind: Option<u8>, value: &Value) -> String {
        match (kind, value) {
            (Some(b'Z'), Value::Int(i)) => (*i != 0).to_string(),
            (Some(b'C'), Value::Int(i)) => char::from_u32(*i as u32 & 0xffff)
                .map(String::from)
                .unwrap_or_default(),
            _ => self.concat_text(value),
        }
    }

    /// The text string concatenation appends for `value`: a mod class's own
    /// `toString()` when it has one, otherwise the runtime's rendering.
    fn concat_text(&mut self, value: &Value) -> String {
        let class = match value.as_obj().and_then(|id| self.heap.get(id)) {
            // `Class.toString` is `class java.lang.Foo` (or `interface ...`).
            Some(JavaObject::Class(name)) => {
                let mut current = name.clone();
                for _ in 0..64 {
                    let Ok(Some(class)) = self.loader.get_or_load(&current) else {
                        break;
                    };
                    if class.is_interface {
                        return format!("interface {}", name.replace('/', "."));
                    }
                    match class.super_class.clone() {
                        Some(super_class) => current = super_class,
                        None => break,
                    }
                }
                return format!("class {}", name.replace('/', "."));
            }
            Some(JavaObject::Instance { class_name, .. }) => Some(class_name.clone()),
            Some(collection) if collection_skeleton(collection).is_some() => {
                return self.collection_text(value);
            }
            _ => None,
        };
        if let Some(class) = class {
            let mut current = Some(class);
            while let Some(name) = current {
                // The JDK's collections render themselves (`TreeMap` is
                // `AbstractMap.toString`); other JDK and Minecraft objects are
                // rendered by the runtime, which models many of them.
                if ["java/", "jdk/", "sun/", "net/minecraft/"]
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
                    && !name.starts_with("java/util/")
                {
                    break;
                }
                let Ok(Some(c)) = self.loader.get_or_load(&name) else {
                    break;
                };
                if c.methods
                    .get(&("toString".to_string(), "()Ljava/lang/String;".to_string()))
                    .is_some_and(|m| m.code.is_some())
                {
                    if let Ok(result) = self.execute_method(
                        &name,
                        "toString",
                        "()Ljava/lang/String;",
                        std::slice::from_ref(value),
                    ) {
                        if let Some(text) = result.as_obj().and_then(|id| self.heap.get_string(id))
                        {
                            return text.to_string();
                        }
                    }
                    break;
                }
                current = c.super_class.clone();
            }
        }
        crate::builtins::value_to_string(&self.heap, value)
    }

    /// Whether `value` is a runtime collection or an instance of a JDK
    /// `java.util` class, which render through their own `toString`.
    fn renders_as_jdk_collection(&self, value: &Value) -> bool {
        match value.as_obj().and_then(|id| self.heap.get(id)) {
            Some(JavaObject::Instance { class_name, .. }) => class_name.starts_with("java/util/"),
            Some(other) => collection_skeleton(other).is_some(),
            None => false,
        }
    }

    /// `toString()` of a runtime list, set or map: the JDK's own
    /// `AbstractCollection`/`AbstractMap` rendering (`[a, b]`, `{k=v}`), with
    /// each element rendered by its own `toString`.
    fn collection_text(&mut self, value: &Value) -> String {
        match self.execute_method(
            "java/lang/Object",
            "toString",
            "()Ljava/lang/String;",
            std::slice::from_ref(value),
        ) {
            Ok(result) => match result.as_obj().and_then(|id| self.heap.get_string(id)) {
                Some(text) => text.to_string(),
                None => crate::builtins::value_to_string(&self.heap, value),
            },
            Err(_) => crate::builtins::value_to_string(&self.heap, value),
        }
    }

    /// The class that declares `field_name` as a static field, following the
    /// superclass chain. A JVM resolves `getstatic Subclass.inheritedField`
    /// against the class that actually declares it, so both `Subclass.x` and
    /// `Superclass.x` name one storage slot.
    fn static_field_owner(&mut self, class_name: &str, field_name: &str) -> Option<String> {
        let key = (class_name.to_string(), field_name.to_string());
        if let Some(owner) = self.static_field_owners.get(&key) {
            return owner.clone();
        }
        let owner = self.find_static_field_owner(class_name, field_name);
        self.static_field_owners.insert(key, owner.clone());
        owner
    }

    fn find_static_field_owner(&mut self, class_name: &str, field_name: &str) -> Option<String> {
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(class_name.to_string());
        let mut visited = std::collections::HashSet::new();
        while let Some(curr) = queue.pop_front() {
            if !visited.insert(curr.clone()) {
                continue;
            }
            let _ = self.loader.get_or_load(&curr);
            let Some(class) = self.loader.get(&curr) else {
                continue;
            };
            if class
                .fields
                .get(field_name)
                .is_some_and(|field| field.is_static)
            {
                return Some(curr);
            }
            if let Some(super_name) = &class.super_class {
                queue.push_back(super_name.clone());
            }
            for iface in &class.interfaces {
                queue.push_back(iface.clone());
            }
        }
        None
    }

    /// The string a vanilla class's initializer binds `field_name` to (a tag path,
    /// or a sound id): `ldc "<path>"; invokestatic create/bind; putstatic FIELD`.
    pub(crate) fn vanilla_tag_path(
        &mut self,
        class_name: &str,
        field_name: &str,
    ) -> Option<String> {
        let _ = self.loader.get_or_load(class_name);
        let code = self
            .loader
            .get(class_name)?
            .methods
            .get(&("<clinit>".to_string(), "()V".to_string()))?
            .code
            .clone()?;
        let mut path: Option<&str> = None;
        for (_, opcode) in &code.opcodes {
            match opcode {
                Opcode::LdcString(text) => path = Some(text),
                Opcode::Putstatic {
                    class_name: owner,
                    field_name: name,
                    ..
                } if owner == class_name && name == field_name => {
                    return path.map(str::to_string);
                }
                _ => {}
            }
        }
        None
    }

    /// Whether a class static field or the runtime's static singleton cache
    /// kept a heap id `>= mark`, which the current allocation scope would free.
    pub(crate) fn static_refs_into(&self, mark: usize) -> bool {
        self.static_singletons
            .values()
            .any(|value| value.as_obj().is_some_and(|id| id >= mark))
            || self.loader.static_refs_into(mark)
    }

    /// Resolve static field value or create fallback singleton.
    pub fn resolve_static_field(&mut self, class_name: &str, field_name: &str) -> Value {
        let owner = self
            .static_field_owner(class_name, field_name)
            .unwrap_or_else(|| class_name.to_string());
        let class_name = owner.as_str();
        let key = (class_name.to_string(), field_name.to_string());
        if let Some(val) = self.static_singletons.get(&key) {
            return val.clone();
        }

        // `System.out`/`System.err` are set by the JVM's `initPhase1`, which
        // the runtime does not run: they are the host's standard streams.
        if class_name == "java/lang/System" && matches!(field_name, "out" | "err") {
            let console = crate::text_io::Console {
                error: field_name == "err",
                line: String::new(),
            };
            let id = self.heap.alloc_native(console);
            let value = Value::Object(Some(id));
            self.static_singletons.insert(key, value.clone());
            return value;
        }

        // A class whose initializer is running: its statics are whatever it has
        // assigned so far, and the rest are still the JVM's default. Synthesizing
        // one here would hand out a placeholder the initializer's own
        // `putstatic` then replaces.
        if self.initializing.iter().any(|name| name == class_name) {
            if let Some(class) = self.loader.get(class_name) {
                if let Some(val) = class.static_fields.get(field_name) {
                    return val.clone();
                }
                if let Some(field) = class.fields.get(field_name) {
                    if field.is_static {
                        return default_for_descriptor(&field.descriptor);
                    }
                }
            }
            return Value::NULL;
        }

        if let Some(class) = self.loader.get(class_name) {
            if let Some(val) = class.static_fields.get(field_name) {
                if !val.is_null() {
                    return val.clone();
                }
                // A mod's own static that nothing has assigned yet is null, as
                // in a JVM (`if (path == null) path = ...` must see null). Only
                // platform classes get synthesized values for unset statics.
                let platform = ["java/", "jdk/", "sun/", "net/minecraft/", "com/mojang/"]
                    .iter()
                    .any(|prefix| class_name.starts_with(prefix));
                // The JDK's statics come from its own `<clinit>` too: a null one
                // is a lazily set field (`SharedSecrets.javaUtilCollectionAccess`
                // is null until `ImmutableCollections` initializes), not a
                // constant to invent. Only the `VarHandle`s the runtime stands
                // in for are synthesized.
                let jdk = ["java/", "jdk/", "sun/"]
                    .iter()
                    .any(|prefix| class_name.starts_with(prefix))
                    && !(class_name.starts_with("java/")
                        && matches!(field_name, "NEXT" | "PREV" | "HEAD" | "TAIL" | "ITEM"));
                if (!platform || jdk) && class.fields.get(field_name).is_some_and(|f| f.is_static) {
                    if !self.building_exception
                        && (class_name.starts_with("net/fabricmc/") || field_name == "UNSAFE")
                    {
                    }
                    return Value::NULL;
                }
            }
        }

        // A platform class's statics are what its initializer built. Reading
        // one before that initializer ran would hand back a synthesized
        // placeholder that a later `putstatic` replaces, and two readers of
        // "the same" static would then hold different objects
        // (`BlockStateProperties.AGE_7` in a block's state definition and in
        // `setValue`). Initialize first, then synthesize only what is still
        // null.
        let platform = ["net/minecraft/", "com/mojang/", "java/", "jdk/", "sun/"]
            .iter()
            .any(|prefix| class_name.starts_with(prefix));
        if platform && !has_synthetic_statics(class_name) && !runtime_supplied_state(class_name) {
            let _ = self.ensure_initialized(class_name);
        }
        if let Some(class) = self.loader.get(class_name) {
            if let Some(val) = class.static_fields.get(field_name) {
                if !val.is_null() {
                    return val.clone();
                }
            }
        }
        let val = self.create_static_field_val(class_name, field_name);
        self.static_singletons.insert(key, val.clone());
        if let Some(class) = self.loader.get_mut(class_name) {
            class
                .static_fields
                .insert(field_name.to_string(), val.clone());
        }
        val
    }

    fn values_equal(&self, a: &Value, b: &Value) -> bool {
        if a == b {
            return true;
        }
        if let (Some(id1), Some(id2)) = (a.as_obj(), b.as_obj()) {
            match (self.heap.get(id1), self.heap.get(id2)) {
                (Some(JavaObject::Class(c1)), Some(JavaObject::Class(c2))) => {
                    return c1.replace('.', "/") == c2.replace('.', "/");
                }
                (Some(JavaObject::Native(n1)), Some(JavaObject::Native(n2))) => {
                    if let (Some(i1), Some(i2)) = (
                        n1.as_any().downcast_ref::<crate::minecraft::Item>(),
                        n2.as_any().downcast_ref::<crate::minecraft::Item>(),
                    ) {
                        return i1 == i2;
                    }
                }
                _ => {}
            }
        }
        false
    }

    fn create_static_field_val(&mut self, class_name: &str, field_name: &str) -> Value {
        if field_name.starts_with("$SwitchMap$") {
            let arr_id = self.heap.alloc(JavaObject::Array {
                elem_type: "int".into(),
                elements: vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)],
            });
            return Value::Object(Some(arr_id));
        }

        if class_name.starts_with("java/")
            && matches!(field_name, "NEXT" | "PREV" | "HEAD" | "TAIL" | "ITEM")
        {
            let fname = field_name.to_lowercase();
            let fid = self.heap.alloc_string(fname);
            let mut f = HashMap::new();
            f.insert("fieldName".to_string(), Value::Object(Some(fid)));
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: "java/lang/invoke/VarHandle".into(),
                fields: f,
            });
            return Value::Object(Some(id));
        }

        // Minecraft enum/singleton constants
        if class_name == "com/mojang/blaze3d/platform/InputConstants$Key" {
            let mut f = HashMap::new();
            f.insert("value".into(), Value::Int(-1));
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: class_name.into(),
                fields: f,
            });
            return Value::Object(Some(id));
        }

        if class_name == "net/minecraft/client/KeyMapping$Category" {
            let fid = self.heap.alloc_string(field_name);
            let mut f = HashMap::new();
            f.insert("name".into(), Value::Object(Some(fid)));
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: class_name.into(),
                fields: f,
            });
            return Value::Object(Some(id));
        }

        if class_name == "net/minecraft/world/item/Rarity" {
            let ordinal = match field_name {
                "UNCOMMON" => 1,
                "RARE" => 2,
                "EPIC" => 3,
                _ => 0,
            };
            let fid = self.heap.alloc_string(field_name);
            let mut fields = HashMap::new();
            fields.insert("name".to_string(), Value::Object(Some(fid)));
            fields.insert("ordinal".to_string(), Value::Int(ordinal));
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: class_name.into(),
                fields,
            });
            return Value::Object(Some(id));
        }

        if class_name == "net/minecraft/world/InteractionHand" {
            let fid = self.heap.alloc_string(field_name);
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: class_name.into(),
                fields: {
                    let mut f = HashMap::new();
                    f.insert("name".into(), Value::Object(Some(fid)));
                    f
                },
            });
            return Value::Object(Some(id));
        }

        if class_name == "net/minecraft/world/InteractionResult" {
            let fid = self.heap.alloc_string(field_name);
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: format!("{class_name}${field_name}"),
                fields: {
                    let mut f = HashMap::new();
                    f.insert("name".into(), Value::Object(Some(fid)));
                    f
                },
            });
            return Value::Object(Some(id));
        }

        if class_name == "net/minecraft/core/Direction" {
            let dir = match field_name {
                "DOWN" => Direction::DOWN,
                "NORTH" => Direction::NORTH,
                "SOUTH" => Direction::SOUTH,
                "WEST" => Direction::WEST,
                "EAST" => Direction::EAST,
                _ => Direction::UP,
            };
            let id = self.heap.alloc_direction(dir);
            return Value::Object(Some(id));
        }

        // The runtime stands in for Fabric Loader's metadata API: a
        // `CustomValue`'s type is the constant's name, as an interned string, so
        // `value.getType() == CvType.OBJECT` compares like the enum does.
        if class_name == "net/fabricmc/loader/api/metadata/CustomValue$CvType"
            && matches!(
                field_name,
                "OBJECT" | "ARRAY" | "STRING" | "NUMBER" | "BOOLEAN" | "NULL"
            )
        {
            let id = self.heap.alloc_string(field_name);
            return Value::Object(Some(id));
        }

        if class_name == "net/minecraft/core/Direction$Axis" {
            let id = self.heap.alloc_string(field_name);
            return Value::Object(Some(id));
        }

        if class_name == "net/minecraft/core/registries/BuiltInRegistries" {
            let mut fields = HashMap::new();
            let kind = self.heap.alloc_string(field_name);
            fields.insert("kind".to_string(), Value::Object(Some(kind)));
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/core/DefaultedRegistry".into(),
                fields,
            });
            return Value::Object(Some(id));
        }

        if class_name == "net/minecraft/core/registries/Registries" {
            let id = self.heap.alloc_string(field_name);
            return Value::Object(Some(id));
        }

        if class_name == "net/minecraft/world/entity/EquipmentSlot" {
            let id = self.heap.alloc_string(field_name);
            return Value::Object(Some(id));
        }

        if class_name == "net/minecraft/sounds/SoundEvents" {
            let id = self.heap.alloc_string(field_name);
            return Value::Object(Some(id));
        }

        // `DataComponents`' constants name the component types an item's
        // `DataComponentMap` is keyed by. The real `<clinit>` registers them
        // into Minecraft's built-in registries (tens of seconds in the
        // interpreter), and a stand-in for the wrong class makes every
        // component lookup miss: `ItemStack.getMaxStackSize()` then answers
        // the `getOrDefault` fallback of 1 for every item, and vanilla's own
        // `ItemStackHandler.extractItem` takes its "partial" branch and
        // destroys the stack. The stand-in keeps the field's own name, which
        // is what a component lookup compares.
        if class_name == "net/minecraft/core/component/DataComponents" {
            let name = self.heap.alloc_string(field_name);
            let mut fields = HashMap::new();
            fields.insert("name".to_string(), Value::Object(Some(name)));
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/core/component/DataComponentType".into(),
                fields,
            });
            return Value::Object(Some(id));
        }

        if class_name == "net/minecraft/world/item/Items" {
            let item_id = format!("minecraft:{}", field_name.to_lowercase());
            let id = self.heap.alloc_native(Item { id: item_id });
            return Value::Object(Some(id));
        }

        if class_name == "net/minecraft/core/particles/ParticleTypes" {
            // `ParticleTypes.FALLING_WATER` is the constant registered under
            // `minecraft:falling_water`; the runtime does not build the
            // particle registry, so the field name gives the id.
            let id = format!("minecraft:{}", field_name.to_ascii_lowercase());
            let id = self.heap.alloc_native(ParticleType { id });
            return Value::Object(Some(id));
        }

        if class_name == "net/minecraft/world/level/block/Blocks" {
            let block_id = format!("minecraft:{}", field_name.to_lowercase());
            let id = self.heap.alloc_native(BlockState::new(&block_id).block);
            return Value::Object(Some(id));
        }

        if class_name == "net/minecraft/tags/ItemTags"
            || class_name == "net/minecraft/tags/BlockTags"
        {
            // The real tag id is the string the class's initializer binds the
            // field to (`MINEABLE_WITH_PICKAXE` is `mineable/pickaxe`).
            let id = self.vanilla_tag_path(class_name, field_name).map_or_else(
                || field_name.to_lowercase(),
                |path| format!("minecraft:{path}"),
            );
            let id = self.heap.alloc_native(TagKey {
                registry: class_name.to_string(),
                id,
            });
            return Value::Object(Some(id));
        }

        if class_name == "io/github/jamalam360/jamlib/api/events/InteractionEvent" {
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: "io/github/jamalam360/jamlib/api/events/core/CancellableEvent".into(),
                fields: HashMap::new(),
            });
            return Value::Object(Some(id));
        }

        if field_name == "EVENT" || field_name.ends_with("_EVENT") || class_name.contains("Event") {
            let mut fields = HashMap::new();
            let arr_id = self.heap.alloc(JavaObject::Array {
                elem_type: "java/lang/Object".into(),
                elements: Vec::new(),
            });
            fields.insert("listeners".into(), Value::Object(Some(arr_id)));
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: "net/fabricmc/fabric/api/event/Event".into(),
                fields,
            });
            return Value::Object(Some(id));
        }

        if field_name == "INSTANCE" {
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: class_name.to_string(),
                fields: HashMap::new(),
            });
            return Value::Object(Some(id));
        }

        // TagKey fallback (e.g. RightClickHarvest.BLACKLIST, HOE_NEVER_REQUIRED)
        let id = self.heap.alloc_native(TagKey {
            registry: "tag".into(),
            id: field_name.to_lowercase(),
        });
        Value::Object(Some(id))
    }
}

/// Order two values for `Stream.sorted()` without a comparator. Objects have
/// no natural order here; callers that need one pass a comparator.
fn natural_order(a: &Value, b: &Value) -> i32 {
    let order = match (a, b) {
        (Value::Int(a), Value::Int(b)) => a.cmp(b),
        (Value::Long(a), Value::Long(b)) => a.cmp(b),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
        (Value::Double(a), Value::Double(b)) => {
            a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
        }
        _ => std::cmp::Ordering::Equal,
    };
    match order {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// Find index in opcodes corresponding to byte_offset + offset.
fn find_target_pc(opcodes: &[(usize, Opcode)], current_byte_offset: usize, offset: i32) -> usize {
    let target = (current_byte_offset as i64 + offset as i64) as usize;
    // Opcodes are stored in byte-offset order, so a branch lands in O(log n)
    // instead of scanning the whole method on every loop iteration.
    match opcodes.binary_search_by_key(&target, |(b_off, _)| *b_off) {
        Ok(index) => index,
        Err(index) => index.min(opcodes.len()),
    }
}

/// Parse descriptor parameter count, e.g. "(Ljava/lang/String;I)V" -> 2.
pub fn parse_descriptor_param_count(desc: &str) -> usize {
    let start = match desc.find('(') {
        Some(i) => i + 1,
        None => return 0,
    };
    let end = match desc.find(')') {
        Some(i) => i,
        None => return 0,
    };

    let params = &desc[start..end];
    let mut count = 0;
    let mut chars = params.chars();

    while let Some(c) = chars.next() {
        match c {
            '[' => {
                // Array type: consume prefix and continue to element type
            }
            'L' => {
                // Object type: scan until ';'
                for ch in chars.by_ref() {
                    if ch == ';' {
                        break;
                    }
                }
                count += 1;
            }
            'Z' | 'B' | 'C' | 'S' | 'I' | 'J' | 'F' | 'D' => {
                count += 1;
            }
            _ => {}
        }
    }
    count
}

/// Extract return type from descriptor, e.g. "()Lnet/minecraft/world/InteractionResult;" -> "net/minecraft/world/InteractionResult".
fn extract_return_type(desc: &str) -> String {
    if let Some(idx) = desc.rfind(')') {
        let ret = &desc[idx + 1..];
        if ret.starts_with('L') && ret.ends_with(';') {
            return ret[1..ret.len() - 1].to_string();
        }
        return ret.to_string();
    }
    "java/lang/Object".into()
}

/// The JVM descriptor of the class a `java.lang.Class` object names.
fn class_descriptor(name: &str) -> String {
    match name {
        "boolean" => "Z".into(),
        "byte" => "B".into(),
        "char" => "C".into(),
        "short" => "S".into(),
        "int" => "I".into(),
        "long" => "J".into(),
        "float" => "F".into(),
        "double" => "D".into(),
        _ if name.starts_with('[') => name.to_string(),
        _ => format!("L{name};"),
    }
}

#[cfg(test)]
mod exception_tests {
    use super::*;
    use crate::class::ExceptionHandler;

    fn run(opcodes: Vec<Opcode>, handlers: Vec<ExceptionHandler>) -> Value {
        let code = CodeInfo {
            max_stack: 4,
            max_locals: 0,
            opcodes: opcodes.into_iter().enumerate().collect(),
            handlers,
        };
        Interpreter::new()
            .interpret_code("T", "m", "()I", &[], &code, &[])
            .expect("the method runs")
    }

    fn catching(class: Option<&str>) -> Vec<ExceptionHandler> {
        vec![ExceptionHandler {
            start: 0,
            end: 3,
            handler: 4,
            catch_type: class.map(str::to_string),
        }]
    }

    fn divide_by_zero() -> Vec<Opcode> {
        vec![
            Opcode::Iconst(7),
            Opcode::Iconst(0),
            Opcode::Idiv,
            Opcode::Ireturn,
            Opcode::Pop,
            Opcode::Iconst(-5),
            Opcode::Ireturn,
        ]
    }

    #[test]
    fn a_handler_of_the_method_catches_division_by_zero() {
        let result = run(
            divide_by_zero(),
            catching(Some("java/lang/ArithmeticException")),
        );
        assert_eq!(result.as_int(), -5);
    }

    #[test]
    fn a_handler_for_another_class_does_not_catch_it() {
        let result = run(
            divide_by_zero(),
            catching(Some("java/lang/IllegalStateException")),
        );
        assert_eq!(result.as_int(), 0, "falls through to the lenient result");
    }

    #[test]
    fn a_catch_all_handler_catches_athrow() {
        let result = run(
            vec![
                Opcode::Iconst(1),
                Opcode::Pop,
                Opcode::AconstNull,
                Opcode::Athrow,
                Opcode::Pop,
                Opcode::Iconst(9),
                Opcode::Ireturn,
            ],
            vec![ExceptionHandler {
                start: 0,
                end: 4,
                handler: 4,
                catch_type: None,
            }],
        );
        assert_eq!(result.as_int(), 9);
    }
}

/// Calls that copy the elements of a collection argument into a heap-modelled
/// list or set.
fn collection_copy_call(class_name: &str, method_name: &str) -> bool {
    matches!(
        class_name,
        "java/util/List"
            | "java/util/Set"
            | "java/util/ArrayList"
            | "java/util/LinkedList"
            | "java/util/HashSet"
            | "java/util/LinkedHashSet"
    ) && matches!(method_name, "copyOf" | "<init>" | "addAll")
}

/// Whether a modelled collection class matches `target`: the collection's own
/// hierarchy plus the implementation family it stands for.
fn modelled_collection_assignable(modelled: &str, target: &str) -> bool {
    const LIST: &[&str] = &[
        "java/util/ArrayList",
        "java/util/LinkedList",
        "java/util/AbstractList",
        "java/util/AbstractSequentialList",
        "java/util/List",
        "java/util/SequencedCollection",
        "java/util/RandomAccess",
        "java/lang/Cloneable",
        "java/io/Serializable",
    ];
    const MAP: &[&str] = &[
        "java/util/HashMap",
        "java/util/LinkedHashMap",
        "java/util/TreeMap",
        "java/util/concurrent/ConcurrentHashMap",
        "java/util/concurrent/ConcurrentMap",
        "java/util/AbstractMap",
        "java/util/Map",
        "java/util/SequencedMap",
        "java/io/Serializable",
        "java/lang/Cloneable",
    ];
    const SET: &[&str] = &[
        "java/util/HashSet",
        "java/util/LinkedHashSet",
        "java/util/TreeSet",
        "java/util/AbstractSet",
        "java/util/Set",
        "java/util/SequencedSet",
        "java/io/Serializable",
        "java/lang/Cloneable",
    ];
    let family = match modelled {
        "java/util/ArrayList" | "java/util/LinkedList" => LIST,
        "java/util/HashMap" | "java/util/LinkedHashMap" => MAP,
        "java/util/HashSet" | "java/util/LinkedHashSet" => SET,
        _ => return false,
    };
    family.contains(&target)
}

/// The block-state `Property` subclasses whose `create` factory the runtime
/// interns: vanilla compares properties by identity.
fn is_property_class(class_name: &str) -> bool {
    matches!(
        class_name,
        "net/minecraft/world/level/block/state/properties/IntegerProperty"
            | "net/minecraft/world/level/block/state/properties/BooleanProperty"
            | "net/minecraft/world/level/block/state/properties/EnumProperty"
            | "net/minecraft/world/level/block/state/properties/DirectionProperty"
    )
}

/// The Minecraft registry types the runtime models with one object.
fn is_registry_model_class(class_name: &str) -> bool {
    matches!(
        class_name,
        "net/minecraft/core/Registry"
            | "net/minecraft/core/WritableRegistry"
            | "net/minecraft/core/DefaultedRegistry"
            | "net/minecraft/core/MappedRegistry"
            | "net/minecraft/core/DefaultedMappedRegistry"
            | "net/minecraft/core/IdMap"
            | "net/minecraft/core/HolderLookup$RegistryLookup"
    )
}

/// Whether an operand-stack value is a JVM category-2 value (`long`, `double`).
fn is_wide(value: &Value) -> bool {
    matches!(value, Value::Long(_) | Value::Double(_))
}

/// The JDK skeleton class a runtime-modelled collection behaves as, for the
/// methods the collection builtins do not answer themselves.
pub(crate) fn collection_skeleton(object: &JavaObject) -> Option<&'static str> {
    match object {
        JavaObject::List(_) => Some("java/util/AbstractList"),
        JavaObject::Set(_) => Some("java/util/AbstractSet"),
        JavaObject::Map(_) => Some("java/util/AbstractMap"),
        _ => None,
    }
}
