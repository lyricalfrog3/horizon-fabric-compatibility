//! `ValueOutput` and `ValueInput`: the storage view a `BlockEntity.saveAdditional`
//! writes through and `loadAdditional` reads back (26.3's NBT, seen through an
//! interface rather than `CompoundTag`).
//!
//! The runtime keeps a plain tree: an output object holds the values written to
//! it, `child(name)` makes a nested one, and an input object reads the same
//! shape back. Only what the interface names as primitives is modelled
//! (`putInt`/`getIntOr`, `putString`, `putBoolean`, `putIntArray`, `child`,
//! `childOrEmpty`, `discard`, `isEmpty`). The codec calls
//! (`store`/`storeNullable`/`read`/`list`) go through `Codec<T>`, which the
//! runtime does not implement, so they are reported as unresolved rather than
//! answered with made-up values.

use crate::heap::Heap;
use crate::value::{JavaObject, Value};
use std::collections::HashMap;

const OUTPUT: &str = "net/minecraft/world/level/storage/ValueOutput";
const INPUT: &str = "net/minecraft/world/level/storage/ValueInput";

/// One value written through a `ValueOutput`.
#[derive(Debug, Clone, PartialEq)]
pub enum StoredValue {
    Int(i32),
    Long(i64),
    Byte(i8),
    Short(i16),
    Float(f32),
    Double(f64),
    Bool(bool),
    Text(String),
    IntArray(Vec<i32>),
    Child(usize),
}

/// A `ValueOutput` or `ValueInput` over the runtime's heap: the values under
/// its own name, and the heap id of an output whose `child` was asked for.
#[derive(Debug)]
pub struct ValueTreeNode {
    /// `true` for an output (written to), `false` for an input (read from).
    pub output: bool,
    pub values: HashMap<String, StoredValue>,
}

