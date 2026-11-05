//! Bytecode interpreter for JVM classes and lambda closures.

use crate::builtins::{BuiltinResult, dispatch_builtin};
use crate::class::{BootstrapMethodInfo, CodeInfo, Opcode};
use crate::heap::Heap;
use crate::loader::ClassLoader;
use crate::minecraft::*;
use crate::value::{JavaObject, Value};
use hfc_core::{Error, Result};
use std::collections::HashMap;

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
    pub call_stack: Vec<String>,
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
            call_stack: Vec::new(),
            menu_injection: false,
            injection_depth: 0,
            menu_contributions: Vec::new(),
            mixin_accessors: HashMap::new(),
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
            call_stack: Vec::new(),
            menu_injection: false,
            injection_depth: 0,
            menu_contributions: Vec::new(),
            mixin_accessors: HashMap::new(),
        }
    }

    fn note_missing(&mut self, what: String) {
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
            if let Some(class) = self.loader.get(class_name) {
                // `Language` is replaced wholesale (translations come from the
                // host), so its initializer, which reads en_us.json through JDK
                // stream decoders, must not run.
                if class_name != "net/minecraft/locale/Language"
                    && class
                        .methods
                        .contains_key(&("<clinit>".into(), "()V".into()))
                {
                    // A throwing initializer leaves the class half-initialized (a JVM
                    // would raise ExceptionInInitializerError on the next use); say so.
                    if let Err(error) =
                        self.execute_special_method(class_name, "<clinit>", "()V", &[])
                    {
                        log::warn!("static initializer of {class_name} failed: {error}");
                    }
                }
            }
        }
        Ok(())
    }

    /// Check if a value is an instance of the target type.
    pub fn is_instance_of(&mut self, obj_val: &Value, target_type: &str) -> bool {
        // A boxed number is a plain value; it is an instance of its wrapper.
        if !matches!(obj_val, Value::Object(_)) {
            return match obj_val {
                Value::Int(_) => matches!(
                    target_type,
                    "java/lang/Integer" | "java/lang/Number" | "java/lang/Comparable"
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
            JavaObject::List(_) => {
                matches!(
                    target_type,
                    "java/util/List" | "java/util/Collection" | "java/lang/Iterable"
                )
            }
            JavaObject::Map(_) => {
                matches!(
                    target_type,
                    "java/util/Map" | "java/util/concurrent/ConcurrentMap"
                )
            }
            JavaObject::Set(_) => {
                matches!(
                    target_type,
                    "java/util/Set" | "java/util/Collection" | "java/lang/Iterable"
                )
            }
            JavaObject::Stream(_) => {
                matches!(
                    target_type,
                    "java/util/stream/Stream" | "java/util/stream/BaseStream"
                )
            }
            JavaObject::Iterator { .. } => {
                matches!(target_type, "java/util/Iterator")
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
            JavaObject::Native(n) => {
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
            return Err(Error::new(
                "interpreter",
                format!("call stack overflow at {class_name}.{method_name}{descriptor}"),
            ));
        }
        self.call_depth += 1;
        let traced = self.trace_calls;
        if traced {
            self.call_stack
                .push(format!("{class_name}.{method_name}{descriptor}"));
        }
        let res =
            self.execute_method_inner(class_name, method_name, descriptor, args, virtual_dispatch);
        if traced {
            self.call_stack.pop();
        }
        self.call_depth -= 1;
        res
    }

    /// Whether `class_name` declares `method_name`/`descriptor` as a static method.
    fn is_static_method(&self, class_name: &str, method_name: &str, descriptor: &str) -> bool {
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
    pub(crate) fn alloc_uninitialized(&mut self, cls: &str) -> usize {
        match cls {
            "java/util/ArrayList" | "java/util/LinkedList" => {
                self.heap.alloc(JavaObject::List(Vec::new()))
            }
            "java/util/HashMap"
            | "java/util/LinkedHashMap"
            | "java/util/TreeMap"
            | "java/util/concurrent/ConcurrentHashMap" => {
                self.heap.alloc(JavaObject::Map(Vec::new()))
            }
            "java/util/HashSet" | "java/util/LinkedHashSet" | "java/util/TreeSet" => {
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
    #[inline(never)]
    fn redirect_mixin_accessor(
        &mut self,
        class_name: &str,
        method_name: &str,
        descriptor: &str,
        args: &[Value],
    ) -> Option<Result<Value>> {
        use crate::mixin::AccessorKind;
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
        let receiver = args.first()?.as_obj();
        Some(match accessor.kind {
            AccessorKind::Invoker { method } => {
                self.execute_method(&accessor.target_class, &method, descriptor, args)
            }
            AccessorKind::Getter { field } => Ok(receiver
                .and_then(|id| match self.heap.get(id) {
                    Some(JavaObject::Instance { fields, .. }) => fields.get(&field).cloned(),
                    _ => None,
                })
                .unwrap_or(Value::NULL)),
            AccessorKind::Setter { field } => {
                if let (Some(id), Some(value)) = (receiver, args.get(1)) {
                    if let Some(JavaObject::Instance { fields, .. }) = self.heap.get_mut(id) {
                        fields.insert(field, value.clone());
                    }
                }
                Ok(Value::NULL)
            }
        })
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
                if !matches!(method_name, "getClass" | "toString" | "hashCode" | "equals")
                    && !self.is_static_method(class_name, method_name, descriptor)
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
                    let existing = |key: &Value| {
                        entries
                            .iter()
                            .find(|(k, _)| k == key)
                            .map(|(_, v)| v.clone())
                            .filter(|v| !v.is_null())
                    };
                    let store = |interp: &mut Self, key: Value, value: Value| {
                        if let Some(JavaObject::Map(entries)) = interp.heap.get_mut(map_id) {
                            match entries.iter_mut().find(|(k, _)| *k == key) {
                                Some((_, v)) => *v = value,
                                None => entries.push((key, value)),
                            }
                        }
                    };
                    let remove = |interp: &mut Self, key: &Value| {
                        if let Some(JavaObject::Map(entries)) = interp.heap.get_mut(map_id) {
                            entries.retain(|(k, _)| k != key);
                        }
                    };
                    const FUNCTION: &str = "(Ljava/lang/Object;)Ljava/lang/Object;";
                    const BIFUNCTION: &str =
                        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;";
                    match method_name {
                        "computeIfAbsent" if args.len() >= 3 => {
                            if let Some(value) = existing(&args[1]) {
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
                            let old = existing(&args[1]);
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
                            let value = match existing(&args[1]) {
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
                    "collect" => {
                        let collector = args.get(1).cloned().unwrap_or(Value::NULL);
                        let (kind, collector_args) =
                            match collector.as_obj().and_then(|id| self.heap.get(id)) {
                                Some(JavaObject::Instance { fields, .. }) => {
                                    let kind = fields
                                        .get("type")
                                        .and_then(|v| v.as_obj())
                                        .and_then(|id| self.heap.get_string(id))
                                        .unwrap_or("toList")
                                        .to_string();
                                    let collector_args: Vec<Value> = (0..4)
                                        .map_while(|n| fields.get(&format!("arg{n}")).cloned())
                                        .collect();
                                    (kind, collector_args)
                                }
                                _ => ("toList".to_string(), Vec::new()),
                            };
                        let text = |interp: &Self, value: Option<&Value>| -> String {
                            value
                                .and_then(|v| v.as_obj())
                                .and_then(|id| interp.heap.get_string(id))
                                .unwrap_or("")
                                .to_string()
                        };
                        return Ok(match kind.as_str() {
                            "toSet" => Value::Object(Some(self.heap.alloc_set(items))),
                            "joining" => {
                                let parts = items
                                    .iter()
                                    .filter_map(|v| {
                                        v.as_obj().and_then(|id| self.heap.get_string(id))
                                    })
                                    .collect::<Vec<_>>()
                                    .join(&text(self, collector_args.first()));
                                let joined = format!(
                                    "{}{parts}{}",
                                    text(self, collector_args.get(1)),
                                    text(self, collector_args.get(2))
                                );
                                Value::Object(Some(self.heap.alloc_string(joined)))
                            }
                            "toCollection" if !collector_args.is_empty() => {
                                let target = self.execute_method(
                                    "java/util/function/Supplier",
                                    "get",
                                    "()Ljava/lang/Object;",
                                    &[collector_args[0].clone()],
                                )?;
                                for item in items {
                                    self.execute_method(
                                        "java/util/Collection",
                                        "add",
                                        "(Ljava/lang/Object;)Z",
                                        &[target.clone(), item],
                                    )?;
                                }
                                target
                            }
                            "toMap" if collector_args.len() >= 2 => {
                                let target = match collector_args.get(3) {
                                    Some(supplier) => self.execute_method(
                                        "java/util/function/Supplier",
                                        "get",
                                        "()Ljava/lang/Object;",
                                        &[supplier.clone()],
                                    )?,
                                    None => Value::Object(Some(
                                        self.heap.alloc(JavaObject::Map(Vec::new())),
                                    )),
                                };
                                for item in items {
                                    let key = self.execute_method(
                                        "java/util/function/Function",
                                        "apply",
                                        "(Ljava/lang/Object;)Ljava/lang/Object;",
                                        &[collector_args[0].clone(), item.clone()],
                                    )?;
                                    let value = self.execute_method(
                                        "java/util/function/Function",
                                        "apply",
                                        "(Ljava/lang/Object;)Ljava/lang/Object;",
                                        &[collector_args[1].clone(), item],
                                    )?;
                                    self.execute_method(
                                        "java/util/Map",
                                        "put",
                                        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                                        &[target.clone(), key, value],
                                    )?;
                                }
                                target
                            }
                            _ => Value::Object(Some(self.heap.alloc_list(items))),
                        });
                    }
                    _ => {}
                }
            }
        }

        // Comparator built from an int-key function, and its reverse. The
        // actual comparison stays here so the key function can be Java.
        if class_name == "java/util/Comparator" && !args.is_empty() {
            if method_name == "comparingInt" {
                let function = args.get(1).cloned().unwrap_or(Value::NULL);
                let mut fields = HashMap::new();
                fields.insert("function".into(), function);
                let id = self.heap.alloc(JavaObject::Instance {
                    class_name: "java/util/Comparator".into(),
                    fields,
                });
                return Ok(Value::Object(Some(id)));
            }
            if method_name == "reversed" {
                let mut fields = HashMap::new();
                fields.insert("reversed".into(), args[0].clone());
                let id = self.heap.alloc(JavaObject::Instance {
                    class_name: "java/util/Comparator".into(),
                    fields,
                });
                return Ok(Value::Object(Some(id)));
            }
            if method_name == "compare" && args.len() >= 3 {
                let fields = match self.heap.get(args[0].as_obj().unwrap_or(usize::MAX)) {
                    Some(JavaObject::Instance { fields, .. }) => fields.clone(),
                    _ => HashMap::new(),
                };
                if let Some(inner) = fields.get("reversed").cloned() {
                    let inner_order = self.execute_method(
                        "java/util/Comparator",
                        "compare",
                        "(Ljava/lang/Object;Ljava/lang/Object;)I",
                        &[inner, args[1].clone(), args[2].clone()],
                    )?;
                    return Ok(Value::Int(-inner_order.as_int()));
                }
                if let Some(function) = fields.get("function").cloned() {
                    let left = self.execute_method(
                        "java/util/function/ToIntFunction",
                        "applyAsInt",
                        "(Ljava/lang/Object;)I",
                        &[function.clone(), args[1].clone()],
                    )?;
                    let right = self.execute_method(
                        "java/util/function/ToIntFunction",
                        "applyAsInt",
                        "(Ljava/lang/Object;)I",
                        &[function, args[2].clone()],
                    )?;
                    let (left, right) = (left.as_int(), right.as_int());
                    return Ok(Value::Int(if left < right {
                        -1
                    } else if left > right {
                        1
                    } else {
                        0
                    }));
                }
                // A lambda comparator is unpacked by the branch at the top.
            }
        }

        if class_name == "java/lang/Thread" && (method_name == "start" || method_name == "start0") {
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
                    if let Some(JavaObject::Instance {
                        class_name: actual, ..
                    }) = self.heap.get(id)
                    {
                        if actual != class_name {
                            queue.push_back(actual.clone());
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
                    "Method not found in hierarchy of {class_name}: {method_name}{descriptor}"
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

    /// An exception raised at `byte_offset`: when this method has a handler
    /// for it, the pc to continue at, with the stack holding the exception.
    ///
    /// Handlers are frame-local. An exception nothing in the same method
    /// catches does not unwind into the caller: the real JDK, Netty and
    /// Minecraft code a mod pulls in throws many exceptions the runtime
    /// answers elsewhere with defaults, and unwinding them all would abort
    /// those classes' initializers. Callers then take the old lenient path.
    fn catch_locally(
        &mut self,
        code: &CodeInfo,
        byte_offset: usize,
        exception: Value,
        stack: &mut Vec<Value>,
    ) -> Option<usize> {
        let handler = self.find_handler(code, byte_offset, &exception)?;
        stack.clear();
        stack.push(exception);
        Some(find_target_pc(&code.opcodes, handler, 0))
    }

    /// Allocate an exception of `class`, but only when a handler of this method
    /// covers `byte_offset`; see [`Self::catch_locally`].
    fn raise_if_caught(
        &mut self,
        code: &CodeInfo,
        byte_offset: usize,
        class: &str,
        message: &str,
        stack: &mut Vec<Value>,
    ) -> Option<usize> {
        if !code
            .handlers
            .iter()
            .any(|entry| byte_offset >= entry.start && byte_offset < entry.end)
        {
            return None;
        }
        let mut fields = HashMap::new();
        let text = self.heap.alloc_string(message);
        fields.insert("message".to_string(), Value::Object(Some(text)));
        let id = self.heap.alloc(JavaObject::Instance {
            class_name: class.to_string(),
            fields,
        });
        self.catch_locally(code, byte_offset, Value::Object(Some(id)), stack)
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
        while pc < opcodes.len() {
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
                Opcode::DupX2 => {
                    let len = stack.len();
                    if len >= 3 {
                        let top = stack[len - 1].clone();
                        stack.insert(len - 3, top);
                    }
                }
                Opcode::Dup2 => {
                    let len = stack.len();
                    if len >= 2 {
                        let a = stack[len - 2].clone();
                        let b = stack[len - 1].clone();
                        stack.push(a);
                        stack.push(b);
                    }
                }
                Opcode::Dup2X1 => {
                    let len = stack.len();
                    if len >= 3 {
                        let a = stack[len - 2].clone();
                        let b = stack[len - 1].clone();
                        stack.insert(len - 3, b);
                        stack.insert(len - 3, a);
                    }
                }
                Opcode::Dup2X2 => {
                    let len = stack.len();
                    if len >= 4 {
                        let a = stack[len - 2].clone();
                        let b = stack[len - 1].clone();
                        stack.insert(len - 4, b);
                        stack.insert(len - 4, a);
                    }
                }
                Opcode::Pop => {
                    stack.pop();
                }
                Opcode::Pop2 => {
                    stack.pop();
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
                        if let Some(target) = self.raise_if_caught(
                            code,
                            byte_offset,
                            "java/lang/ArithmeticException",
                            "/ by zero",
                            stack,
                        ) {
                            pc = target;
                            continue;
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
                        if let Some(target) = self.raise_if_caught(
                            code,
                            byte_offset,
                            "java/lang/ArithmeticException",
                            "/ by zero",
                            stack,
                        ) {
                            pc = target;
                            continue;
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
                        if let Some(target) = self.raise_if_caught(
                            code,
                            byte_offset,
                            "java/lang/ArithmeticException",
                            "/ by zero",
                            stack,
                        ) {
                            pc = target;
                            continue;
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
                        if let Some(target) = self.raise_if_caught(
                            code,
                            byte_offset,
                            "java/lang/ArithmeticException",
                            "/ by zero",
                            stack,
                        ) {
                            pc = target;
                            continue;
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
                    if !has_synthetic_statics(class_name) {
                        self.ensure_initialized(class_name)?;
                    }
                    let val = self.resolve_static_field(class_name, field_name);
                    stack.push(val);
                }
                Opcode::Putstatic {
                    class_name,
                    field_name,
                    ..
                } => {
                    self.ensure_initialized(class_name)?;
                    let val = stack.pop().unwrap_or(Value::NULL);
                    // An inherited static field belongs to the class that
                    // declares it; writing it through the subclass must not
                    // create a second slot.
                    let owner = self
                        .static_field_owner(class_name, field_name)
                        .unwrap_or_else(|| class_name.clone());
                    if let Some(c) = self.loader.get_mut(&owner) {
                        c.static_fields.insert(field_name.clone(), val);
                    }
                }
                Opcode::Getfield {
                    field_name,
                    descriptor,
                    ..
                } => {
                    let obj_ref = stack.pop().unwrap_or(Value::NULL);
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
                Opcode::Putfield { field_name, .. } => {
                    let val = stack.pop().unwrap_or(Value::NULL);
                    let obj_ref = stack.pop().unwrap_or(Value::NULL);
                    if let Some(id) = obj_ref.as_obj() {
                        if let Some(JavaObject::Instance { fields, .. }) = self.heap.get_mut(id) {
                            fields.insert(field_name.clone(), val);
                        }
                    }
                }

                Opcode::New(cls) => {
                    self.ensure_initialized(cls)?;
                    let id = self.alloc_uninitialized(cls);
                    stack.push(Value::Object(Some(id)));
                }
                Opcode::Newarray(elem) | Opcode::Anewarray(elem) => {
                    let count = stack.pop().unwrap_or(Value::Int(0)).as_int().max(0) as usize;
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
                    let len = if let Some(id) = arr_ref.as_obj() {
                        if let Some(JavaObject::Array { elements, .. }) = self.heap.get(id) {
                            elements.len() as i32
                        } else {
                            0
                        }
                    } else {
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
                        if let Some(target) =
                            self.raise_if_caught(code, byte_offset, class, &message, stack)
                        {
                            pc = target;
                            continue;
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
                        if let Some(target) =
                            self.raise_if_caught(code, byte_offset, class, &message, stack)
                        {
                            pc = target;
                            continue;
                        }
                    }
                }

                Opcode::Checkcast(_) => {
                    // Checkcast passes the top of stack through
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
                    // A method of `Class` on null throws, as in a JVM. Reflection
                    // walks (`c = c.getSuperclass()` until a field is found) end
                    // on that exception; answering null instead loops forever.
                    if this_ref.is_null() && class_name == "java/lang/Class" {
                        let message = format!(
                            "Cannot invoke \"Class.{method_name}()\" because value is null"
                        );
                        if let Some(target) = self.raise_if_caught(
                            code,
                            byte_offset,
                            "java/lang/NullPointerException",
                            &message,
                            stack,
                        ) {
                            pc = target;
                            continue;
                        }
                        return Err(Error::new(
                            "interpreter",
                            format!("java.lang.NullPointerException: {message}"),
                        ));
                    }
                    call_args.insert(0, this_ref);

                    let ret =
                        self.execute_method(class_name, method_name, descriptor, &call_args)?;
                    self.record_menu_child(method_name, &call_args);

                    if !descriptor.ends_with(")V") {
                        stack.push(ret);
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
                        let mut out = String::new();
                        if target_method == "makeConcatWithConstants" {
                            if let Some(recipe) = strings.first() {
                                let mut dynamic = 0usize;
                                let mut constant = 0usize;
                                for ch in recipe.chars() {
                                    if ch == '\u{1}' {
                                        if let Some(value) = captured.get(dynamic) {
                                            let text = self.concat_text(value);
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
                            for value in &captured {
                                let text = self.concat_text(value);
                                out.push_str(&text);
                            }
                        }
                        let sid = self.heap.alloc_string(out);
                        stack.push(Value::Object(Some(sid)));
                        continue;
                    }

                    let return_type = extract_return_type(descriptor);
                    let lambda_id = self.heap.alloc(JavaObject::Lambda {
                        interface_class: return_type,
                        target_class,
                        target_method,
                        target_desc,
                        captured,
                    });
                    stack.push(Value::Object(Some(lambda_id)));
                }

                Opcode::Athrow => {
                    let exception = stack.pop().unwrap_or(Value::NULL);
                    if let Some(target) = self.catch_locally(code, byte_offset, exception, stack) {
                        pc = target;
                        continue;
                    }
                    // Not caught in this method: end it, as before exceptions existed.
                    return Ok(Value::NULL);
                }
            }
        }

        Ok(stack.pop().unwrap_or(Value::NULL))
    }

    /// The text string concatenation appends for `value`: a mod class's own
    /// `toString()` when it has one, otherwise the runtime's rendering.
    fn concat_text(&mut self, value: &Value) -> String {
        let class = match value.as_obj().and_then(|id| self.heap.get(id)) {
            Some(JavaObject::Instance { class_name, .. }) => Some(class_name.clone()),
            _ => None,
        };
        if let Some(class) = class {
            let mut current = Some(class);
            while let Some(name) = current {
                if ["java/", "jdk/", "sun/", "net/minecraft/"]
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
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

    /// The class that declares `field_name` as a static field, following the
    /// superclass chain. A JVM resolves `getstatic Subclass.inheritedField`
    /// against the class that actually declares it, so both `Subclass.x` and
    /// `Superclass.x` name one storage slot.
    fn static_field_owner(&mut self, class_name: &str, field_name: &str) -> Option<String> {
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
                if !platform && class.fields.get(field_name).is_some_and(|f| f.is_static) {
                    return Value::NULL;
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

        if class_name == "net/minecraft/world/item/Items" {
            let item_id = format!("minecraft:{}", field_name.to_lowercase());
            let id = self.heap.alloc_native(Item { id: item_id });
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
            let id = self.heap.alloc_native(TagKey {
                registry: class_name.to_string(),
                id: field_name.to_lowercase(),
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
