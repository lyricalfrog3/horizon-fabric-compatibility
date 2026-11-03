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
    )
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
    /// Optional instruction budget. `None` runs without a limit, as a JVM does.
    /// A finite budget keeps a mis-decoded branch from hanging the host.
    pub step_budget: Option<u64>,
    pub call_depth: usize,
    /// When set, every entered method is pushed onto [`Self::call_stack`] so a
    /// budget exhaustion can name the Java frames it happened under. Off in
    /// normal runs: the bookkeeping allocates a string per call.
    pub trace_calls: bool,
    pub call_stack: Vec<String>,
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
            missing: Vec::new(),
            step_budget: None,
            call_depth: 0,
            trace_calls: false,
            call_stack: Vec::new(),
        }
    }

    pub fn with_loader(loader: ClassLoader) -> Self {
        Self {
            loader,
            heap: Heap::new(),
            static_singletons: HashMap::new(),
            trace_missing: false,
            missing: Vec::new(),
            step_budget: None,
            call_depth: 0,
            trace_calls: false,
            call_stack: Vec::new(),
        }
    }

    fn note_missing(&mut self, what: String) {
        if self.trace_missing {
            let caller = if self.trace_calls {
                self.call_stack
                    .last()
                    .map(|frame| format!(" <- {frame}"))
                    .unwrap_or_default()
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
                if class
                    .methods
                    .contains_key(&("<clinit>".into(), "()V".into()))
                {
                    let _ = self.execute_special_method(class_name, "<clinit>", "()V", &[]);
                }
            }
        }
        Ok(())
    }

    /// Check if a value is an instance of the target type.
    pub fn is_instance_of(&mut self, obj_val: &Value, target_type: &str) -> bool {
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
            JavaObject::Array { .. } => {
                target_type.starts_with('[') || target_type == "java/lang/Object"
            }
            JavaObject::Native(n) => {
                let c = n.class_name();
                c == target_type
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

    fn execute_method_inner(
        &mut self,
        class_name: &str,
        method_name: &str,
        descriptor: &str,
        args: &[Value],
        virtual_dispatch: bool,
    ) -> Result<Value> {
        log::trace!("Execute: {class_name}.{method_name}{descriptor}");

        // If this invocation is on a Lambda object, unpack and delegate to the lambda target
        if let Some(first_arg) = args.first() {
            if let Some(this_id) = first_arg.as_obj() {
                if !matches!(method_name, "getClass" | "toString" | "hashCode" | "equals") {
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
                        return self.execute_method(&t_class, &t_method, &t_desc, &all_args);
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
        if class_name == "java/util/stream/Stream" && !args.is_empty() {
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
                    "map" => {
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
                        let kind = args
                            .get(1)
                            .and_then(|v| v.as_obj())
                            .and_then(|id| match self.heap.get(id) {
                                Some(JavaObject::Instance { fields, .. }) => {
                                    fields.get("type").and_then(|v| v.as_obj())
                                }
                                _ => None,
                            })
                            .and_then(|id| self.heap.get_string(id))
                            .unwrap_or("toList")
                            .to_string();
                        return Ok(match kind.as_str() {
                            "toSet" => Value::Object(Some(self.heap.alloc_set(items))),
                            "joining" => {
                                let joined = items
                                    .iter()
                                    .filter_map(|v| {
                                        v.as_obj().and_then(|id| self.heap.get_string(id))
                                    })
                                    .collect::<Vec<_>>()
                                    .join("");
                                Value::Object(Some(self.heap.alloc_string(joined)))
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
        if virtual_dispatch && method_name != "<init>" {
            if let Some(first) = args.first() {
                if let Some(id) = first.as_obj() {
                    if let Some(JavaObject::Instance {
                        class_name: actual, ..
                    }) = self.heap.get(id)
                    {
                        if actual != class_name {
                            queue.push_back(actual.clone());
                        }
                    }
                }
            }
        }
        queue.push_back(class_name.to_string());

        let mut visited = std::collections::HashSet::new();
        let (this_class, bsm, code) = 'lookup: loop {
            let Some(curr) = queue.pop_front() else {
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
        for (i, arg) in args.iter().enumerate() {
            if i < locals.len() {
                locals[i] = arg.clone();
            }
        }

        let mut stack: Vec<Value> = Vec::with_capacity(code.max_stack as usize);
        let mut pc = 0;
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
                    stack.push(Value::Int(if b == 0 { 0 } else { a / b }));
                }
                Opcode::Irem => {
                    let b = stack.pop().unwrap_or(Value::Int(1)).as_int();
                    let a = stack.pop().unwrap_or(Value::Int(0)).as_int();
                    stack.push(Value::Int(if b == 0 { 0 } else { a % b }));
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
                    stack.push(Value::Long(if b == 0 { 0 } else { a.wrapping_div(b) }));
                }
                Opcode::Lrem => {
                    let b = stack.pop().unwrap_or(Value::Long(1)).as_long();
                    let a = stack.pop().unwrap_or(Value::Long(0)).as_long();
                    stack.push(Value::Long(if b == 0 { 0 } else { a.wrapping_rem(b) }));
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
                    if let Some(c) = self.loader.get_mut(class_name) {
                        c.static_fields.insert(field_name.clone(), val);
                    }
                }
                Opcode::Getfield { field_name, .. } => {
                    let obj_ref = stack.pop().unwrap_or(Value::NULL);
                    let val = if let Some(id) = obj_ref.as_obj() {
                        if let Some(JavaObject::Instance { fields, .. }) = self.heap.get(id) {
                            fields.get(field_name).cloned().unwrap_or(Value::NULL)
                        } else {
                            Value::NULL
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
                    let id = match cls.as_str() {
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
                            class_name: cls.clone(),
                            fields: HashMap::new(),
                        }),
                    };
                    stack.push(Value::Object(Some(id)));
                }
                Opcode::Newarray(elem) | Opcode::Anewarray(elem) => {
                    let count = stack.pop().unwrap_or(Value::Int(0)).as_int().max(0) as usize;
                    let default_val = match elem.as_str() {
                        "boolean" | "byte" | "short" | "int" => Value::Int(0),
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
                    let idx = stack.pop().unwrap_or(Value::Int(0)).as_int().max(0) as usize;
                    let arr_ref = stack.pop().unwrap_or(Value::NULL);
                    let val = if let Some(id) = arr_ref.as_obj() {
                        if let Some(JavaObject::Array { elements, .. }) = self.heap.get(id) {
                            elements.get(idx).cloned().unwrap_or(Value::NULL)
                        } else {
                            Value::NULL
                        }
                    } else {
                        Value::NULL
                    };
                    stack.push(val);
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
                    let idx = stack.pop().unwrap_or(Value::Int(0)).as_int().max(0) as usize;
                    let arr_ref = stack.pop().unwrap_or(Value::NULL);
                    if let Some(id) = arr_ref.as_obj() {
                        if let Some(JavaObject::Array { elements, .. }) = self.heap.get_mut(id) {
                            if idx < elements.len() {
                                elements[idx] = val;
                            }
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
                    call_args.insert(0, this_ref);

                    let ret =
                        self.execute_method(class_name, method_name, descriptor, &call_args)?;

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
                    // Rethrow or return NULL
                    return Ok(Value::NULL);
                }
            }
        }

        Ok(stack.pop().unwrap_or(Value::NULL))
    }

    /// Resolve static field value or create fallback singleton.
    pub fn resolve_static_field(&mut self, class_name: &str, field_name: &str) -> Value {
        let key = (class_name.to_string(), field_name.to_string());
        if let Some(val) = self.static_singletons.get(&key) {
            return val.clone();
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

        if field_name == "NEXT"
            || field_name == "PREV"
            || field_name == "HEAD"
            || field_name == "TAIL"
            || field_name == "ITEM"
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

        if class_name == "io/github/jamalam360/rightclickharvest/Config$HungerLevel" {
            let mut f = HashMap::new();
            f.insert("modifier".into(), Value::Float(0.0));
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: class_name.into(),
                fields: f,
            });
            return Value::Object(Some(id));
        }

        if class_name == "io/github/jamalam360/rightclickharvest/Config$ExperienceType" {
            let mut f = HashMap::new();
            f.insert("ordinal".into(), Value::Int(2)); // NONE
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: class_name.into(),
                fields: f,
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