impl crate::value::NativeObject for ValueTreeNode {
    fn class_name(&self) -> &str {
        if self.output { OUTPUT } else { INPUT }
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn held_ids(&self) -> Vec<usize> {
        // A `ValueOutput` child is a heap object (`StoredValue::Child`), so a
        // nested output a block entity wrote must survive the scope. The
        // scalars and text hold no ids.
        self.values
            .values()
            .filter_map(|value| match value {
                StoredValue::Child(id) => Some(*id),
                _ => None,
            })
            .collect()
    }
}

/// The values a finished output holds, for the host to store with the block
/// entity's own NBT.
pub fn output_values(heap: &Heap, id: usize) -> Option<&HashMap<String, StoredValue>> {
    if heap.get_native::<ValueTreeNode>(id).is_some() {
        return heap
            .get_native::<ValueTreeNode>(id)
            .map(|node| &node.values);
    }
    None
}

/// A `ValueOutput` for the host to fill: `putInt` and friends answer through
/// the builtins, as a mod's own code would have.
pub fn alloc_output(heap: &mut Heap) -> usize {
    heap.alloc_native(ValueTreeNode {
        output: true,
        values: HashMap::new(),
    })
}

/// Make an output that starts with `values`, so a `ValueInput` reads what a
/// previous output wrote.
pub fn alloc_input(heap: &mut Heap, values: HashMap<String, StoredValue>) -> usize {
    heap.alloc_native(ValueTreeNode {
        output: false,
        values,
    })
}

fn int_of(value: &Value) -> i32 {
    value.as_int()
}

fn text_of(heap: &Heap, value: &Value) -> String {
    value
        .as_obj()
        .and_then(|id| heap.get_string(id))
        .map(str::to_string)
        .unwrap_or_default()
}

/// A `ValueOutput`/`ValueInput` call. `None` when the runtime does not model
/// it, so the caller reports it as unresolved.
pub fn dispatch(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
    args: &[Value],
) -> Option<Value> {
    if class_name != OUTPUT
        && class_name != INPUT
        && class_name != "net/minecraft/world/level/storage/ValueOutput$ValueOutputList"
        && class_name != "net/minecraft/world/level/storage/ValueInput$ValueInputList"
    {
        return None;
    }
    if !matches!(class_name, OUTPUT | INPUT) {
        // The list interfaces are not modelled: their typed elements go
        // through a codec.
        return None;
    }
    let this = args.first()?.as_obj()?;
    let output = class_name == OUTPUT;
    if heap.get_native::<ValueTreeNode>(this).is_none() {
        return None;
    }
    let key = |index: usize| -> Option<String> {
        let value = args.get(index)?;
        Some(text_of(heap, value))
    };
    match (output, method_name) {
        // Writes.
        (true, "putInt") => {
            let (Some(k), Some(v)) = (key(1), args.get(2)) else {
                return None;
            };
            let v = int_of(v);
            heap.get_native_mut::<ValueTreeNode>(this)?
                .values
                .insert(k, StoredValue::Int(v));
            Some(Value::NULL)
        }
        (true, "putLong") => {
            let (Some(k), Some(v)) = (key(1), args.get(2)) else {
                return None;
            };
            let v = v.as_int() as i64;
            heap.get_native_mut::<ValueTreeNode>(this)?
                .values
                .insert(k, StoredValue::Long(v));
            Some(Value::NULL)
        }
        (true, "putByte") | (true, "putShort") => {
            let (Some(k), Some(v)) = (key(1), args.get(2)) else {
                return None;
            };
            let v = int_of(v);
            let stored = if method_name == "putByte" {
                StoredValue::Byte(v as i8)
            } else {
                StoredValue::Short(v as i16)
            };
            heap.get_native_mut::<ValueTreeNode>(this)?
                .values
                .insert(k, stored);
            Some(Value::NULL)
        }
        (true, "putFloat") | (true, "putDouble") => {
            let (Some(k), Some(v)) = (key(1), args.get(2)) else {
                return None;
            };
            let v = v.as_double();
            let stored = if method_name == "putFloat" {
                StoredValue::Float(v as f32)
            } else {
                StoredValue::Double(v)
            };
            heap.get_native_mut::<ValueTreeNode>(this)?
                .values
                .insert(k, stored);
            Some(Value::NULL)
        }
        (true, "putBoolean") => {
            let (Some(k), Some(v)) = (key(1), args.get(2)) else {
                return None;
            };
            heap.get_native_mut::<ValueTreeNode>(this)?
                .values
                .insert(k, StoredValue::Bool(v.as_bool()));
            Some(Value::NULL)
        }
        (true, "putString") => {
            let (Some(k), Some(v)) = (key(1), args.get(2)) else {
                return None;
            };
            let v = text_of(heap, v);
            heap.get_native_mut::<ValueTreeNode>(this)?
                .values
                .insert(k, StoredValue::Text(v));
            Some(Value::NULL)
        }
        (true, "putIntArray") => {
            let Some(k) = key(1) else { return None };
            let values = args
                .get(2)
                .and_then(Value::as_obj)
                .and_then(|id| match heap.get(id) {
                    Some(JavaObject::Array { elements, .. }) => Some(elements.clone()),
                    _ => None,
                })
                .unwrap_or_default()
                .iter()
                .map(Value::as_int)
                .collect();
            heap.get_native_mut::<ValueTreeNode>(this)?
                .values
                .insert(k, StoredValue::IntArray(values));
            Some(Value::NULL)
        }
        // Nested outputs and inputs.
        (true, "child") => {
            let k = key(1)?;
            let child = heap.alloc_native(ValueTreeNode {
                output: true,
                values: HashMap::new(),
            });
            heap.get_native_mut::<ValueTreeNode>(this)?
                .values
                .insert(k, StoredValue::Child(child));
            Some(Value::Object(Some(child)))
        }
        (false, "childOrEmpty") => {
            let k = key(1)?;
            let child = match heap.get_native::<ValueTreeNode>(this)?.values.get(&k) {
                Some(StoredValue::Child(id)) => *id,
                _ => heap.alloc_native(ValueTreeNode {
                    output: false,
                    values: HashMap::new(),
                }),
            };
            // The input child reads the same values; share them.
            let values = heap
                .get_native::<ValueTreeNode>(child)
                .map(|node| node.values.clone())
                .unwrap_or_default();
            if heap.get_native::<ValueTreeNode>(child).is_none() {
                heap.get_native_mut::<ValueTreeNode>(child)?
                    .values
                    .extend(values);
            }
            Some(Value::Object(Some(child)))
        }
        (false, "isEmpty") => Some(Value::from_bool(
            heap.get_native::<ValueTreeNode>(this)?.values.is_empty(),
        )),
        (true, "isEmpty") => Some(Value::from_bool(
            heap.get_native::<ValueTreeNode>(this)?.values.is_empty(),
        )),
        (true, "discard") => {
            if let Some(k) = key(1) {
                heap.get_native_mut::<ValueTreeNode>(this)?
                    .values
                    .remove(&k);
            }
            Some(Value::NULL)
        }
        // Reads.
        (false, method) if method.starts_with("get") && method.ends_with("Or") => {
            let k = key(1)?;
            let fallback = args.get(2).cloned().unwrap_or(Value::Int(0));
            let stored = heap
                .get_native::<ValueTreeNode>(this)?
                .values
                .get(&k)
                .cloned();
            let value = match (method, stored) {
                ("getIntOr", Some(StoredValue::Int(v))) => Value::Int(v),
                ("getLongOr", Some(StoredValue::Long(v))) => Value::Long(v),
                ("getLongOr", Some(StoredValue::Int(v))) => Value::Long(i64::from(v)),
                ("getByteOr", Some(StoredValue::Byte(v))) => Value::Int(i32::from(v)),
                ("getShortOr", Some(StoredValue::Short(v))) => Value::Int(i32::from(v)),
                ("getFloatOr", Some(StoredValue::Float(v))) => Value::Float(v),
                ("getDoubleOr", Some(StoredValue::Double(v))) => Value::Double(v),
                ("getBooleanOr", Some(StoredValue::Bool(v))) => Value::from_bool(v),
                ("getStringOr", Some(StoredValue::Text(v))) => {
                    Value::Object(Some(heap.alloc_string(v)))
                }
                ("getStringOr", Some(_)) => return None,
                (_, Some(_)) => return None,
                (_, None) => fallback,
            };
            Some(value)
        }
        (false, "getIntArray") => {
            let k = key(1)?;
            let values = match heap.get_native::<ValueTreeNode>(this)?.values.get(&k) {
                Some(StoredValue::IntArray(values)) => values.clone(),
                _ => return None,
            };
            let array = heap.alloc(JavaObject::Array {
                elem_type: "I".into(),
                elements: values.into_iter().map(Value::Int).collect(),
            });
            Some(Value::Object(Some(array)))
        }
        (false, "getBoolean") | (false, "getInt") | (false, "getString") => {
            // `ValueInput.get*(key)` is an `Option`; the caller uses the `Or`
            // form in practice, and an absent read must not invent a value.
            None
        }
        _ => None,
    }
}

/// A tree's values, flattened: `"CookTime"` for a top-level value and
/// `"Inventory/Slot0"` for one under a child, so a host can store the whole
/// tree depth-first without holding heap ids across calls.
pub fn flat_values(heap: &Heap, id: usize) -> Vec<(String, StoredValue)> {
    fn walk(
        heap: &Heap,
        id: usize,
        prefix: &str,
        out: &mut Vec<(String, StoredValue)>,
        depth: usize,
    ) {
        if depth > 16 {
            return;
        }
        let Some(node) = heap.get_native::<ValueTreeNode>(id) else {
            return;
        };
        let mut keys: Vec<&String> = node.values.keys().collect();
        keys.sort();
        for key in keys {
            let Some(value) = node.values.get(key) else {
                continue;
            };
            let name = format!("{prefix}{key}");
            match value {
                StoredValue::Child(child) => {
                    walk(heap, *child, &format!("{name}/"), out, depth + 1)
                }
                other => out.push((name, other.clone())),
            }
        }
    }
    let mut out = Vec::new();
    walk(heap, id, "", &mut out, 0);
    out
}

/// A `ValueInput` tree from a flattened one (`flat_values`'s shape): each
/// `a/b/c` path becomes children named `a`, `b` and the value `c`.
pub fn input_from_flat(heap: &mut Heap, values: &[(String, StoredValue)]) -> usize {
    let root = heap.alloc_native(ValueTreeNode {
        output: false,
        values: HashMap::new(),
    });
    for (path, value) in values {
        let parts: Vec<&str> = path.split('/').collect();
        let mut current = root;
        for part in &parts[..parts.len() - 1] {
            let existing = heap.get_native::<ValueTreeNode>(current).and_then(|node| {
                match node.values.get(*part) {
                    Some(StoredValue::Child(id)) => Some(*id),
                    _ => None,
                }
            });
            let child = match existing {
                Some(child) => child,
                None => {
                    let child = heap.alloc_native(ValueTreeNode {
                        output: false,
                        values: HashMap::new(),
                    });
                    if let Some(node) = heap.get_native_mut::<ValueTreeNode>(current) {
                        node.values
                            .insert((*part).to_string(), StoredValue::Child(child));
                    }
                    child
                }
            };
            current = child;
        }
        if let Some(node) = heap.get_native_mut::<ValueTreeNode>(current) {
            node.values
                .insert(parts[parts.len() - 1].to_string(), value.clone());
        }
    }
    root
}

/// Every name/primitive pair of a stored tree, for a test assertion.
#[doc(hidden)]
pub fn flatten(heap: &Heap, id: usize, prefix: &str, out: &mut Vec<String>) {
    let Some(node) = heap.get_native::<ValueTreeNode>(id) else {
        return;
    };
    for (key, value) in &node.values {
        let name = format!("{prefix}{key}");
        match value {
            StoredValue::Child(child) => flatten(heap, *child, &format!("{name}/"), out),
            StoredValue::Int(v) => out.push(format!("{name}={v}")),
            StoredValue::Long(v) => out.push(format!("{name}={v}")),
            StoredValue::Byte(v) => out.push(format!("{name}={v}")),
            StoredValue::Short(v) => out.push(format!("{name}={v}")),
            StoredValue::Float(v) => out.push(format!("{name}={v}")),
            StoredValue::Double(v) => out.push(format!("{name}={v}")),
            StoredValue::Bool(v) => out.push(format!("{name}={v}")),
            StoredValue::Text(v) => out.push(format!("{name}={v}")),
            StoredValue::IntArray(v) => out.push(format!("{name}={v:?}")),
        }
    }
}
