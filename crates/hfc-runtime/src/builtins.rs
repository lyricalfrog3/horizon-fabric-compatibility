//! Built-in shims for Java standard library, Fabric API, and Minecraft runtime classes.

use crate::heap::Heap;
use crate::minecraft::*;
use crate::value::{JavaObject, Value};
use std::collections::HashMap;

/// Result of dispatching a method to builtins.
pub enum BuiltinResult {
    /// Handled by a builtin; returns a value (or Value::NULL for void).
    Handled(Value),
    /// Not a builtin; should be executed as bytecode.
    NotHandled,
}

/// Default drop table for vanilla Minecraft crop blocks.
fn get_default_drops(block_id: &str) -> Vec<(String, i32)> {
    match block_id {
        "minecraft:wheat" => vec![
            ("minecraft:wheat".to_string(), 1),
            ("minecraft:wheat_seeds".to_string(), 2),
        ],
        "minecraft:carrots" => vec![("minecraft:carrot".to_string(), 2)],
        "minecraft:potatoes" => vec![("minecraft:potato".to_string(), 2)],
        "minecraft:beetroots" => vec![
            ("minecraft:beetroot".to_string(), 1),
            ("minecraft:beetroot_seeds".to_string(), 2),
        ],
        "minecraft:nether_wart" => vec![("minecraft:nether_wart".to_string(), 2)],
        "minecraft:cocoa" => vec![("minecraft:cocoa_beans".to_string(), 2)],
        _ => vec![(block_id.to_string(), 1)],
    }
}

/// The values a Java collection-like object holds, for `addAll`/`addAll`
/// style calls that cross List, Set, Array and Stream representations.
fn collection_items(heap: &Heap, val: &Value) -> Vec<Value> {
    let Some(id) = val.as_obj() else {
        return Vec::new();
    };
    match heap.get(id) {
        Some(JavaObject::List(items)) | Some(JavaObject::Set(items)) => items.clone(),
        Some(JavaObject::Stream(items)) => items.clone(),
        Some(JavaObject::Array { elements, .. }) => elements.clone(),
        _ => Vec::new(),
    }
}

/// Text carried by a string, component, or formatted text object. Components
/// built by the runtime keep their plain text in a `text` field; formatted
/// sequences copy it from the component they were made from.
/// Index of the entry of map `map_id` whose key is `key`: the same value, or a
/// `String` with equal text (`HashMap` compares keys with `equals`, and a string
/// built at runtime is a different heap object from an interned literal).
pub(crate) fn map_key_position(heap: &Heap, map_id: usize, key: &Value) -> Option<usize> {
    let Some(JavaObject::Map(entries)) = heap.get(map_id) else {
        return None;
    };
    if let Some(pos) = entries.iter().position(|(k, _)| k == key) {
        return Some(pos);
    }
    // `List.equals`: a list key matches a list with equal elements. Block state
    // tables key their states by the list of their property values.
    if let Some(JavaObject::List(wanted)) = key.as_obj().and_then(|id| heap.get(id)) {
        return entries.iter().position(|(k, _)| {
            matches!(
                k.as_obj().and_then(|id| heap.get(id)),
                Some(JavaObject::List(items)) if lists_equal(heap, items, wanted)
            )
        });
    }
    let text = key.as_obj().and_then(|id| heap.get_string(id))?;
    entries
        .iter()
        .position(|(k, _)| k.as_obj().and_then(|id| heap.get_string(id)) == Some(text))
}

/// `List.equals` over heap lists: equal length and equal elements, where an
/// element is equal by value, by identity, or as a list or string with equal content.
fn lists_equal(heap: &Heap, a: &[Value], b: &[Value]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            if x == y {
                return true;
            }
            let (Some(xi), Some(yi)) = (x.as_obj(), y.as_obj()) else {
                return false;
            };
            match (heap.get(xi), heap.get(yi)) {
                (Some(JavaObject::List(p)), Some(JavaObject::List(q))) => lists_equal(heap, p, q),
                (Some(JavaObject::String(p)), Some(JavaObject::String(q))) => p == q,
                // Boxed booleans are fresh objects per `Boolean.valueOf`.
                (
                    Some(JavaObject::Instance {
                        class_name: p_class,
                        fields: p,
                    }),
                    Some(JavaObject::Instance {
                        class_name: q_class,
                        fields: q,
                    }),
                ) => {
                    p_class == "java/lang/Boolean"
                        && q_class == p_class
                        && p.get("value") == q.get("value")
                }
                _ => false,
            }
        })
}

pub(crate) fn text_of_value(heap: &Heap, val: &Value) -> String {
    if let Some(id) = val.as_obj() {
        if let Some(s) = heap.get_string(id) {
            return s.to_string();
        }
        if let Some(JavaObject::Instance { class_name, fields }) = heap.get(id) {
            if class_name == "net/minecraft/util/FormattedCharSequence" {
                return fields
                    .get("text")
                    .map(|inner| text_of_value(heap, inner))
                    .unwrap_or_default();
            }
            if class_name == "java/lang/Boolean" {
                if let Some(inner) = fields.get("value") {
                    return if inner.as_bool() { "true" } else { "false" }.to_string();
                }
            }
            for key in ["text", "value", "string", "contents"] {
                if let Some(inner) = fields.get(key) {
                    if !inner.is_null() {
                        return text_of_value(heap, inner);
                    }
                }
            }
            if let Some(sibs_id) = fields.get("siblings").and_then(|v| v.as_obj()) {
                if let Some(JavaObject::List(parts)) = heap.get(sibs_id) {
                    return parts.iter().map(|p| text_of_value(heap, p)).collect();
                }
            }
        }
        if let Some(JavaObject::Lambda {
            interface_class,
            captured,
            ..
        }) = heap.get(id)
        {
            // `FormattedCharSequence` is a functional interface: anything built
            // by its factories is a lambda that captures the text it will hand
            // to a sink. The runtime never calls the sink (the host draws the
            // string), so read the captured text back instead of losing it.
            if interface_class == "net/minecraft/util/FormattedCharSequence" {
                return captured
                    .iter()
                    .map(|value| text_of_value(heap, value))
                    .collect();
            }
        }
    }
    value_to_string(heap, val)
}

/// A `FormattedCharSequence` carrying `text`, and optionally the line's ARGB
/// color so `extractText` keeps a tooltip line's color when the component
/// passes `-1` (the text style's own color).
fn alloc_formatted_char_sequence_colored(
    heap: &mut Heap,
    text: String,
    color: Option<i32>,
) -> Value {
    let sid = heap.alloc_string(text);
    let mut fields = HashMap::new();
    fields.insert("text".into(), Value::Object(Some(sid)));
    if let Some(color) = color {
        fields.insert("color".into(), Value::Int(color));
    }
    let id = heap.alloc(JavaObject::Instance {
        class_name: "net/minecraft/util/FormattedCharSequence".into(),
        fields,
    });
    Value::Object(Some(id))
}

/// The ARGB color a formatted sequence carries, when it has one.
fn formatted_sequence_color(heap: &Heap, value: &Value) -> Option<i32> {
    let id = value.as_obj()?;
    match heap.get(id)? {
        JavaObject::Instance { fields, .. } => fields.get("color").map(Value::as_int),
        JavaObject::List(items) | JavaObject::Set(items) | JavaObject::Stream(items) => items
            .iter()
            .find_map(|item| formatted_sequence_color(heap, item)),
        JavaObject::Array { elements, .. } => elements
            .iter()
            .find_map(|item| formatted_sequence_color(heap, item)),
        _ => None,
    }
}

/// The text a formatted sequence or a composite of them displays. `composite`
/// and `fromList` take a list or an array of sequences, which `text_of_value`
/// alone would stringify as an opaque object.
fn formatted_text_of_value(heap: &Heap, val: &Value) -> String {
    if let Some(id) = val.as_obj() {
        match heap.get(id) {
            Some(JavaObject::List(items))
            | Some(JavaObject::Set(items))
            | Some(JavaObject::Stream(items)) => {
                return items.iter().map(|item| text_of_value(heap, item)).collect();
            }
            Some(JavaObject::Array { elements, .. }) => {
                return elements
                    .iter()
                    .map(|item| text_of_value(heap, item))
                    .collect();
            }
            _ => {}
        }
    }
    text_of_value(heap, val)
}

/// Font width in GUI pixels. The runtime has no glyph atlas, so text layout
/// uses the vanilla-like 6 pixel advance (4 for a space). The host draws the
/// same strings with its own font, so this only affects the layout arithmetic
/// mods do themselves.
fn text_width(text: &str) -> i32 {
    text.chars()
        .map(|c| if c == ' ' { 4 } else { 6 })
        .sum::<i32>()
}

/// The resource id of a `Identifier`/`ResourceLocation` object.
fn identifier_of(heap: &Heap, val: &Value) -> String {
    if let Some(id) = val.as_obj() {
        if let Some(s) = heap.get_string(id) {
            return s.to_string();
        }
        if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
            let namespace = fields.get("namespace").map(|v| text_of_value(heap, v));
            let path = fields.get("path").map(|v| text_of_value(heap, v));
            if let (Some(namespace), Some(path)) = (namespace, path) {
                if !path.is_empty() {
                    return if namespace.is_empty() {
                        path
                    } else {
                        format!("{namespace}:{path}")
                    };
                }
            }
            for key in ["id", "value"] {
                if let Some(v) = fields.get(key) {
                    let s = text_of_value(heap, v);
                    if !s.is_empty() {
                        return s;
                    }
                }
            }
        }
    }
    String::new()
}

/// The first `Identifier` argument, skipping a render pipeline or sprite.
fn identifier_argument(heap: &Heap, args: &[Value], from: usize) -> String {
    for arg in args.iter().skip(from) {
        let Some(id) = arg.as_obj() else {
            continue;
        };
        let Some(JavaObject::Instance { class_name, .. }) = heap.get(id) else {
            continue;
        };
        if class_name.ends_with("Identifier") || class_name.ends_with("ResourceLocation") {
            let value = identifier_of(heap, arg);
            if !value.is_empty() {
                return value;
            }
        }
    }
    String::new()
}

/// The Y rotation, in degrees, encoded by a quaternion (`0, sin(θ/2), 0,
/// cos(θ/2)`). A quaternion the runtime did not build has no fields and reads
/// as no rotation.
fn quaternion_y_degrees(heap: &Heap, value: &Value) -> f32 {
    if let Some(JavaObject::Instance { fields, .. }) = value.as_obj().and_then(|id| heap.get(id)) {
        let y = fields.get("y").map(Value::as_float);
        let w = fields.get("w").map(Value::as_float);
        if let (Some(y), Some(w)) = (y, w) {
            return 2.0 * y.atan2(w).to_degrees();
        }
    }
    0.0
}

/// The size in a PNG's IHDR chunk. `NativeImage.read` only needs the header;
/// the host decodes the pixels when it draws the registered texture.
pub(crate) fn png_size(data: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if data.len() >= 24 && &data[..8] == SIGNATURE {
        let width = u32::from_be_bytes(data[16..20].try_into().ok()?);
        let height = u32::from_be_bytes(data[20..24].try_into().ok()?);
        if width > 0 && height > 0 {
            return Some((width, height));
        }
    }
    None
}

/// Read one entry out of a zip archive.
fn read_zip_entry<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut resource = archive.by_name(name).ok()?;
    let mut bytes = Vec::new();
    resource.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// The `InputStream` methods of a [`ByteStream`] (the bytes behind
/// `Files.newInputStream` and similar). Without them a JDK reader loops on the
/// abstract `read()`, which never reports the end of the stream.
/// `Atomic{Reference,Integer,Long}FieldUpdater` over an instance field. The
/// real classes go through `Unsafe`, which the runtime does not model; the
/// interpreter is single-threaded, so a plain read-modify-write is atomic.
fn field_updater_method(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    let kind = class_name.strip_prefix("java/util/concurrent/atomic/")?;
    if !matches!(
        kind,
        "AtomicReferenceFieldUpdater" | "AtomicIntegerFieldUpdater" | "AtomicLongFieldUpdater"
    ) {
        return None;
    }
    if method_name == "newUpdater" {
        let name = args.last()?.as_obj()?;
        let name = heap.get_string(name)?.to_string();
        let name_val = Value::Object(Some(heap.alloc_string(name)));
        let mut fields = HashMap::new();
        fields.insert("name".to_string(), name_val);
        let id = heap.alloc(JavaObject::Instance {
            class_name: class_name.into(),
            fields,
        });
        return Some(BuiltinResult::Handled(Value::Object(Some(id))));
    }
    let updater = args.first()?.as_obj()?;
    let field = match heap.get(updater)? {
        JavaObject::Instance { fields, .. } => fields.get("name")?.as_obj()?,
        _ => return None,
    };
    let field = heap.get_string(field)?.to_string();
    let target = args.get(1)?.as_obj()?;
    let is_long = kind == "AtomicLongFieldUpdater";
    let is_ref = kind == "AtomicReferenceFieldUpdater";
    let zero = if is_ref {
        Value::NULL
    } else if is_long {
        Value::Long(0)
    } else {
        Value::Int(0)
    };
    let current = match heap.get(target)? {
        JavaObject::Instance { fields, .. } => fields.get(&field).cloned().unwrap_or(zero.clone()),
        _ => return None,
    };
    let same = |a: &Value, b: &Value| match (a, b) {
        (Value::Object(x), Value::Object(y)) => x == y,
        (Value::Long(x), Value::Long(y)) => x == y,
        _ => a.as_int() == b.as_int(),
    };
    let add = |v: &Value, d: i64| {
        if is_long {
            Value::Long(v.as_long().wrapping_add(d))
        } else {
            Value::Int(v.as_int().wrapping_add(d as i32))
        }
    };
    let (store, result): (Option<Value>, Value) = match method_name {
        "get" => (None, current),
        "set" | "lazySet" => (Some(args.get(2)?.clone()), Value::NULL),
        "getAndSet" => (Some(args.get(2)?.clone()), current),
        "compareAndSet"
        | "weakCompareAndSet"
        | "weakCompareAndSetPlain"
        | "weakCompareAndSetVolatile" => {
            let ok = same(&current, args.get(2)?);
            (ok.then(|| args[3].clone()), Value::from_bool(ok))
        }
        "incrementAndGet" => {
            let n = add(&current, 1);
            (Some(n.clone()), n)
        }
        "decrementAndGet" => {
            let n = add(&current, -1);
            (Some(n.clone()), n)
        }
        "getAndIncrement" => (Some(add(&current, 1)), current),
        "getAndDecrement" => (Some(add(&current, -1)), current),
        "addAndGet" => {
            let n = add(&current, args.get(2)?.as_long());
            (Some(n.clone()), n)
        }
        "getAndAdd" => (Some(add(&current, args.get(2)?.as_long())), current),
        _ => return None,
    };
    if let Some(value) = store {
        if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(target) {
            fields.insert(field, value);
        }
    }
    Some(BuiltinResult::Handled(result))
}

fn byte_stream_method(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    if !class_name.starts_with("java/io/") {
        return None;
    }
    let id = args.first()?.as_obj()?;
    heap.get_native::<ByteStream>(id)?;
    let remaining = |heap: &Heap| {
        heap.get_native::<ByteStream>(id)
            .map(|s| s.data.len().saturating_sub(s.pos))
            .unwrap_or(0)
    };
    let value = match method_name {
        "read" if args.len() == 1 => {
            let stream = heap.get_native_mut::<ByteStream>(id)?;
            match stream.data.get(stream.pos).copied() {
                Some(byte) => {
                    stream.pos += 1;
                    Value::Int(byte as i32)
                }
                None => Value::Int(-1),
            }
        }
        "read" | "readNBytes" if args.len() >= 2 => {
            let target = args[1].as_obj()?;
            let (offset, wanted) = if args.len() >= 4 {
                (
                    args[2].as_int().max(0) as usize,
                    args[3].as_int().max(0) as usize,
                )
            } else {
                let len = match heap.get(target) {
                    Some(JavaObject::Array { elements, .. }) => elements.len(),
                    _ => 0,
                };
                (0, len)
            };
            if wanted == 0 {
                return Some(BuiltinResult::Handled(Value::Int(0)));
            }
            if remaining(heap) == 0 {
                return Some(BuiltinResult::Handled(Value::Int(-1)));
            }
            let chunk: Vec<u8> = {
                let stream = heap.get_native_mut::<ByteStream>(id)?;
                let n = wanted.min(stream.data.len() - stream.pos);
                let chunk = stream.data[stream.pos..stream.pos + n].to_vec();
                stream.pos += n;
                chunk
            };
            if let Some(JavaObject::Array { elements, .. }) = heap.get_mut(target) {
                for (i, byte) in chunk.iter().enumerate() {
                    if let Some(slot) = elements.get_mut(offset + i) {
                        *slot = Value::Int(*byte as i8 as i32);
                    }
                }
            }
            Value::Int(chunk.len() as i32)
        }
        "readAllBytes" => {
            let rest: Vec<u8> = {
                let stream = heap.get_native_mut::<ByteStream>(id)?;
                let rest = stream.data[stream.pos.min(stream.data.len())..].to_vec();
                stream.pos = stream.data.len();
                rest
            };
            let elements = rest.iter().map(|b| Value::Int(*b as i8 as i32)).collect();
            let array = heap.alloc(JavaObject::Array {
                elem_type: "byte".into(),
                elements,
            });
            Value::Object(Some(array))
        }
        "available" => Value::Int(remaining(heap) as i32),
        "skip" if args.len() >= 2 => {
            let wanted = match &args[1] {
                Value::Long(n) => (*n).max(0) as usize,
                other => other.as_int().max(0) as usize,
            };
            let stream = heap.get_native_mut::<ByteStream>(id)?;
            let n = wanted.min(stream.data.len() - stream.pos);
            stream.pos += n;
            Value::Long(n as i64)
        }
        "markSupported" => Value::from_bool(false),
        _ => return None,
    };
    Some(BuiltinResult::Handled(value))
}

/// Where `Unsafe.objectFieldOffset` offsets start: above any array offset the
/// runtime answers (`arrayBaseOffset` 16 + index × 8), so the two never meet.
const UNSAFE_FIELD_BASE: i64 = 1 << 40;

/// A memory slot `Unsafe` addresses by `(object, offset)`: a named field of an
/// interpreter object, or an element of an array.
enum UnsafeSlot {
    Field(usize, String),
    Element(usize, usize),
}

fn unsafe_slot(heap: &Heap, object: &Value, offset: &Value) -> Option<UnsafeSlot> {
    let id = object.as_obj()?;
    let offset = offset.as_long();
    match heap.get(id)? {
        JavaObject::Instance { .. } if offset >= UNSAFE_FIELD_BASE => {
            let name = heap
                .unsafe_fields
                .get((offset - UNSAFE_FIELD_BASE) as usize)?;
            Some(UnsafeSlot::Field(id, name.clone()))
        }
        JavaObject::Array { elements, .. } if (16..UNSAFE_FIELD_BASE).contains(&offset) => {
            let index = ((offset - 16) / 8) as usize;
            (index < elements.len()).then_some(UnsafeSlot::Element(id, index))
        }
        _ => None,
    }
}

fn read_slot(heap: &Heap, slot: &UnsafeSlot) -> Value {
    match slot {
        UnsafeSlot::Field(id, name) => match heap.get(*id) {
            Some(JavaObject::Instance { fields, .. }) => {
                fields.get(name).cloned().unwrap_or(Value::NULL)
            }
            _ => Value::NULL,
        },
        UnsafeSlot::Element(id, index) => match heap.get(*id) {
            Some(JavaObject::Array { elements, .. }) => {
                elements.get(*index).cloned().unwrap_or(Value::NULL)
            }
            _ => Value::NULL,
        },
    }
}

fn write_slot(heap: &mut Heap, slot: &UnsafeSlot, value: Value) {
    match slot {
        UnsafeSlot::Field(id, name) => {
            if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(*id) {
                fields.insert(name.clone(), value);
            }
        }
        UnsafeSlot::Element(id, index) => {
            if let Some(JavaObject::Array { elements, .. }) = heap.get_mut(*id) {
                if let Some(element) = elements.get_mut(*index) {
                    *element = value;
                }
            }
        }
    }
}

/// `VarHandle` accessors on an array element: `(handle, array, index, ...)`.
fn var_handle_array(heap: &mut Heap, method_name: &str, args: &[Value]) -> Option<BuiltinResult> {
    let handled = |v: Value| Some(BuiltinResult::Handled(v));
    let array = args.get(1)?.as_obj()?;
    let index = usize::try_from(args.get(2)?.as_int()).ok()?;
    let length = match heap.get(array)? {
        JavaObject::Array { elements, .. } => elements.len(),
        _ => return None,
    };
    let read = |heap: &Heap| match heap.get(array) {
        Some(JavaObject::Array { elements, .. }) => {
            elements.get(index).cloned().unwrap_or(Value::NULL)
        }
        _ => Value::NULL,
    };
    let write = |heap: &mut Heap, value: Value| {
        if let Some(JavaObject::Array { elements, .. }) = heap.get_mut(array) {
            if let Some(slot) = elements.get_mut(index) {
                *slot = value;
            }
        }
    };
    if index >= length {
        // A JVM throws `ArrayIndexOutOfBoundsException` here; nothing in the
        // runtime's own model writes out of bounds.
        return None;
    }
    if matches!(
        method_name,
        "get" | "getVolatile" | "getAcquire" | "getOpaque"
    ) {
        if args.len() == 3 {
            return handled(read(heap));
        }
    }
    if matches!(
        method_name,
        "set" | "setVolatile" | "setRelease" | "setOpaque" | "setPlain"
    ) && args.len() == 4
    {
        write(heap, args[3].clone());
        return handled(Value::NULL);
    }
    if matches!(
        method_name,
        "getAndSet" | "getAndSetRelease" | "getAndSetAcquire"
    ) && args.len() == 4
    {
        let old = read(heap);
        write(heap, args[3].clone());
        return handled(old);
    }
    if matches!(
        method_name,
        "compareAndSet"
            | "weakCompareAndSet"
            | "weakCompareAndSetPlain"
            | "weakCompareAndSetAcquire"
            | "weakCompareAndSetRelease"
            | "compareAndExchange"
    ) && args.len() >= 5
    {
        let old = read(heap);
        let expected = &args[3];
        let same = match (&old, expected) {
            (Value::Object(x), Value::Object(y)) => x == y,
            _ => old.as_long() == expected.as_long() && old.as_double() == expected.as_double(),
        };
        if same {
            write(heap, args[4].clone());
        }
        return handled(if method_name == "compareAndExchange" {
            old
        } else {
            Value::from_bool(same)
        });
    }
    None
}

/// The zero of an `Unsafe` accessor's type, for a field nothing wrote yet./// The zero of an `Unsafe` accessor's type, for a field nothing wrote yet.
fn unsafe_zero(kind: &str) -> Value {
    match kind {
        "Int" | "Short" | "Byte" | "Char" | "Boolean" => Value::Int(0),
        "Long" => Value::Long(0),
        "Float" => Value::Float(0.0),
        "Double" => Value::Double(0.0),
        _ => Value::NULL,
    }
}

/// `Unsafe` reads, writes, compare-and-sets and get-and-adds on a slot the
/// runtime can name. One thread runs, so every access is trivially atomic.
/// `None` when the call is not one of these or its slot is not addressable.
fn unsafe_memory(heap: &mut Heap, method_name: &str, args: &[Value]) -> Option<BuiltinResult> {
    let handled = |v: Value| Some(BuiltinResult::Handled(v));
    let kinds = [
        "Reference",
        "Object",
        "Int",
        "Long",
        "Boolean",
        "Byte",
        "Short",
        "Char",
        "Float",
        "Double",
    ];
    let kind_of = |rest: &str| {
        kinds
            .iter()
            .find(|kind| rest.starts_with(*kind))
            .map(|kind| kind.to_string())
    };
    let slot = unsafe_slot(heap, args.get(1)?, args.get(2)?)?;
    let current = |heap: &Heap, kind: &str| {
        let value = read_slot(heap, &slot);
        if value.is_null() && !matches!(kind, "Reference" | "Object") {
            unsafe_zero(kind)
        } else {
            value
        }
    };
    let same = |a: &Value, b: &Value| match (a, b) {
        (Value::Object(x), Value::Object(y)) => x == y,
        _ => a.as_long() == b.as_long() && a.as_double() == b.as_double(),
    };
    for prefix in ["compareAndSet", "compareAndSwap", "weakCompareAndSet"] {
        if let Some(rest) = method_name.strip_prefix(prefix) {
            let kind = kind_of(rest)?;
            let (expected, new) = (args.get(3)?, args.get(4)?);
            let ok = same(&current(heap, &kind), expected);
            if ok {
                write_slot(heap, &slot, new.clone());
            }
            return handled(Value::from_bool(ok));
        }
    }
    if let Some(rest) = method_name.strip_prefix("compareAndExchange") {
        let kind = kind_of(rest)?;
        let (expected, new) = (args.get(3)?, args.get(4)?);
        let old = current(heap, &kind);
        if same(&old, expected) {
            write_slot(heap, &slot, new.clone());
        }
        return handled(old);
    }
    if let Some(rest) = method_name.strip_prefix("getAndAdd") {
        let kind = kind_of(rest)?;
        let old = current(heap, &kind);
        let delta = args.get(3)?;
        let new = match kind.as_str() {
            "Long" => Value::Long(old.as_long().wrapping_add(delta.as_long())),
            _ => Value::Int(old.as_int().wrapping_add(delta.as_int())),
        };
        write_slot(heap, &slot, new);
        return handled(old);
    }
    if let Some(rest) = method_name.strip_prefix("getAndSet") {
        let kind = kind_of(rest)?;
        let old = current(heap, &kind);
        write_slot(heap, &slot, args.get(3)?.clone());
        return handled(old);
    }
    if let Some(rest) = method_name.strip_prefix("get") {
        if args.len() == 3 {
            let kind = kind_of(rest)?;
            return handled(current(heap, &kind));
        }
    }
    if let Some(rest) = method_name.strip_prefix("put") {
        if args.len() == 4 {
            kind_of(rest)?;
            write_slot(heap, &slot, args[3].clone());
            return handled(Value::NULL);
        }
    }
    None
}

/// A `CustomValue` over one JSON value of a `fabric.mod.json` `custom` entry:
/// an instance keeping the value's JSON text.
fn custom_value(heap: &mut Heap, json: &serde_json::Value) -> Value {
    let text = heap.alloc_string(json.to_string());
    let mut fields = HashMap::new();
    fields.insert("json".to_string(), Value::Object(Some(text)));
    let id = heap.alloc(JavaObject::Instance {
        class_name: "net/fabricmc/loader/api/metadata/CustomValue".into(),
        fields,
    });
    Value::Object(Some(id))
}

/// `CustomValue`, `CvObject` and `CvArray` accessors, answered from the JSON the
/// value was wrapped from. Mods (Mod Menu among them) read their own and other
/// mods' `custom` data through these.
fn custom_value_method(
    heap: &mut Heap,
    method_name: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    let this = args.first()?.as_obj()?;
    let json: serde_json::Value = match heap.get(this)? {
        JavaObject::Instance { fields, .. } => {
            let text = fields.get("json")?.as_obj()?;
            serde_json::from_str(heap.get_string(text)?).ok()?
        }
        _ => return None,
    };
    let string_arg = |heap: &Heap, index: usize| {
        args.get(index)
            .and_then(|value| value.as_obj())
            .and_then(|id| heap.get_string(id))
            .map(str::to_string)
    };
    let value = match method_name {
        // The `CvType` constant's name; see `CustomValue$CvType` statics.
        "getType" => {
            let kind = match &json {
                serde_json::Value::Object(_) => "OBJECT",
                serde_json::Value::Array(_) => "ARRAY",
                serde_json::Value::String(_) => "STRING",
                serde_json::Value::Number(_) => "NUMBER",
                serde_json::Value::Bool(_) => "BOOLEAN",
                serde_json::Value::Null => "NULL",
            };
            Value::Object(Some(heap.alloc_string(kind)))
        }
        "getAsNumber" => match &json {
            serde_json::Value::Number(n) if n.is_i64() => match n.as_i64().map(i32::try_from) {
                Some(Ok(small)) => Value::Int(small),
                _ => Value::Long(n.as_i64().unwrap_or(0)),
            },
            serde_json::Value::Number(n) => Value::Double(n.as_f64().unwrap_or(0.0)),
            _ => return None,
        },
        "getAsString" => {
            let text = match &json {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            Value::Object(Some(heap.alloc_string(text)))
        }
        "getAsBoolean" => Value::from_bool(json.as_bool().unwrap_or(false)),
        "getAsObject" | "getAsArray" => args[0].clone(),
        "containsKey" => {
            let key = string_arg(heap, 1)?;
            Value::from_bool(json.get(&key).is_some())
        }
        "get" => match &json {
            serde_json::Value::Object(map) => {
                let key = string_arg(heap, 1)?;
                match map.get(&key) {
                    Some(inner) => custom_value(heap, inner),
                    None => Value::NULL,
                }
            }
            serde_json::Value::Array(items) => {
                match items.get(args.get(1)?.as_int().max(0) as usize) {
                    Some(inner) => custom_value(heap, inner),
                    None => Value::NULL,
                }
            }
            _ => return None,
        },
        "size" => Value::Int(match &json {
            serde_json::Value::Object(map) => map.len(),
            serde_json::Value::Array(items) => items.len(),
            _ => 0,
        } as i32),
        "isEmpty" => Value::from_bool(match &json {
            serde_json::Value::Object(map) => map.is_empty(),
            serde_json::Value::Array(items) => items.is_empty(),
            _ => true,
        }),
        "iterator" => {
            let mut items = Vec::new();
            match &json {
                serde_json::Value::Object(map) => {
                    for (key, inner) in map {
                        let key = Value::Object(Some(heap.alloc_string(key.clone())));
                        let inner = custom_value(heap, inner);
                        let mut fields = HashMap::new();
                        fields.insert("key".to_string(), key);
                        fields.insert("value".to_string(), inner);
                        let entry = heap.alloc(JavaObject::Instance {
                            class_name: "java/util/Map$Entry".into(),
                            fields,
                        });
                        items.push(Value::Object(Some(entry)));
                    }
                }
                serde_json::Value::Array(values) => {
                    for inner in values {
                        items.push(custom_value(heap, inner));
                    }
                }
                _ => {}
            }
            Value::Object(Some(heap.alloc_iterator(items)))
        }
        _ => return None,
    };
    Some(BuiltinResult::Handled(value))
}

/// A `net.fabricmc.loader.api.ModContainer` for a loaded mod (or the built-in
/// `minecraft` / `fabricloader`), carrying the metadata the mod declared.
fn fabric_mod_container(heap: &mut Heap, mod_id: &str) -> usize {
    let (mname, mver, mdesc) = if let Some(meta) = heap.loaded_mods.get(mod_id) {
        (
            meta.name.clone(),
            meta.version.clone(),
            meta.description.clone(),
        )
    } else if mod_id == "minecraft" {
        (
            "Minecraft".into(),
            "26.3".into(),
            "Minecraft: Java Edition".into(),
        )
    } else if mod_id == "fabricloader" {
        (
            "Fabric Loader".into(),
            "0.16.10".into(),
            "Fabric Mod Loader".into(),
        )
    } else {
        (mod_id.to_string(), "1.0.0".into(), String::new())
    };
    let mut meta_fields = HashMap::new();
    let mid_id = heap.alloc_string(mod_id);
    let mname_id = heap.alloc_string(&mname);
    let mdesc_id = heap.alloc_string(&mdesc);
    meta_fields.insert("id".into(), Value::Object(Some(mid_id)));
    meta_fields.insert("name".into(), Value::Object(Some(mname_id)));
    meta_fields.insert("description".into(), Value::Object(Some(mdesc_id)));

    let mut ver_fields = HashMap::new();
    let ver_str_id = heap.alloc_string(&mver);
    ver_fields.insert("friendlyString".into(), Value::Object(Some(ver_str_id)));
    let ver_id = heap.alloc(JavaObject::Instance {
        class_name: "net/fabricmc/loader/api/Version".into(),
        fields: ver_fields,
    });
    meta_fields.insert("version".into(), Value::Object(Some(ver_id)));

    let meta_id = heap.alloc(JavaObject::Instance {
        class_name: "net/fabricmc/loader/api/metadata/ModMetadata".into(),
        fields: meta_fields,
    });
    let mut container_fields = HashMap::new();
    container_fields.insert("metadata".into(), Value::Object(Some(meta_id)));
    heap.alloc(JavaObject::Instance {
        class_name: "net/fabricmc/loader/api/ModContainer".into(),
        fields: container_fields,
    })
}

/// The mod whose jar carries `mod_id` as a jar-in-jar, which Fabric reports as
/// the nested mod's containing mod.
fn containing_mod_id(heap: &Heap, mod_id: &str) -> Option<String> {
    let meta = heap.loaded_mods.get(mod_id)?;
    if !meta.is_nested {
        return None;
    }
    heap.loaded_mods
        .iter()
        .find(|(_, other)| !other.is_nested && other.jar == meta.jar)
        .map(|(id, _)| id.clone())
}

/// Read one resource out of a mod jar (or a nested jar inside it). This is
/// what `Files.newInputStream` needs for `ModContainer.getPath`: the mod's own
/// `NativeImage.read` then runs on the real bytes.
pub(crate) fn read_mod_resource(jar: &str, nested: Option<&str>, entry: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(jar).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    match nested {
        None => read_zip_entry(&mut archive, entry),
        Some(nested) => {
            let nested_bytes = read_zip_entry(&mut archive, nested)?;
            let mut inner = zip::ZipArchive::new(std::io::Cursor::new(nested_bytes)).ok()?;
            read_zip_entry(&mut inner, entry)
        }
    }
}

/// The raw `fabric.mod.json` behind a `ModMetadata` object.
fn metadata_raw<'a>(heap: &'a Heap, value: &Value) -> Option<&'a str> {
    let id = value.as_obj()?;
    let JavaObject::Instance { fields, .. } = heap.get(id)? else {
        return None;
    };
    let mod_id = fields.get("id")?.as_obj()?;
    let mod_id = heap.get_string(mod_id)?;
    heap.loaded_mods
        .get(mod_id)
        .map(|metadata| metadata.raw.as_str())
}

pub(crate) fn value_to_string(heap: &Heap, val: &Value) -> String {
    match val {
        Value::Int(i) => i.to_string(),
        Value::Long(l) => l.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Double(d) => d.to_string(),
        Value::Object(Some(id)) => {
            if let Some(s) = heap.get_string(*id) {
                s.to_string()
            } else if let Some(JavaObject::Instance { class_name, fields }) = heap.get(*id) {
                if class_name == "net/minecraft/util/FormattedCharSequence" {
                    return fields
                        .get("text")
                        .map(|inner| text_of_value(heap, inner))
                        .unwrap_or_default();
                }
                for key in [
                    "text",
                    "contents",
                    "string",
                    "value",
                    "friendlyString",
                    "name",
                ] {
                    if let Some(inner) = fields.get(key) {
                        if !inner.is_null() {
                            let text = text_of_value(heap, inner);
                            if !text.is_empty() && !text.starts_with("[Object ") {
                                return text;
                            }
                        }
                    }
                }
                for key in ["parts", "siblings", "list", "items"] {
                    if let Some(parts) = fields.get(key).and_then(|v| v.as_obj()) {
                        if let Some(JavaObject::List(parts)) = heap.get(parts) {
                            let joined: String =
                                parts.iter().map(|p| text_of_value(heap, p)).collect();
                            if !joined.is_empty() {
                                return joined;
                            }
                        }
                    }
                }
                format!("[Object {id}]")
            } else {
                format!("[Object {id}]")
            }
        }
        Value::Object(None) => "null".to_string(),
    }
}

fn format_translation(template: &str, args: &[String]) -> String {
    if args.is_empty() {
        return template.to_string();
    }
    let mut res = String::new();
    let mut chars = template.chars().peekable();
    let mut arg_idx = 0;
    while let Some(c) = chars.next() {
        if c == '%' {
            if let Some(&next) = chars.peek() {
                if next == '%' {
                    chars.next();
                    res.push('%');
                    continue;
                }
                if next.is_ascii_digit() {
                    let mut num_str = String::new();
                    while let Some(&d) = chars.peek() {
                        if d.is_ascii_digit() {
                            num_str.push(d);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if chars.peek() == Some(&'$') {
                        chars.next();
                        let spec = chars.next().unwrap_or('s');
                        let _ = spec;
                        if let Ok(idx) = num_str.parse::<usize>() {
                            if idx > 0 && idx <= args.len() {
                                res.push_str(&args[idx - 1]);
                                continue;
                            }
                        }
                    } else {
                        res.push('%');
                        res.push_str(&num_str);
                        continue;
                    }
                } else if next == 's' || next == 'd' || next == 'f' {
                    chars.next();
                    if arg_idx < args.len() {
                        res.push_str(&args[arg_idx]);
                        arg_idx += 1;
                    } else {
                        res.push('%');
                        res.push(next);
                    }
                    continue;
                }
            }
        }
        res.push(c);
    }
    res
}

/// Try to dispatch a method call to built-in handlers.
/// The registry kind ("BLOCK", "ITEM") a `Registry` object or `ResourceKey` stands for.
pub(crate) fn registry_kind(heap: &Heap, value: &Value) -> Option<String> {
    let id = value.as_obj()?;
    match heap.get(id)? {
        JavaObject::String(s) => Some(s.clone()),
        JavaObject::Instance { fields, .. } => {
            if let Some(kind) = fields
                .get("kind")
                .and_then(|k| k.as_obj())
                .and_then(|k| heap.get_string(k))
            {
                return Some(kind.to_string());
            }
            // A real `MappedRegistry`: its `key` is `minecraft:<registry path>`.
            // The synthetic kinds are the upper-cased path (`BLOCK` for
            // `minecraft:block`), so both forms answer the same token.
            let key = fields.get("key")?.as_obj()?;
            let JavaObject::Instance { fields: key, .. } = heap.get(key)? else {
                return None;
            };
            let location = key.get("location")?;
            let text = identifier_text(heap, location)?;
            let (_, path) = text.split_once(':')?;
            Some(path.to_ascii_uppercase())
        }
        _ => None,
    }
}

/// A `ResourceKey` for `kind` (the registry) and `location` (an Identifier).
fn resource_key(heap: &mut Heap, kind: &str, location: usize) -> Value {
    let kind = heap.alloc_string(kind);
    let mut fields = HashMap::new();
    fields.insert("kind".to_string(), Value::Object(Some(kind)));
    fields.insert("location".to_string(), Value::Object(Some(location)));
    Value::Object(Some(heap.alloc(JavaObject::Instance {
        class_name: "net/minecraft/resources/ResourceKey".into(),
        fields,
    })))
}

fn resource_key_text(heap: &Heap, key: &Value) -> Option<String> {
    let kind = registry_kind(heap, key)?;
    let location = match heap.get(key.as_obj()?)? {
        JavaObject::Instance { fields, .. } => fields.get("location")?.clone(),
        _ => return None,
    };
    Some(format!("{kind}/{}", identifier_text(heap, &location)?))
}

pub(crate) fn identifier_text(heap: &Heap, value: &Value) -> Option<String> {
    let id = value.as_obj()?;
    match heap.get(id)? {
        JavaObject::Instance { fields, .. } => {
            let part = |name: &str| {
                fields
                    .get(name)
                    .and_then(|v| v.as_obj())
                    .and_then(|v| heap.get_string(v))
            };
            Some(format!("{}:{}", part("namespace")?, part("path")?))
        }
        _ => None,
    }
}

/// `MinecraftServer.registryAccess()`, `Registry.containsKey/getTagOrEmpty`
/// and `TagKey.create`, answered from the host-loaded registry snapshot.
fn registry_method(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    let handled = |v: Value| Some(BuiltinResult::Handled(v));
    if let Some(result) = builtin_registry_method(heap, class_name, method_name, args) {
        return Some(result);
    }
    match (class_name, method_name) {
        (
            "net/minecraft/server/MinecraftServer"
            | "net/minecraft/world/level/Level"
            | "net/minecraft/client/multiplayer/ClientLevel"
            | "net/minecraft/server/level/ServerLevel",
            "registryAccess",
        ) => {
            // The client level's registry access is the host's registry
            // snapshot, the same one `lookupOrThrow`/`Registry` answer from.
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/core/RegistryAccess$Frozen".into(),
                fields: HashMap::new(),
            });
            handled(Value::Object(Some(id)))
        }
        (
            "net/minecraft/core/RegistryAccess$Frozen"
            | "net/minecraft/core/RegistryAccess"
            | "net/minecraft/core/HolderLookup$Provider",
            "lookupOrThrow" | "registryOrThrow",
        ) => {
            let kind = registry_kind(heap, args.get(1)?)?;
            let mut fields = HashMap::new();
            fields.insert(
                "kind".to_string(),
                Value::Object(Some(heap.alloc_string(kind))),
            );
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/core/DefaultedRegistry".into(),
                fields,
            });
            handled(Value::Object(Some(id)))
        }
        ("net/minecraft/tags/TagKey", "create") if args.len() >= 2 => {
            let registry = registry_kind(heap, &args[0])?;
            let id = identifier_text(heap, &args[1])?;
            let tag = heap.alloc_native(TagKey { registry, id });
            handled(Value::Object(Some(tag)))
        }
        (
            "net/minecraft/core/DefaultedRegistry"
            | "net/minecraft/core/Registry"
            | "net/minecraft/core/MappedRegistry"
            | "net/minecraft/core/DefaultedMappedRegistry",
            "containsKey" | "getTagOrEmpty" | "get" | "getValue" | "getOptional",
        ) => {
            let kind = registry_kind(heap, args.first()?)?;
            match method_name {
                "containsKey" => {
                    let id = identifier_text(heap, args.get(1)?)?;
                    handled(Value::from_bool(heap.registries.contains(&kind, &id)))
                }
                "getTagOrEmpty" => {
                    let tag = args
                        .get(1)
                        .and_then(|v| v.as_obj())
                        .and_then(|id| heap.get_native::<TagKey>(id))
                        .map(|t| t.id.clone())?;
                    let tag = if tag.contains(':') {
                        tag
                    } else {
                        format!("minecraft:{tag}")
                    };
                    let members = heap.registries.tag_members(&kind, &tag);
                    let holders: Vec<Value> = members
                        .into_iter()
                        .map(|member| {
                            let object = if kind == "BLOCK" {
                                heap.alloc_native(BlockState::new(&member).block)
                            } else {
                                heap.alloc_native(Item { id: member })
                            };
                            let mut fields = HashMap::new();
                            fields.insert("value".to_string(), Value::Object(Some(object)));
                            Value::Object(Some(heap.alloc(JavaObject::Instance {
                                class_name: "net/minecraft/core/Holder$Reference".into(),
                                fields,
                            })))
                        })
                        .collect();
                    let list = heap.alloc_list(holders);
                    handled(Value::Object(Some(list)))
                }
                _ => None,
            }
        }
        ("net/minecraft/core/Holder" | "net/minecraft/core/Holder$Reference", "value") => {
            let id = args.first()?.as_obj()?;
            match heap.get(id)? {
                JavaObject::Instance { fields, .. } => handled(fields.get("value")?.clone()),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The registry token of one of the runtime's built-in registries
/// (`BuiltInRegistries.SOUND_EVENT` is an object carrying `kind`), not of a
/// real `MappedRegistry` instance, whose own bytecode answers.
fn builtin_registry_kind(heap: &Heap, value: &Value) -> Option<String> {
    let JavaObject::Instance { class_name, fields } = heap.get(value.as_obj()?)? else {
        return None;
    };
    if class_name != "net/minecraft/core/DefaultedRegistry" {
        return None;
    }
    let kind = fields.get("kind")?.as_obj()?;
    heap.get_string(kind).map(str::to_string)
}

/// A `Holder.Reference` for `value`, with `key` once it is registered. Its
/// fields are `Holder.Reference`'s own, so its bytecode (`unwrapKey`,
/// `isBound`, `is(ResourceKey)`) reads them.
fn registry_holder(heap: &mut Heap, key: Option<Value>, value: Value) -> usize {
    let mut fields = HashMap::new();
    fields.insert("value".to_string(), value);
    if let Some(key) = key {
        fields.insert("key".to_string(), key);
    }
    heap.alloc(JavaObject::Instance {
        class_name: "net/minecraft/core/Holder$Reference".into(),
        fields,
    })
}

/// Every value a built-in registry holds, in registration order.
fn registry_values(heap: &mut Heap, kind: &str) -> Vec<Value> {
    // What code registered: the general table, plus the item and block paths,
    // which record their own objects (a block's facts hang off them).
    let mut values: Vec<Value> = heap
        .registry_entries
        .iter()
        .filter(|entry| entry.kind == kind)
        .map(|entry| entry.value.clone())
        .collect();
    match kind {
        "BLOCK" => values.extend(
            heap.registered_blocks
                .iter()
                .map(|record| Value::Object(Some(record.object))),
        ),
        "ITEM" => values.extend(
            heap.registered_items
                .iter()
                .map(|record| Value::Object(Some(record.object))),
        ),
        _ => {}
    }
    // The host's snapshot names the vanilla entries; their objects are the
    // runtime's stand-ins, in the order the snapshot lists them.
    let snapshot: Vec<String> = match kind {
        "ITEM" => heap.registries.items.iter().cloned().collect(),
        "BLOCK" => heap.registries.blocks.iter().cloned().collect(),
        _ => Vec::new(),
    };
    // An entry a mod registered is that mod's own object: yielding a stand-in
    // for the same id would give the registry two objects for one entry, and
    // vanilla code compares them by identity (`BlockEntity.validateBlockState`).
    let registered: std::collections::HashSet<String> = if kind == "BLOCK" {
        heap.registered_blocks
            .iter()
            .map(|record| record.id.clone())
            .collect()
    } else {
        heap.registered_items
            .iter()
            .map(|record| record.id.clone())
            .chain(
                heap.registered_block_items
                    .iter()
                    .map(|(id, _, _)| id.clone()),
            )
            .collect()
    };
    for id in snapshot {
        if registered.contains(&id) {
            continue;
        }
        let already = values.iter().any(|value| {
            value.as_obj().is_some_and(|object| {
                heap.get_native::<Item>(object)
                    .is_some_and(|item| item.id == id)
                    || heap
                        .get_native::<crate::minecraft::Block>(object)
                        .is_some_and(|block| block.id == id)
            })
        });
        if already {
            continue;
        }
        let object = if kind == "BLOCK" {
            heap.alloc_native(crate::minecraft::Block {
                id,
                is_crop: false,
                max_age: 0,
            })
        } else {
            heap.alloc_native(Item { id })
        };
        values.push(Value::Object(Some(object)));
    }
    values
}

/// The id a mod object was registered under in registry `kind`, or a vanilla
/// item/block stand-in's own id.
fn registered_id(heap: &Heap, kind: &str, value: &Value) -> Option<String> {
    let object = value.as_obj()?;
    if let Some(entry) = heap
        .registry_entries
        .iter()
        .find(|entry| entry.kind == kind && entry.value.as_obj() == Some(object))
    {
        return Some(entry.id.clone());
    }
    match kind {
        "ITEM" => heap
            .registered_items
            .iter()
            .find(|record| record.object == object)
            .map(|record| record.id.clone())
            .or_else(|| heap.get_native::<Item>(object).map(|item| item.id.clone())),
        "BLOCK" => heap
            .registered_blocks
            .iter()
            .find(|record| record.object == object)
            .map(|record| record.id.clone())
            .or_else(|| {
                heap.get_native::<Block>(object)
                    .map(|block| block.id.clone())
            }),
        _ => None,
    }
}

/// What a mod writes into and reads back from a built-in registry:
/// `key()`, `register(key, value, info)` (which `Registry.register` calls),
/// `createIntrusiveHolder` and `getKey`. Registrations are kept in
/// `Heap::registry_entries`; the vanilla contents of registries other than
/// items and blocks are not known to the runtime, so lookups of those answer
/// only what mods registered.
fn builtin_registry_method(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    if !matches!(
        class_name,
        "net/minecraft/core/Registry"
            | "net/minecraft/core/WritableRegistry"
            | "net/minecraft/core/DefaultedRegistry"
            | "net/minecraft/core/MappedRegistry"
            | "net/minecraft/core/DefaultedMappedRegistry"
            | "net/minecraft/core/IdMap"
    ) {
        return None;
    }
    let kind = builtin_registry_kind(heap, args.first()?)?;
    let handled = |v: Value| Some(BuiltinResult::Handled(v));
    match (method_name, args.len()) {
        // The registry's own key is its token, as `Registries.X` is.
        ("key", 1) => handled(Value::Object(Some(heap.alloc_string(kind)))),
        ("createIntrusiveHolder", 2) => {
            let object = args[1].as_obj()?;
            if let Some(&holder) = heap.intrusive_holders.get(&object) {
                return handled(Value::Object(Some(holder)));
            }
            let holder = registry_holder(heap, None, args[1].clone());
            heap.intrusive_holders.insert(object, holder);
            handled(Value::Object(Some(holder)))
        }
        ("register", 4) => {
            let id = crate::item_builtins::registration_id(heap, &args[1])?;
            if let Some(existing) = heap
                .registry_entries
                .iter()
                .find(|entry| entry.kind == kind && entry.id == id)
            {
                // A JVM throws IllegalStateException("Adding duplicate key").
                log::warn!("{kind} {id} is registered twice; the first registration is kept");
                return handled(Value::Object(Some(existing.holder)));
            }
            let value = args[2].clone();
            let holder = match value.as_obj().and_then(|o| heap.intrusive_holders.get(&o)) {
                Some(&holder) => {
                    if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(holder) {
                        fields.insert("key".to_string(), args[1].clone());
                    }
                    holder
                }
                None => registry_holder(heap, Some(args[1].clone()), value.clone()),
            };
            heap.registry_entries.push(crate::heap::RegistryEntry {
                kind,
                id,
                value,
                holder,
            });
            handled(Value::Object(Some(holder)))
        }
        // Iteration: what code registered through this registry, in
        // registration order. For items and blocks the host's snapshot of the
        // vanilla list is there too (it names every entry); for the other
        // registries the runtime has no vanilla contents, so it yields what was
        // registered in code — which is what vanilla's own initializers do
        // (`Fluids` registers its fluids, then walks the registry it filled).
        ("iterator" | "stream", 1) => {
            let values = registry_values(heap, &kind);
            let value = if method_name == "stream" {
                heap.alloc_stream(values)
            } else {
                heap.alloc_iterator(values)
            };
            handled(Value::Object(Some(value)))
        }
        ("getKey", 2) => match registered_id(heap, &kind, &args[1]) {
            Some(id) => {
                let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", &id));
                let (namespace, path) = (namespace.to_string(), path.to_string());
                handled(Value::Object(Some(
                    heap.alloc_identifier(&namespace, &path),
                )))
            }
            // Unregistered: `MappedRegistry.getKey` answers null too. For a
            // registry whose vanilla contents the runtime lacks, null is
            // "not known", so say so.
            None => {
                if !matches!(kind.as_str(), "ITEM" | "BLOCK") {
                    log::debug!(
                        "{kind}.getKey of an object no mod registered; vanilla {kind} entries are not modelled"
                    );
                }
                handled(Value::NULL)
            }
        },
        _ => None,
    }
}

/// The object-typed parameters of a method descriptor: (parameter index, class).
fn descriptor_object_params(descriptor: &str) -> Vec<(usize, String)> {
    let inner = descriptor
        .strip_prefix('(')
        .and_then(|d| d.split(')').next())
        .unwrap_or("");
    let bytes = inner.as_bytes();
    let (mut out, mut i, mut index) = (Vec::new(), 0, 0);
    while i < bytes.len() {
        let array = bytes[i] == b'[';
        while bytes[i] == b'[' {
            i += 1;
        }
        if bytes[i] == b'L' {
            let end = inner[i..].find(';').map_or(bytes.len(), |e| i + e);
            if !array {
                out.push((index, inner[i + 1..end].to_string()));
            }
            i = end + 1;
        } else {
            i += 1;
        }
        index += 1;
    }
    out
}

/// The pose matrix `[m00, m01, m10, m11, m20, m21]` of a graphics object's
/// `Matrix3x2fStack`, identity when `pose()` was never called. JOML maps
/// `x' = m00*x + m10*y + m20`, `y' = m01*x + m11*y + m21`.
fn gui_pose_matrix(heap: &Heap, graphics: usize) -> [f32; 6] {
    const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let Some(pose) = heap
        .get_native::<GuiGraphics>(graphics)
        .and_then(|g| g.pose)
    else {
        return IDENTITY;
    };
    let Some(JavaObject::Instance { fields, .. }) = heap.get(pose) else {
        return IDENTITY;
    };
    let mut m = IDENTITY;
    for (slot, name) in ["m00", "m01", "m10", "m11", "m20", "m21"]
        .iter()
        .enumerate()
    {
        if let Some(value) = fields.get(*name) {
            m[slot] = value.as_float();
        }
    }
    if m.iter().all(|v| v.is_finite()) {
        m
    } else {
        IDENTITY
    }
}

/// Apply the GUI pose to a draw in the recording graphics, then record it.
/// Rectangles (fill, gradient, scissor) transform both corners and take the
/// bounds. Text, blits and items move and scale but are not rotated: the host
/// has no rotated text or sprite path, so rotation is not applied to them.
fn push_gui_draw(heap: &mut Heap, graphics: usize, draw: GuiDraw) {
    let m = gui_pose_matrix(heap, graphics);
    let [m00, m01, m10, m11, m20, m21] = m;
    let point = |x: i32, y: i32| -> (f32, f32) {
        let (x, y) = (x as f32, y as f32);
        (m00 * x + m10 * y + m20, m01 * x + m11 * y + m21)
    };
    let rect = |x1: i32, y1: i32, x2: i32, y2: i32| -> (i32, i32, i32, i32) {
        let (ax, ay) = point(x1, y1);
        let (bx, by) = point(x2, y2);
        (
            ax.min(bx).round() as i32,
            ay.min(by).round() as i32,
            ax.max(bx).round() as i32,
            ay.max(by).round() as i32,
        )
    };
    let scale_x = m00.hypot(m01);
    let scale_y = m10.hypot(m11);
    let scaled = |value: i32, scale: f32| -> i32 { (value as f32 * scale).round() as i32 };
    let draw = match draw {
        GuiDraw::Fill {
            x1,
            y1,
            x2,
            y2,
            color,
        } => {
            let (x1, y1, x2, y2) = rect(x1, y1, x2, y2);
            GuiDraw::Fill {
                x1,
                y1,
                x2,
                y2,
                color,
            }
        }
        GuiDraw::Gradient {
            x1,
            y1,
            x2,
            y2,
            color_from,
            color_to,
            horizontal: _,
        } => {
            let (x1, y1, x2, y2) = rect(x1, y1, x2, y2);
            // The local direction is +y, which the pose maps to (m10, m11).
            let (horizontal, flip) = if m10.abs() > m11.abs() {
                (true, m10 < 0.0)
            } else {
                (false, m11 < 0.0)
            };
            let (color_from, color_to) = if flip {
                (color_to, color_from)
            } else {
                (color_from, color_to)
            };
            GuiDraw::Gradient {
                x1,
                y1,
                x2,
                y2,
                color_from,
                color_to,
                horizontal,
            }
        }
        GuiDraw::Scissor { x1, y1, x2, y2 } => {
            let (x1, y1, x2, y2) = rect(x1, y1, x2, y2);
            GuiDraw::Scissor { x1, y1, x2, y2 }
        }
        GuiDraw::Text { text, x, y, color } => {
            let (x, y) = point(x, y);
            GuiDraw::Text {
                text,
                x: x.round() as i32,
                y: y.round() as i32,
                color,
            }
        }
        GuiDraw::Blit {
            texture,
            x,
            y,
            width,
            height,
            u0,
            u1,
            v0,
            v1,
        } => {
            let (x, y) = point(x, y);
            GuiDraw::Blit {
                texture,
                x: x.round() as i32,
                y: y.round() as i32,
                width: scaled(width, scale_x).max(1),
                height: scaled(height, scale_y).max(1),
                u0,
                u1,
                v0,
                v1,
            }
        }
        GuiDraw::Item {
            item,
            x,
            y,
            size,
            rotation,
        } => {
            let (x, y) = point(x, y);
            GuiDraw::Item {
                item,
                x: x.round() as i32,
                y: y.round() as i32,
                size: scaled(size, scale_x).max(1),
                rotation,
            }
        }
        other => other,
    };
    if let Some(g) = heap.get_native_mut::<GuiGraphics>(graphics) {
        g.draws.push(draw);
    }
}

/// Whether an `InteractionHand` value is `OFF_HAND`, as the runtime's
/// constants carry their name.
fn interaction_hand_is_off(heap: &Heap, hand: &Value) -> Option<bool> {
    let id = hand.as_obj()?;
    let JavaObject::Instance { fields, .. } = heap.get(id)? else {
        return None;
    };
    let name = fields.get("name")?.as_obj()?;
    Some(heap.get_string(name)? == "OFF_HAND")
}

/// Where one `ServerLevel.sendParticles` overload keeps the arguments the
/// runtime reads: `[receiver, ...]` positions of the particle type, the
/// first coordinate (x; y and z follow), the count, the first spread (dx; dy
/// and dz follow), and the speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParticleLayout {
    particle: usize,
    x: usize,
    count: usize,
    spread: usize,
    speed: usize,
    /// The `ServerPlayer`-first overload answers a boolean.
    returns_bool: bool,
}

/// The argument layout of a 26.3 `ServerLevel.sendParticles` descriptor, or
/// `None` for one the runtime does not model.
fn particle_layout(descriptor: &str) -> Option<ParticleLayout> {
    const PLAIN: &str = "(Lnet/minecraft/core/particles/ParticleOptions;DDDIDDDD)I";
    let plain = ParticleLayout {
        particle: 1,
        x: 2,
        count: 5,
        spread: 6,
        speed: 9,
        returns_bool: false,
    };
    let flagged = ParticleLayout {
        particle: 1,
        x: 4,
        count: 7,
        spread: 8,
        speed: 11,
        returns_bool: false,
    };
    let player = ParticleLayout {
        particle: 2,
        x: 5,
        count: 8,
        spread: 9,
        speed: 12,
        returns_bool: true,
    };
    match descriptor {
        PLAIN
        | "(Lnet/minecraft/core/particles/ParticleOptions;DDDIDDDDDD)I"
        | "(Lnet/minecraft/core/particles/ParticleOptions;DDDIDDDDLnet/minecraft/network/protocol/game/ClientboundLevelParticlesPacket$RandomizationType;)I"
        | "(Lnet/minecraft/core/particles/ParticleOptions;DDDIDDDDDDLnet/minecraft/network/protocol/game/ClientboundLevelParticlesPacket$RandomizationType;)I" => {
            Some(plain)
        }
        "(Lnet/minecraft/core/particles/ParticleOptions;ZZDDDIDDDD)I"
        | "(Lnet/minecraft/core/particles/ParticleOptions;ZZDDDIDDDDLnet/minecraft/network/protocol/game/ClientboundLevelParticlesPacket$RandomizationType;)I"
        | "(Lnet/minecraft/core/particles/ParticleOptions;ZZDDDIDDDDDDLnet/minecraft/network/protocol/game/ClientboundLevelParticlesPacket$RandomizationType;)I" => {
            Some(flagged)
        }
        "(Lnet/minecraft/server/level/ServerPlayer;Lnet/minecraft/core/particles/ParticleOptions;ZZDDDIDDDD)Z" => {
            Some(player)
        }
        _ => None,
    }
}

/// The system property table `System.getProperties` hands out, made once and
/// kept in a singleton so a mod that sets a property sees it later.
pub(crate) fn system_properties(heap: &mut Heap) -> usize {
    if let Some(id) = heap.get_singleton("system.properties") {
        return id;
    }
    let pairs: Vec<(String, String)> = heap
        .properties
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let entries: Vec<(Value, Value)> = pairs
        .into_iter()
        .map(|(key, value)| {
            (
                Value::Object(Some(heap.alloc_string(key))),
                Value::Object(Some(heap.alloc_string(value))),
            )
        })
        .collect();
    let id = heap.alloc(JavaObject::Map(entries));
    heap.set_singleton("system.properties", id);
    id
}

/// `System.getProperty(key)`: the table's entry, or one of the two that depend
/// on where the game folder is.
pub(crate) fn system_property(heap: &Heap, key: &str) -> Option<String> {
    match key {
        "user.dir" => Some(crate::fs_builtins::game_dir(heap)),
        "java.io.tmpdir" => Some(crate::vfs::join(&crate::fs_builtins::game_dir(heap), "tmp")),
        _ => {
            if let Some(id) = heap.get_singleton("system.properties")
                && let Some(JavaObject::Map(entries)) = heap.get(id)
            {
                return entries
                    .iter()
                    .find(|(entry_key, _)| {
                        entry_key
                            .as_obj()
                            .and_then(|id| heap.get_string(id))
                            .is_some_and(|text| text == key)
                    })
                    .and_then(|(_, value)| {
                        value
                            .as_obj()
                            .and_then(|id| heap.get_string(id))
                            .map(str::to_string)
                    });
            }
            heap.properties.get(key).cloned()
        }
    }
}

fn properties_get(heap: &Heap, table: usize, key: &str) -> Option<String> {
    let JavaObject::Map(entries) = heap.get(table)? else {
        return None;
    };
    entries.iter().find_map(|(entry_key, value)| {
        let matches = entry_key
            .as_obj()
            .and_then(|id| heap.get_string(id))
            .is_some_and(|text| text == key);
        matches
            .then(|| {
                value
                    .as_obj()
                    .and_then(|id| heap.get_string(id))
                    .map(str::to_string)
            })
            .flatten()
    })
}

fn properties_put(heap: &mut Heap, table: usize, key: &str, value: &str) {
    let key_id = heap.alloc_string(key);
    let value_id = heap.alloc_string(value);
    let existing = match heap.get(table) {
        Some(JavaObject::Map(entries)) => entries.iter().position(|(entry_key, _)| {
            entry_key
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .is_some_and(|text| text == key)
        }),
        _ => None,
    };
    if let Some(JavaObject::Map(entries)) = heap.get_mut(table) {
        match existing {
            Some(position) => entries[position].1 = Value::Object(Some(value_id)),
            None => entries.push((Value::Object(Some(key_id)), Value::Object(Some(value_id)))),
        }
    }
}

fn properties_remove(heap: &mut Heap, table: usize, key: &str) {
    let keys: Vec<usize> = match heap.get(table) {
        Some(JavaObject::Map(entries)) => entries
            .iter()
            .filter_map(|(entry_key, _)| {
                let id = entry_key.as_obj()?;
                heap.get_string(id)
                    .is_some_and(|text| text == key)
                    .then_some(id)
            })
            .collect(),
        _ => Vec::new(),
    };
    if let Some(JavaObject::Map(entries)) = heap.get_mut(table) {
        entries.retain(|(entry_key, _)| entry_key.as_obj().is_some_and(|id| !keys.contains(&id)));
    }
}

pub fn dispatch_builtin(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
    descriptor: &str,
    args: &[Value],
    virtual_dispatch: bool,
) -> BuiltinResult {
    // LWJGL's windowing natives (SDL, GLFW) belong to the host: the window, its
    // cursor and text input are the host's, and the interpreter cannot load a
    // shared library. A call into them does nothing and answers the type's zero.
    if class_name.starts_with("org/lwjgl/sdl/") || class_name.starts_with("org/lwjgl/glfw/") {
        let zero = match descriptor.rsplit(')').next().and_then(|r| r.bytes().next()) {
            Some(b'Z' | b'B' | b'C' | b'S' | b'I') => Value::Int(0),
            Some(b'J') => Value::Long(0),
            Some(b'F') => Value::Float(0.0),
            Some(b'D') => Value::Double(0.0),
            _ => Value::NULL,
        };
        return BuiltinResult::Handled(zero);
    }

    // A native list or set reached through `Collection` is still that list or set.
    // So is one reached through the JDK skeleton class its unanswered methods run
    // as (`AbstractList.toString` calls `this.iterator()` as `AbstractCollection`).
    if matches!(
        class_name,
        "java/util/Collection" | "java/util/SequencedCollection"
    ) || virtual_dispatch
        && matches!(
            class_name,
            "java/util/AbstractCollection"
                | "java/util/AbstractList"
                | "java/util/AbstractSequentialList"
                | "java/util/AbstractSet"
                | "java/util/AbstractMap"
                | "java/util/SequencedMap"
        )
    {
        let concrete = args
            .first()
            .and_then(|v| v.as_obj())
            .and_then(|id| match heap.get(id) {
                Some(JavaObject::List(_)) => Some("java/util/List"),
                Some(JavaObject::Set(_)) => Some("java/util/Set"),
                Some(JavaObject::Map(_)) => Some("java/util/Map"),
                _ => None,
            });
        if let Some(concrete) = concrete {
            return dispatch_builtin(
                heap,
                concrete,
                method_name,
                descriptor,
                args,
                virtual_dispatch,
            );
        }
    }

    // A class that extends a modeled collection has the runtime's own list,
    // map or set storage (its superclass constructor installed it). The call
    // site names the subclass — an anonymous `HashMap` subclass calling its
    // inherited `put`, for example — so dispatch by what the receiver holds.
    if virtual_dispatch && !class_name.starts_with("java/") {
        let concrete = args
            .first()
            .and_then(|v| v.as_obj())
            .and_then(|id| match heap.get(id) {
                Some(JavaObject::Map(_)) => Some("java/util/HashMap"),
                Some(JavaObject::List(_)) => Some("java/util/ArrayList"),
                Some(JavaObject::Set(_)) => Some("java/util/HashSet"),
                Some(JavaObject::Iterator { .. }) => Some("java/util/Iterator"),
                Some(JavaObject::Stream(_)) => Some("java/util/stream/Stream"),
                _ => None,
            });
        if let Some(concrete) = concrete {
            return dispatch_builtin(heap, concrete, method_name, descriptor, args, false);
        }
    }

    // A String reached through the CharSequence interface is still a String.
    if class_name == "java/lang/CharSequence"
        && args
            .first()
            .and_then(|v| v.as_obj())
            .is_some_and(|id| heap.get_string(id).is_some())
    {
        return dispatch_builtin(
            heap,
            "java/lang/String",
            method_name,
            descriptor,
            args,
            virtual_dispatch,
        );
    }

    if let Some(result) = field_updater_method(heap, class_name, method_name, args) {
        return result;
    }

    // `Item` and `Item.Properties` record what a mod registers.
    if let Some(result) =
        crate::item_builtins::item_method(heap, class_name, method_name, descriptor, args)
    {
        return result;
    }

    // `BlockPlaceContext` getters for `getStateForPlacement`.
    if let Some(result) = crate::place_builtins::place_method(heap, class_name, method_name, args) {
        return result;
    }

    // Block-state properties: a native IntegerProperty has no Java fields.
    if let Some(result) =
        crate::block_builtins::property_method(heap, class_name, method_name, descriptor, args)
    {
        return result;
    }

    // The item list a creative tab event listener records into.
    if let Some(result) =
        crate::creative_tab_builtins::output_method(heap, method_name, descriptor, args)
    {
        return result;
    }
    // A mod's own tab generator writes into its list with the same calls.
    if let Some(result) = crate::creative_tab_builtins::tab_items_method(heap, method_name, args) {
        return result;
    }

    // Kotlin's null checks. The runtime has no exceptions that cross frames,
    // and the real failure path builds a message by walking a Java stack trace
    // the runtime does not have (it never terminates). Report the null, which
    // is the information that matters, and carry on.
    if class_name == "kotlin/jvm/internal/Intrinsics" {
        let reported = match method_name {
            "checkNotNullParameter"
            | "checkNotNullExpressionValue"
            | "checkNotNull"
            | "checkParameterIsNotNull"
            | "checkExpressionValueIsNotNull" => {
                if args.first().is_some_and(Value::is_null) {
                    let what = args
                        .get(1)
                        .and_then(|v| v.as_obj())
                        .and_then(|id| heap.get_string(id))
                        .unwrap_or("value")
                        .to_string();
                    Some(what)
                } else {
                    return BuiltinResult::Handled(Value::NULL);
                }
            }
            "throwParameterIsNullNPE"
            | "throwNpe"
            | "throwUninitializedPropertyAccessException" => Some(
                args.first()
                    .and_then(|v| v.as_obj())
                    .and_then(|id| heap.get_string(id))
                    .unwrap_or("value")
                    .to_string(),
            ),
            _ => None,
        };
        if let Some(what) = reported {
            log::warn!("Kotlin null check failed for `{what}`; a JVM would throw here");
            return BuiltinResult::Handled(Value::NULL);
        }
    }
    if let Some(result) =
        crate::fs_builtins::file_system_method(heap, class_name, method_name, args)
    {
        return result;
    }
    if let Some(result) =
        crate::num_builtins::numeric_method(heap, class_name, method_name, descriptor, args)
    {
        return result;
    }
    if let Some(result) = crate::text_io::console_method(heap, method_name, descriptor, args) {
        return result;
    }
    if let Some(result) = crate::text_io::text_method(heap, class_name, method_name, args) {
        return result;
    }

    // Collection builtins model the runtime's own List/Map/Set/Iterator objects.
    // A real Java implementation reached through an interface call must run its
    // own bytecode instead, otherwise e.g. AbstractSelectionList$TrackedList.size()
    // would be answered with the native list size (0) and the list would render empty.
    if virtual_dispatch
        && args.first().and_then(|v| v.as_obj()).is_some_and(|id| {
            // The runtime's own map entries are Instances of the interface
            // itself and have no bytecode to run.
            matches!(heap.get(id), Some(JavaObject::Instance { class_name, .. })
                    if class_name != "java/util/Map$Entry")
        })
        && matches!(
            class_name,
            "java/util/List"
                | "java/util/ArrayList"
                | "java/util/LinkedList"
                | "java/util/Collection"
                | "java/lang/Iterable"
                | "java/util/Set"
                | "java/util/HashSet"
                | "java/util/LinkedHashSet"
                | "java/util/TreeSet"
                | "java/util/Map"
                | "java/util/HashMap"
                | "java/util/LinkedHashMap"
                | "java/util/TreeMap"
                | "java/util/concurrent/ConcurrentHashMap"
                | "java/util/concurrent/ConcurrentMap"
                | "java/util/Map$Entry"
                | "java/util/Iterator"
        )
    {
        return BuiltinResult::NotHandled;
    }

    // A formatted-text implementation may be reached through an interface
    // other than `Component` (StrictText, a mod mixin, or a real
    // FormattedText subclass). Keep the conversion in one place so wrapped
    // labels keep their text instead of falling through to a real JDK/Minecraft
    // method whose fields the runtime does not carry.
    if method_name == "getVisualOrderText" && !args.is_empty() {
        let text = if args[0].is_null() {
            String::new()
        } else {
            text_of_value(heap, &args[0])
        };
        let color = args[0].as_obj().and_then(|id| match heap.get(id) {
            Some(JavaObject::Instance { fields, .. }) => fields.get("color").map(Value::as_int),
            _ => None,
        });
        return BuiltinResult::Handled(alloc_formatted_char_sequence_colored(heap, text, color));
    }

    // `FormattedCharSequence` is a functional interface in 26.3: its factories
    // (`forward`, `backward`, `codepoint`, `composite`, `fromPair`, `fromList`)
    // build lambdas that capture the text. `EditBox` renders its value through
    // `forward`, so a lambda there would draw as an empty string and the typed
    // characters would stay invisible. Answer the factories with the runtime's
    // text-carrying sequence instead of executing them.
    if class_name == "net/minecraft/util/FormattedCharSequence" {
        let built = match method_name {
            "forward" | "backward" => {
                let mut text = formatted_text_of_value(heap, args.first().unwrap_or(&Value::NULL));
                if method_name == "backward" {
                    text = text.chars().rev().collect();
                }
                let color = args
                    .first()
                    .and_then(|value| formatted_sequence_color(heap, value));
                Some((text, color))
            }
            "codepoint" => Some((
                char::from_u32(args.first().map(Value::as_int).unwrap_or(0) as u32)
                    .map(|c| c.to_string())
                    .unwrap_or_default(),
                None,
            )),
            "composite" | "fromPair" | "fromList" => {
                let text = args
                    .iter()
                    .map(|value| formatted_text_of_value(heap, value))
                    .collect::<String>();
                let color = args
                    .iter()
                    .find_map(|value| formatted_sequence_color(heap, value));
                Some((text, color))
            }
            _ => None,
        };
        if let Some((text, color)) = built {
            return BuiltinResult::Handled(alloc_formatted_char_sequence_colored(
                heap, text, color,
            ));
        }
    }

    // MixinExtras `@Local` sugar wrappers (`LocalIntRef`, `LocalBooleanRef`,
    // `LocalRef`, ...): a mutable box over a synthesized local value.
    if class_name.contains("mixinextras/sugar/ref/Local") {
        let this = args.first().and_then(Value::as_obj);
        if matches!(
            method_name,
            "get" | "getValue" | "getAsInt" | "getAsBoolean"
        ) {
            return BuiltinResult::Handled(
                this.and_then(|id| match heap.get(id) {
                    Some(JavaObject::Instance { fields, .. }) => fields.get("value").cloned(),
                    _ => None,
                })
                .unwrap_or(Value::NULL),
            );
        }
        if matches!(method_name, "set" | "setValue") && args.len() >= 2 {
            if let Some(id) = this {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(id) {
                    fields.insert("value".into(), args[1].clone());
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    // Mixin callback objects. Only the menu-widget bridge creates these, but
    // a callback may also call `isCancellable`/`cancel`.
    if class_name.ends_with("/injection/callback/CallbackInfo")
        || class_name.ends_with("/injection/callback/CallbackInfoReturnable")
    {
        let info = args.first().and_then(Value::as_obj);
        if method_name == "cancel" {
            if let Some(id) = info {
                if let Some(callback) = heap.get_native_mut::<CallbackInfo>(id) {
                    callback.cancelled = true;
                }
            }
        } else if method_name == "isCancelled" {
            return BuiltinResult::Handled(Value::from_bool(
                info.and_then(|id| heap.get_native::<CallbackInfo>(id))
                    .map(|callback| callback.cancelled)
                    .unwrap_or(false),
            ));
        } else if method_name == "isCancellable" {
            return BuiltinResult::Handled(Value::from_bool(true));
        }
        return BuiltinResult::Handled(Value::NULL);
    }

    // -------------------------------------------------------------
    // 1. java/lang/Object
    // -------------------------------------------------------------
    // A runtime list, set or map renders, hashes and compares as the JDK
    // collection it stands for: those run as its skeleton class
    // (`AbstractCollection.toString`, `AbstractList.equals`, ...).
    if class_name == "java/lang/Object"
        && matches!(method_name, "toString" | "hashCode" | "equals")
        && args
            .first()
            .and_then(Value::as_obj)
            .and_then(|id| heap.get(id))
            .is_some_and(|object| crate::interp::collection_skeleton(object).is_some())
    {
        return BuiltinResult::NotHandled;
    }
    if class_name == "java/lang/Object" {
        if method_name == "<init>" {
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "getClass" && !args.is_empty() {
            // A boxed number is a plain value in the heap-less representation.
            let boxed = match &args[0] {
                Value::Int(_) => Some("java/lang/Integer"),
                Value::Long(_) => Some("java/lang/Long"),
                Value::Float(_) => Some("java/lang/Float"),
                Value::Double(_) => Some("java/lang/Double"),
                _ => None,
            };
            if let Some(boxed) = boxed {
                let cid = heap.alloc_class(boxed);
                return BuiltinResult::Handled(Value::Object(Some(cid)));
            }
            if let Some(id) = args[0].as_obj() {
                let cls_name = heap
                    .get(id)
                    .map(|o| o.class_name().to_string())
                    .unwrap_or_default();
                let cid = heap.alloc_class(cls_name);
                return BuiltinResult::Handled(Value::Object(Some(cid)));
            }
        } else if method_name == "toString" && !args.is_empty() {
            // Mods call toString on formatted text, metadata and style objects.
            // Returning the runtime's internal object id leaks `[Object 123]`
            // into the UI, so use the text fields an object actually carries and
            // leave genuinely opaque objects empty rather than printing an id.
            let text = text_of_value(heap, &args[0]);
            let text = if text.starts_with("[Object ") {
                String::new()
            } else {
                text
            };
            let str_id = heap.alloc_string(text);
            return BuiltinResult::Handled(Value::Object(Some(str_id)));
        } else if method_name == "hashCode" {
            return BuiltinResult::Handled(Value::Int(
                args.get(0).map(|v| v.as_int()).unwrap_or(0),
            ));
        } else if method_name == "equals" && args.len() >= 2 {
            if args[0] == args[1] {
                return BuiltinResult::Handled(Value::from_bool(true));
            }
            if let (Some(id1), Some(id2)) = (args[0].as_obj(), args[1].as_obj()) {
                if let (Some(JavaObject::Class(c1)), Some(JavaObject::Class(c2))) =
                    (heap.get(id1), heap.get(id2))
                {
                    return BuiltinResult::Handled(Value::from_bool(
                        c1.replace('.', "/") == c2.replace('.', "/"),
                    ));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        }
    }

    // Records (`ComponentPath$Leaf`, `ComponentPath$Path` in the focus path)
    // only chain to `Object.<init>` in their constructor; the field writes are
    // in the record's own bytecode. Treat the JDK superclass constructor as the
    // no-op it is instead of failing before `applyFocus` can run.
    if class_name == "java/lang/Record" && method_name == "<init>" {
        return BuiltinResult::Handled(Value::NULL);
    }

    // -------------------------------------------------------------
    // 2. java/lang/Class
    // -------------------------------------------------------------
    if class_name == "java/lang/Class" {
        if method_name == "getName" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                let name = match heap.get(id) {
                    Some(JavaObject::Class(c)) => c.replace('/', "."),
                    _ => "java.lang.Object".to_string(),
                };
                let sid = heap.alloc_string(name);
                return BuiltinResult::Handled(Value::Object(Some(sid)));
            }
        } else if method_name == "getSimpleName" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                let name = match heap.get(id) {
                    Some(JavaObject::Class(c)) => c.rsplit('/').next().unwrap_or(c).to_string(),
                    _ => "Object".to_string(),
                };
                let sid = heap.alloc_string(name);
                return BuiltinResult::Handled(Value::Object(Some(sid)));
            }
        } else if method_name == "isPrimitive" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Class(c)) = heap.get(id) {
                    let is_prim = matches!(
                        c.as_str(),
                        "boolean"
                            | "byte"
                            | "char"
                            | "short"
                            | "int"
                            | "long"
                            | "float"
                            | "double"
                            | "void"
                            | "Z"
                            | "B"
                            | "C"
                            | "S"
                            | "I"
                            | "J"
                            | "F"
                            | "D"
                            | "V"
                    );
                    return BuiltinResult::Handled(Value::from_bool(is_prim));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "getPrimitiveClass" && !args.is_empty() {
            let prim_name = if let Some(id) = args[0].as_obj() {
                heap.get_string(id).unwrap_or("void").to_string()
            } else {
                "void".to_string()
            };
            let cid = heap.alloc_class(prim_name);
            return BuiltinResult::Handled(Value::Object(Some(cid)));
        } else if (method_name == "forName" || method_name == "forName0") && !args.is_empty() {
            let name = if let Some(id) = args[0].as_obj() {
                heap.get_string(id)
                    .map(|s| s.replace('.', "/"))
                    .unwrap_or_else(|| "java/lang/Object".to_string())
            } else {
                "java/lang/Object".to_string()
            };
            let cid = heap.alloc_class(name);
            return BuiltinResult::Handled(Value::Object(Some(cid)));
        } else if method_name == "getClassLoader" {
            // One loader for every mod class, as Fabric's does.
            let loader = heap.alloc(JavaObject::Instance {
                class_name: "java/lang/ClassLoader".into(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(loader)));
        } else if method_name == "getComponentType" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Class(c)) = heap.get(id) {
                    if let Some(rest) = c.strip_prefix('[') {
                        let comp = if rest.starts_with('L') && rest.ends_with(';') {
                            &rest[1..rest.len() - 1]
                        } else if rest.starts_with('[') {
                            rest
                        } else {
                            match rest {
                                "Z" => "boolean",
                                "B" => "byte",
                                "C" => "char",
                                "S" => "short",
                                "I" => "int",
                                "J" => "long",
                                "F" => "float",
                                "D" => "double",
                                _ => rest,
                            }
                        };
                        let cid = heap.alloc_class(comp.to_string());
                        return BuiltinResult::Handled(Value::Object(Some(cid)));
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "isArray" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Class(c)) = heap.get(id) {
                    return BuiltinResult::Handled(Value::from_bool(c.starts_with('[')));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "getModifiers" {
            return BuiltinResult::Handled(Value::Int(1)); // Modifier.PUBLIC
        } else if method_name == "getSuperclass" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Class(c)) = heap.get(id) {
                    if c != "java/lang/Object" && !c.is_empty() {
                        let cid = heap.alloc_class("java/lang/Object");
                        return BuiltinResult::Handled(Value::Object(Some(cid)));
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "getInterfaces" {
            let aid = heap.alloc(JavaObject::Array {
                elem_type: "java/lang/Class".into(),
                elements: Vec::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(aid)));
        } else if method_name == "cast" && args.len() >= 2 {
            return BuiltinResult::Handled(args[1].clone());
        } else if method_name == "getDeclaredFields" || method_name == "getFields" {
            let aid = heap.alloc(JavaObject::Array {
                elem_type: "java/lang/reflect/Field".into(),
                elements: Vec::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(aid)));
        } else if method_name == "getDeclaredMethods" || method_name == "getMethods" {
            let aid = heap.alloc(JavaObject::Array {
                elem_type: "java/lang/reflect/Method".into(),
                elements: Vec::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(aid)));
        } else if method_name == "isInstance" || method_name == "isAssignableFrom" {
            return BuiltinResult::Handled(Value::from_bool(true));
        } else if method_name == "desiredAssertionStatus"
            || method_name == "desiredAssertionStatus0"
        {
            return BuiltinResult::Handled(Value::from_bool(false));
        }
    }

    if class_name == "sun/invoke/util/Wrapper" {
        if method_name == "asPrimitiveType" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Class(c)) = heap.get(id) {
                    let norm = c.replace('.', "/");
                    let prim = match norm.as_str() {
                        "java/lang/Integer" => Some("int"),
                        "java/lang/Boolean" => Some("boolean"),
                        "java/lang/Byte" => Some("byte"),
                        "java/lang/Short" => Some("short"),
                        "java/lang/Character" => Some("char"),
                        "java/lang/Long" => Some("long"),
                        "java/lang/Float" => Some("float"),
                        "java/lang/Double" => Some("double"),
                        "java/lang/Void" => Some("void"),
                        _ => None,
                    };
                    if let Some(p) = prim {
                        let cid = heap.alloc_class(p);
                        return BuiltinResult::Handled(Value::Object(Some(cid)));
                    }
                }
            }
            return BuiltinResult::Handled(args[0].clone());
        } else if method_name == "asWrapperType" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Class(c)) = heap.get(id) {
                    let norm = c.replace('.', "/");
                    let wrap = match norm.as_str() {
                        "int" | "I" => Some("java/lang/Integer"),
                        "boolean" | "Z" => Some("java/lang/Boolean"),
                        "byte" | "B" => Some("java/lang/Byte"),
                        "short" | "S" => Some("java/lang/Short"),
                        "char" | "C" => Some("java/lang/Character"),
                        "long" | "J" => Some("java/lang/Long"),
                        "float" | "F" => Some("java/lang/Float"),
                        "double" | "D" => Some("java/lang/Double"),
                        "void" | "V" => Some("java/lang/Void"),
                        _ => None,
                    };
                    if let Some(w) = wrap {
                        let cid = heap.alloc_class(w);
                        return BuiltinResult::Handled(Value::Object(Some(cid)));
                    }
                }
            }
            return BuiltinResult::Handled(args[0].clone());
        }
    }

    // -------------------------------------------------------------
    // 2b. Unsafe & JVM Internals
    // -------------------------------------------------------------
    if class_name == "java/lang/invoke/MethodHandles$Lookup" {
        if method_name == "findVarHandle" && args.len() >= 3 {
            let field_name = if let Some(id) = args[2].as_obj() {
                heap.get_string(id).unwrap_or("").to_string()
            } else {
                String::new()
            };
            let mut fields = HashMap::new();
            let name_id = heap.alloc_string(field_name);
            fields.insert("fieldName".to_string(), Value::Object(Some(name_id)));
            let vid = heap.alloc(JavaObject::Instance {
                class_name: "java/lang/invoke/VarHandle".to_string(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(vid)));
        }
    }

    if class_name == "java/lang/invoke/VarHandle" {
        // An array `VarHandle` (`AtomicReferenceArray.AA`, an
        // `AtomicIntegerArray`'s) is addressed as (array, index, ...): the
        // element is the slot, as the field is for an object handle.
        if let Some(result) = var_handle_array(heap, method_name, args) {
            return result;
        }
        if method_name.starts_with("set") {
            let field_name = args.get(0).and_then(|v| v.as_obj()).and_then(|vid| {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(vid) {
                    fields
                        .get("fieldName")
                        .and_then(|f| f.as_obj())
                        .and_then(|sid| heap.get_string(sid))
                        .map(|s| s.to_string())
                } else {
                    None
                }
            });
            let target_obj = args.get(1).and_then(|v| v.as_obj());
            let val = args.get(2).cloned().unwrap_or(Value::NULL);
            if let (Some(t_id), Some(fname)) = (target_obj, field_name) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(t_id) {
                    fields.insert(fname, val);
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name.starts_with("compareAndSet")
            || method_name.starts_with("weakCompareAndSet")
        {
            let field_name = args.get(0).and_then(|v| v.as_obj()).and_then(|vid| {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(vid) {
                    fields
                        .get("fieldName")
                        .and_then(|f| f.as_obj())
                        .and_then(|sid| heap.get_string(sid))
                        .map(|s| s.to_string())
                } else {
                    None
                }
            });
            let target_obj = args.get(1).and_then(|v| v.as_obj());
            let new_val = args.get(3).cloned().unwrap_or(Value::NULL);
            if let (Some(t_id), Some(fname)) = (target_obj, field_name) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(t_id) {
                    fields.insert(fname, new_val);
                }
            }
            return BuiltinResult::Handled(Value::from_bool(true));
        } else if method_name.starts_with("compareAndExchange") {
            let field_name = args.get(0).and_then(|v| v.as_obj()).and_then(|vid| {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(vid) {
                    fields
                        .get("fieldName")
                        .and_then(|f| f.as_obj())
                        .and_then(|sid| heap.get_string(sid))
                        .map(|s| s.to_string())
                } else {
                    None
                }
            });
            let target_obj = args.get(1).and_then(|v| v.as_obj());
            let expected = args.get(2).cloned().unwrap_or(Value::NULL);
            let new_val = args.get(3).cloned().unwrap_or(Value::NULL);
            if let (Some(t_id), Some(fname)) = (target_obj, field_name) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(t_id) {
                    fields.insert(fname, new_val);
                }
            }
            return BuiltinResult::Handled(expected);
        } else if method_name.starts_with("get") {
            let field_name = args.get(0).and_then(|v| v.as_obj()).and_then(|vid| {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(vid) {
                    fields
                        .get("fieldName")
                        .and_then(|f| f.as_obj())
                        .and_then(|sid| heap.get_string(sid))
                        .map(|s| s.to_string())
                } else {
                    None
                }
            });
            let target_obj = args.get(1).and_then(|v| v.as_obj());
            if let (Some(t_id), Some(fname)) = (target_obj, field_name) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(t_id) {
                    if let Some(v) = fields.get(&fname) {
                        return BuiltinResult::Handled(v.clone());
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    if class_name == "sun/misc/Unsafe" || class_name == "jdk/internal/misc/Unsafe" {
        if method_name == "getUnsafe" || method_name == "theUnsafe" {
            let uid = heap.alloc(JavaObject::Instance {
                class_name: class_name.to_string(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(uid)));
        } else if matches!(
            method_name,
            "registerNatives"
                | "ensureClassInitialized"
                | "ensureClassInitialized0"
                // The interpreter runs one thread: memory fences order nothing.
                | "fullFence"
                | "loadFence"
                | "storeFence"
        ) {
            // `ensureClassInitialized` runs the class's `<clinit>` in the
            // interpreter before this point; see `Interpreter::execute_method_inner`.
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "arrayBaseOffset" || method_name == "arrayBaseOffset0" {
            return BuiltinResult::Handled(Value::Int(16));
        } else if method_name == "arrayIndexScale" || method_name == "arrayIndexScale0" {
            return BuiltinResult::Handled(Value::Int(8));
        } else if method_name == "staticFieldBase0" || method_name == "staticFieldBase" {
            // No raw memory: a static field's base is the class mirror on a
            // JVM, and the matching `staticFieldOffset` above is a placeholder.
            return BuiltinResult::Handled(Value::NULL);
        } else if matches!(
            method_name,
            "objectFieldOffset" | "objectFieldOffset0" | "objectFieldOffset1"
        ) {
            // An offset names the field: `(Class, name)` or a `Field` object.
            let name = match args.len() {
                3 => args[2]
                    .as_obj()
                    .and_then(|id| heap.get_string(id))
                    .map(str::to_string),
                2 => args[1].as_obj().and_then(|id| match heap.get(id) {
                    Some(JavaObject::Instance { fields, .. }) => fields
                        .get("name")
                        .and_then(Value::as_obj)
                        .and_then(|n| heap.get_string(n))
                        .map(str::to_string),
                    _ => None,
                }),
                _ => None,
            };
            if let Some(name) = name {
                let index = match heap.unsafe_fields.iter().position(|f| *f == name) {
                    Some(index) => index,
                    None => {
                        heap.unsafe_fields.push(name);
                        heap.unsafe_fields.len() - 1
                    }
                };
                return BuiltinResult::Handled(Value::Long(UNSAFE_FIELD_BASE + index as i64));
            }
            return BuiltinResult::Handled(Value::Long(16));
        } else if method_name == "staticFieldOffset" || method_name == "staticFieldOffset0" {
            return BuiltinResult::Handled(Value::Long(16));
        } else if let Some(result) = unsafe_memory(heap, method_name, args) {
            return result;
        } else if method_name.starts_with("compareAndSet")
            || method_name.starts_with("compareAndSwap")
        {
            return BuiltinResult::Handled(Value::from_bool(true));
        } else if method_name.starts_with("get") && method_name.ends_with("Volatile") {
            if descriptor.ends_with(")J") {
                return BuiltinResult::Handled(Value::Long(0));
            } else if descriptor.ends_with(")I") {
                return BuiltinResult::Handled(Value::Int(0));
            } else if descriptor.ends_with(")Z") {
                return BuiltinResult::Handled(Value::from_bool(false));
            } else {
                return BuiltinResult::Handled(Value::NULL);
            }
        } else if method_name.starts_with("put") {
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    if class_name.starts_with("java/net/http/HttpClient")
        || class_name.starts_with("jdk/internal/net/http/HttpClient")
    {
        if method_name == "send" {
            let body_id = heap.alloc_string("{}");
            let mut fields = HashMap::new();
            fields.insert("statusCode".to_string(), Value::Int(404));
            fields.insert("body".to_string(), Value::Object(Some(body_id)));
            let rid = heap.alloc(JavaObject::Instance {
                class_name: "java/net/http/HttpResponse".to_string(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(rid)));
        } else if method_name == "build" {
            let hid = heap.alloc(JavaObject::Instance {
                class_name: "java/net/http/HttpClient".to_string(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(hid)));
        }
    }

    if class_name.starts_with("java/net/http/HttpResponse") {
        if method_name == "statusCode" {
            if let Some(id) = args.get(0).and_then(|v| v.as_obj()) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(code) = fields.get("statusCode") {
                        return BuiltinResult::Handled(code.clone());
                    }
                }
            }
            return BuiltinResult::Handled(Value::Int(404));
        } else if method_name == "body" {
            if let Some(id) = args.get(0).and_then(|v| v.as_obj()) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(body) = fields.get("body") {
                        return BuiltinResult::Handled(body.clone());
                    }
                }
            }
            let bid = heap.alloc_string("{}");
            return BuiltinResult::Handled(Value::Object(Some(bid)));
        }
    }

    if class_name == "java/util/concurrent/CompletableFuture" {
        if method_name == "get" || method_name == "join" || method_name == "waitingGet" {
            if let Some(id) = args.get(0).and_then(|v| v.as_obj()) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(res) = fields.get("result") {
                        return BuiltinResult::Handled(res.clone());
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    if class_name.starts_with("java/util/concurrent/ForkJoinPool") {
        if method_name == "managedBlock" || method_name == "unmanagedBlock" {
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    if class_name.contains("Signaller") || class_name.contains("ManagedBlocker") {
        if method_name == "isReleasable" || method_name == "block" {
            return BuiltinResult::Handled(Value::from_bool(true));
        }
    }

    if class_name == "jdk/internal/misc/CDS" {
        return BuiltinResult::Handled(Value::NULL);
    }

    if class_name == "jdk/internal/misc/VM" {
        match method_name {
            // The VM's own bootstrap step; there is no VM here to initialize.
            "initialize" => return BuiltinResult::Handled(Value::NULL),
            // -1 is the VM's own "no adjustment available" answer for a clock
            // that cannot be tied to a monotonic source.
            // `VM.getNanoTimeAdjustment(offsetSeconds)`: nanoseconds since that
            // `time_t`, `-1` when the offset is outside the range a `long` of
            // nanos can express. `java.time.Clock` seeds itself from it.
            "getNanoTimeAdjustment" => {
                let offset = args.get(1).map_or(0, |v| v.as_long());
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as i128)
                    .unwrap_or(0);
                let nanos = now - i128::from(offset) * 1_000_000_000;
                let value = match i64::try_from(nanos) {
                    Ok(value) if offset >= 0 => value,
                    _ => -1,
                };
                return BuiltinResult::Handled(Value::Long(value));
            }
            _ => {}
        }
    }

    if class_name == "java/lang/System" {
        if method_name == "registerNatives" {
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "lineSeparator" {
            let id = heap.alloc_string("\n");
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "arraycopy" && args.len() >= 5 {
            let src = args[0].as_obj();
            let dest = args[2].as_obj();
            let src_pos = args[1].as_int().max(0) as usize;
            let dest_pos = args[3].as_int().max(0) as usize;
            let len = args[4].as_int().max(0) as usize;
            let source: Vec<Value> = match src.and_then(|id| heap.get(id)) {
                Some(JavaObject::Array { elements, .. }) => elements.clone(),
                Some(JavaObject::List(items)) | Some(JavaObject::Set(items)) => items.clone(),
                _ => Vec::new(),
            };
            if let Some(dest_id) = dest {
                match heap.get_mut(dest_id) {
                    Some(JavaObject::Array { elements, .. }) => {
                        for (offset, value) in
                            source.iter().skip(src_pos).take(len).cloned().enumerate()
                        {
                            let at = dest_pos + offset;
                            if at < elements.len() {
                                elements[at] = value;
                            }
                        }
                    }
                    Some(JavaObject::List(items)) | Some(JavaObject::Set(items)) => {
                        for (offset, value) in
                            source.iter().skip(src_pos).take(len).cloned().enumerate()
                        {
                            let at = dest_pos + offset;
                            if at < items.len() {
                                items[at] = value;
                            } else {
                                items.push(value);
                            }
                        }
                    }
                    _ => {}
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "getenv" {
            // The process environment, as a JVM in this process reads it. The
            // JDK's own `ProcessEnvironment` keys its map by byte-array
            // wrappers whose equality the runtime's maps do not run, so the
            // lookup is answered here.
            if descriptor == "(Ljava/lang/String;)Ljava/lang/String;" {
                let name = args
                    .first()
                    .and_then(Value::as_obj)
                    .and_then(|id| heap.get_string(id))
                    .map(str::to_string);
                let value = name
                    .and_then(|name| std::env::var(name).ok())
                    .map_or(Value::NULL, |value| {
                        Value::Object(Some(heap.alloc_string(value)))
                    });
                return BuiltinResult::Handled(value);
            }
            if descriptor == "()Ljava/util/Map;" {
                let entries = std::env::vars()
                    .map(|(name, value)| {
                        (
                            Value::Object(Some(heap.alloc_string(name))),
                            Value::Object(Some(heap.alloc_string(value))),
                        )
                    })
                    .collect();
                return BuiltinResult::Handled(Value::Object(Some(heap.alloc_map(entries))));
            }
        } else if method_name == "getProperty" && !args.is_empty() {
            let key = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("")
                .to_string();
            if let Some(value) = system_property(heap, &key) {
                let sid = heap.alloc_string(value);
                return BuiltinResult::Handled(Value::Object(Some(sid)));
            }
            if args.len() >= 2 {
                return BuiltinResult::Handled(args[1].clone());
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "setProperty" && args.len() >= 2 {
            let key = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("")
                .to_string();
            let value = args[1]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .map(str::to_string);
            let table = system_properties(heap);
            let old = properties_get(heap, table, &key);
            if let Some(value) = value {
                properties_put(heap, table, &key, &value);
            } else {
                properties_remove(heap, table, &key);
            }
            return BuiltinResult::Handled(old.map_or(Value::NULL, |value| {
                Value::Object(Some(heap.alloc_string(value)))
            }));
        } else if method_name == "clearProperty" && !args.is_empty() {
            let key = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("")
                .to_string();
            let table = system_properties(heap);
            let old = properties_get(heap, table, &key);
            properties_remove(heap, table, &key);
            return BuiltinResult::Handled(old.map_or(Value::NULL, |value| {
                Value::Object(Some(heap.alloc_string(value)))
            }));
        } else if method_name == "getProperties" {
            let id = system_properties(heap);
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "identityHashCode" && !args.is_empty() {
            // A JVM gives every object a stable, arbitrary number; the heap id
            // is stable for the object's life and distinct between objects.
            // A boxed number has no identity here, so it hashes by value.
            let hash = match &args[0] {
                Value::Object(Some(id)) => (*id as i32).wrapping_mul(0x9E37_79B1u32 as i32),
                Value::Object(None) => 0,
                Value::Int(i) => *i,
                Value::Long(l) => (*l ^ (*l >> 32)) as i32,
                Value::Float(f) => f.to_bits() as i32,
                Value::Double(d) => (d.to_bits() ^ (d.to_bits() >> 32)) as i32,
            };
            return BuiltinResult::Handled(Value::Int(hash));
        } else if method_name == "currentTimeMillis" {
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            return BuiltinResult::Handled(Value::Long(ms));
        } else if method_name == "nanoTime" {
            static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
            let elapsed = START.get_or_init(std::time::Instant::now).elapsed();
            return BuiltinResult::Handled(Value::Long(elapsed.as_nanos() as i64));
        }
    }

    // The JVM's own bootstrap registers the natives of these classes; the
    // runtime answers their methods itself, so registering is a no-op.
    if method_name == "registerNatives"
        && matches!(
            class_name,
            "java/lang/Class" | "java/lang/ClassLoader" | "java/lang/Thread" | "java/lang/Object"
        )
    {
        return BuiltinResult::Handled(Value::NULL);
    }

    if class_name == "java/lang/Runtime" {
        match method_name {
            "getRuntime" => {
                let id = heap.alloc(JavaObject::Instance {
                    class_name: "java/lang/Runtime".into(),
                    fields: HashMap::new(),
                });
                return BuiltinResult::Handled(Value::Object(Some(id)));
            }
            "availableProcessors" => {
                let count = std::thread::available_parallelism().map_or(1, |n| n.get());
                return BuiltinResult::Handled(Value::Int(count as i32));
            }
            // The host's real limits: the runtime runs inside the game process.
            "maxMemory" | "totalMemory" => return BuiltinResult::Handled(Value::Long(1 << 30)),
            "freeMemory" => return BuiltinResult::Handled(Value::Long(1 << 29)),
            "gc" | "runFinalization" | "halt" | "exit" => {
                return BuiltinResult::Handled(Value::NULL);
            }
            _ => {}
        }
    }

    if class_name == "jdk/internal/loader/BootLoader" {
        if matches!(
            method_name,
            "setBootLoaderUnnamedModule0" | "setBootLoaderUnnamedModule"
        ) {
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    if class_name == "java/lang/Thread" {
        match method_name {
            // Thread ids start above this offset; the process has one thread.
            "getNextThreadIdOffset" | "nextThreadIdOffset" => {
                return BuiltinResult::Handled(Value::Long(1));
            }
            // `Thread.sleep` really waits: callers poll a clock against it
            // (Iceberg waits for a config file for up to 500 ms in 2 ms steps).
            "sleepNanos0" if args.len() == 1 => {
                let nanos = args[0].as_long().max(0) as u64;
                std::thread::sleep(std::time::Duration::from_nanos(nanos));
                return BuiltinResult::Handled(Value::NULL);
            }
            "interrupted" => return BuiltinResult::Handled(Value::from_bool(false)),
            "isInterrupted" => return BuiltinResult::Handled(Value::from_bool(false)),
            _ => {}
        }
    }

    // `Class`'s native metadata: a member class names its declaring class in
    // its binary name (`Outer$Inner`), and the runtime carries no other member
    // information, so an enclosing method answers null as it does for a
    // top-level or anonymous class.
    if class_name == "java/lang/Class" {
        match method_name {
            "getDeclaringClass0" => {
                let name =
                    args.first()
                        .and_then(|v| v.as_obj())
                        .and_then(|id| match heap.get(id) {
                            Some(JavaObject::Class(name)) => Some(name.clone()),
                            _ => None,
                        });
                let outer = name
                    .as_deref()
                    .and_then(|name| name.rsplit_once('$'))
                    .map(|(outer, _)| outer.to_string());
                if let Some(outer) = outer {
                    let id = heap.alloc(JavaObject::Class(outer));
                    return BuiltinResult::Handled(Value::Object(Some(id)));
                }
                return BuiltinResult::Handled(Value::NULL);
            }
            "getEnclosingMethod0" => return BuiltinResult::Handled(Value::NULL),
            "isHidden" | "isAnonymousClass" | "isLocalClass" => {
                return BuiltinResult::Handled(Value::from_bool(false));
            }
            _ => {}
        }
    }

    // User code never instantiates `Properties` behaviourally; the runtime
    // models one as a heap map so `getProperty`/`setProperty` work and
    // `load`/`store` see real entries.
    if class_name == "java/util/Properties" {
        // The runtime models `Properties` as a heap map (see `system_properties`),
        // so either shape is the receiver.
        let table = args
            .first()
            .and_then(|v| v.as_obj())
            .filter(|id| match heap.get(*id) {
                Some(JavaObject::Map(_)) => true,
                Some(JavaObject::Instance { class_name, .. }) => {
                    class_name == "java/util/Properties"
                }
                _ => false,
            });
        if let Some(table) = table {
            let key = args.get(1).and_then(|v| {
                v.as_obj()
                    .and_then(|id| heap.get_string(id))
                    .map(str::to_string)
            });
            match method_name {
                "getProperty" | "get" => {
                    // `user.dir` and `java.io.tmpdir` are computed from the game
                    // folder, so they are not entries of the table.
                    let value = key.clone().and_then(|key| {
                        properties_get(heap, table, &key).or_else(|| system_property(heap, &key))
                    });
                    if let Some(value) = value {
                        let sid = heap.alloc_string(value);
                        return BuiltinResult::Handled(Value::Object(Some(sid)));
                    }
                    if method_name == "getProperty" && args.len() >= 3 {
                        return BuiltinResult::Handled(args[2].clone());
                    }
                    return BuiltinResult::Handled(Value::NULL);
                }
                "setProperty" | "put" if args.len() >= 3 => {
                    let Some(key) = key else {
                        return BuiltinResult::Handled(Value::NULL);
                    };
                    let value = args[2]
                        .as_obj()
                        .and_then(|id| heap.get_string(id))
                        .map(str::to_string)
                        .unwrap_or_default();
                    let old = properties_get(heap, table, &key);
                    properties_put(heap, table, &key, &value);
                    return BuiltinResult::Handled(old.map_or(Value::NULL, |old| {
                        Value::Object(Some(heap.alloc_string(old)))
                    }));
                }
                "containsKey" => {
                    let found = key
                        .and_then(|key| {
                            properties_get(heap, table, &key)
                                .or_else(|| system_property(heap, &key))
                        })
                        .is_some();
                    return BuiltinResult::Handled(Value::from_bool(found));
                }
                "size" | "isEmpty" => {
                    let entries = match heap.get(table) {
                        Some(JavaObject::Map(entries)) => entries.len(),
                        _ => 0,
                    };
                    return BuiltinResult::Handled(if method_name == "isEmpty" {
                        Value::from_bool(entries == 0)
                    } else {
                        Value::Int(entries as i32)
                    });
                }
                "stringPropertyNames" | "propertyNames" => {
                    let names: Vec<Value> = match heap.get(table) {
                        Some(JavaObject::Map(entries)) => {
                            entries.iter().map(|(key, _)| key.clone()).collect()
                        }
                        _ => Vec::new(),
                    };
                    if method_name == "stringPropertyNames" {
                        let id = heap.alloc(JavaObject::Set(names));
                        return BuiltinResult::Handled(Value::Object(Some(id)));
                    }
                    let id = heap.alloc_list(names);
                    return BuiltinResult::Handled(Value::Object(Some(id)));
                }
                _ => {}
            }
        }
    }

    // `Array.newInstance` bottoms out in the native `newArray`; Kotlin's
    // `toTypedArray` and most collection `toArray(T[])` copies go through it.
    if class_name == "java/lang/reflect/Array"
        && (method_name == "newArray" || method_name == "newInstance")
        && args.len() == 2
    {
        let element = args[0]
            .as_obj()
            .and_then(|id| match heap.get(id) {
                Some(JavaObject::Class(name)) => Some(name.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "java/lang/Object".to_string());
        let length = args[1].as_int().max(0) as usize;
        let default = match element.as_str() {
            "boolean" | "byte" | "char" | "short" | "int" => Value::Int(0),
            "long" => Value::Long(0),
            "float" => Value::Float(0.0),
            "double" => Value::Double(0.0),
            _ => Value::NULL,
        };
        let array = heap.alloc(JavaObject::Array {
            elem_type: element,
            elements: vec![default; length],
        });
        return BuiltinResult::Handled(Value::Object(Some(array)));
    }

    // The runtime never bootstraps Minecraft's registries (it synthesizes the
    // ones mods read), so the "was Bootstrap run?" guards that Minecraft classes
    // call before touching a registry have nothing to check.
    if class_name == "net/minecraft/server/Bootstrap"
        && matches!(method_name, "checkBootstrapCalled" | "validate")
    {
        return BuiltinResult::Handled(Value::NULL);
    }

    // The runtime is the one class loader: every class it runs comes from it, so
    // the JDK's loader hierarchy collapses to a single stand-in. `Charset` and
    // others ask whether a loader is the system one.
    if class_name == "java/lang/ClassLoader"
        && matches!(
            method_name,
            "getSystemClassLoader" | "getPlatformClassLoader" | "getBootClassLoader"
        )
    {
        let key = format!("classloader.{method_name}");
        let id = match heap.get_singleton(&key) {
            Some(id) => id,
            None => {
                let mut fields = HashMap::new();
                fields.insert("boot".to_string(), Value::from_bool(true));
                let id = heap.alloc(JavaObject::Instance {
                    class_name: "java/lang/ClassLoader".into(),
                    fields,
                });
                heap.set_singleton(&key, id);
                id
            }
        };
        return BuiltinResult::Handled(Value::Object(Some(id)));
    }
    if class_name == "jdk/internal/misc/VM" && method_name == "isSystemDomainLoader" {
        // Every loader the runtime hands out is its own.
        let system = args
            .first()
            .and_then(|value| value.as_obj())
            .is_some_and(|id| {
                matches!(heap.get(id), Some(JavaObject::Instance { class_name, .. })
                if class_name == "java/lang/ClassLoader")
            });
        return BuiltinResult::Handled(Value::from_bool(system));
    }

    if class_name == "java/lang/Throwable" {
        if method_name == "fillInStackTrace" {
            return BuiltinResult::Handled(args.get(0).cloned().unwrap_or(Value::NULL));
        }
    }

    // The interpreter does not record stack traces, and the JVM allows
    // `getStackTrace` to return an empty array when it has none. Code that walks
    // the array (Kotlin's null-parameter message does) then ends instead of
    // reading garbage.
    if (class_name == "java/lang/Throwable" || class_name == "java/lang/Thread")
        && method_name == "getStackTrace"
    {
        let array = heap.alloc(JavaObject::Array {
            elem_type: "java/lang/StackTraceElement".into(),
            elements: Vec::new(),
        });
        return BuiltinResult::Handled(Value::Object(Some(array)));
    }
    // The interpreter runs on one thread, so `currentThread()` is one object:
    // lock ownership (`ReentrantLock` compares the owner with it) and
    // thread-identity checks hold across calls.
    // The interpreter has no threads: a `Thread`'s own state fields are the
    // ones a JVM sets in its constructor (`threadStatus` 0 is "not started").
    if class_name == "java/lang/Thread" {
        // The interpreter has no threads: a `Thread` object is a record of what
        // the caller set. Its JVM-private state fields are answered here
        // instead of by `Thread`'s own bytecode, which reads a `ThreadGroup`
        // and a `FieldHolder` the runtime does not model.
        if let Some(this) = args.first().and_then(|v| v.as_obj()) {
            let default_name = match method_name {
                "<init>" => Some(heap.alloc_string("Thread-0")),
                _ => None,
            };
            if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this) {
                match method_name {
                    "<init>" => {
                        fields
                            .entry("threadStatus".to_string())
                            .or_insert(Value::Int(0));
                        fields
                            .entry("daemon".to_string())
                            .or_insert(Value::from_bool(false));
                        fields
                            .entry("priority".to_string())
                            .or_insert(Value::Int(5));
                        if let Some(name) = default_name {
                            fields
                                .entry("name".to_string())
                                .or_insert(Value::Object(Some(name)));
                        }
                    }
                    "setDaemon" if args.len() == 2 => {
                        fields.insert("daemon".to_string(), args[1].clone());
                    }
                    "setPriority" if args.len() == 2 => {
                        fields.insert("priority".to_string(), args[1].clone());
                    }
                    "setName" if args.len() == 2 => {
                        fields.insert("name".to_string(), args[1].clone());
                    }
                    "start" => {
                        fields.insert("started".to_string(), Value::from_bool(true));
                        fields.insert("threadStatus".to_string(), Value::Int(5));
                        // The interpreter is the runnable: the thread's `run`
                        // runs when it starts, as `Thread.start` would run it.
                        return BuiltinResult::Handled(Value::NULL);
                    }
                    "interrupt" => {
                        fields.insert("interrupted".to_string(), Value::from_bool(true));
                    }
                    "isInterrupted" => {
                        let interrupted = fields
                            .get("interrupted")
                            .map(Value::as_bool)
                            .unwrap_or(false);
                        return BuiltinResult::Handled(Value::from_bool(interrupted));
                    }
                    "isDaemon" => {
                        let daemon = fields.get("daemon").map(Value::as_bool).unwrap_or(false);
                        return BuiltinResult::Handled(Value::from_bool(daemon));
                    }
                    "isAlive" | "isVirtual" => {
                        return BuiltinResult::Handled(Value::from_bool(false));
                    }
                    "getPriority" => {
                        let priority = fields.get("priority").map_or(5, Value::as_int);
                        return BuiltinResult::Handled(Value::Int(priority));
                    }
                    "getName" => {
                        return BuiltinResult::Handled(
                            fields.get("name").cloned().unwrap_or(Value::NULL),
                        );
                    }
                    _ => {}
                }
            }
        }
    }
    if class_name == "java/lang/Thread" && method_name == "currentThread" {
        let thread = match heap.get_singleton("thread.current") {
            Some(id) => id,
            None => {
                let id = heap.alloc(JavaObject::Instance {
                    class_name: "java/lang/Thread".into(),
                    fields: HashMap::new(),
                });
                heap.set_singleton("thread.current", id);
                id
            }
        };
        return BuiltinResult::Handled(Value::Object(Some(thread)));
    }

    // -------------------------------------------------------------
    // 3. java/lang/Enum & Primitives
    // -------------------------------------------------------------
    // `Enum.<init>(String name, int ordinal)` is what every enum constructor
    // calls first; keep the two values where `name()` / `ordinal()` read them.
    if class_name == "java/lang/Enum" && method_name == "<init>" && args.len() >= 3 {
        if let Some(this) = args[0].as_obj() {
            if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this) {
                fields.insert("name".into(), args[1].clone());
                fields.insert("ordinal".into(), args[2].clone());
            }
        }
        return BuiltinResult::Handled(Value::NULL);
    }

    if method_name == "compareTo" && descriptor == "(Ljava/lang/Enum;)I" && args.len() >= 2 {
        let ordinal = |v: &Value| match v.as_obj().and_then(|id| heap.get(id)) {
            Some(JavaObject::Instance { fields, .. }) => fields.get("ordinal").map(Value::as_int),
            _ => None,
        };
        if let (Some(a), Some(b)) = (ordinal(&args[0]), ordinal(&args[1])) {
            return BuiltinResult::Handled(Value::Int(a - b));
        }
    }

    if class_name == "java/lang/Enum" || method_name == "ordinal" {
        if method_name == "ordinal" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(ord) = fields.get("ordinal") {
                        return BuiltinResult::Handled(ord.clone());
                    }
                }
            }
            return BuiltinResult::Handled(Value::Int(2));
        } else if method_name == "name" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(name) = fields.get("name") {
                        return BuiltinResult::Handled(name.clone());
                    }
                }
            }
        }
    }

    // Arrays are cloned through either their own class or `Object.clone`
    // (Kotlin emits the latter for an enum's `values()`).
    if (class_name.starts_with('[') || class_name == "java/lang/Object")
        && method_name == "clone"
        && !args.is_empty()
    {
        if let Some(id) = args[0].as_obj() {
            if let Some(JavaObject::Array {
                elem_type,
                elements,
            }) = heap.get(id)
            {
                let cloned_arr = JavaObject::Array {
                    elem_type: elem_type.clone(),
                    elements: elements.clone(),
                };
                let new_id = heap.alloc(cloned_arr);
                return BuiltinResult::Handled(Value::Object(Some(new_id)));
            }
            // `Object.clone` of a bytecode object (`BitSet.clone` calls
            // `super.clone()`): a shallow copy of its fields.
            if class_name == "java/lang/Object" {
                if let Some(JavaObject::Instance { class_name, fields }) = heap.get(id) {
                    let copy = JavaObject::Instance {
                        class_name: class_name.clone(),
                        fields: fields.clone(),
                    };
                    let new_id = heap.alloc(copy);
                    return BuiltinResult::Handled(Value::Object(Some(new_id)));
                }
            }
        }
    }

    if class_name == "java/lang/Integer" {
        if (method_name == "valueOf" || method_name == "intValue") && !args.is_empty() {
            return BuiltinResult::Handled(Value::Int(args[0].as_int()));
        }
    }

    if class_name == "java/lang/Boolean" {
        // A `Boolean` made by reflection holds its value in a field.
        if let Some(JavaObject::Instance { fields, .. }) = args
            .first()
            .and_then(|v| v.as_obj())
            .and_then(|id| heap.get(id))
        {
            if let Some(inner) = fields.get("value").map(|v| v.as_bool()) {
                match method_name {
                    "booleanValue" => return BuiltinResult::Handled(Value::from_bool(inner)),
                    "toString" if args.len() == 1 => {
                        let sid = heap.alloc_string(if inner { "true" } else { "false" });
                        return BuiltinResult::Handled(Value::Object(Some(sid)));
                    }
                    "hashCode" => {
                        return BuiltinResult::Handled(Value::Int(if inner { 1231 } else { 1237 }));
                    }
                    "equals" if args.len() == 2 => {
                        let other = match args[1].as_obj().and_then(|id| heap.get(id)) {
                            Some(JavaObject::Instance { fields, .. }) => {
                                fields.get("value").map(|v| v.as_bool())
                            }
                            _ => None,
                        };
                        return BuiltinResult::Handled(Value::from_bool(other == Some(inner)));
                    }
                    _ => {}
                }
            }
        }
        if method_name == "toString" && descriptor.starts_with("(Z)") && !args.is_empty() {
            let sid = heap.alloc_string(if args[0].as_int() != 0 {
                "true"
            } else {
                "false"
            });
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        }
        // `Boolean.parseBoolean` / `valueOf(String)`: true only for "true"
        // ignoring case; null and anything else is false.
        if (method_name == "parseBoolean" || method_name == "valueOf")
            && descriptor.starts_with("(Ljava/lang/String;)")
            && !args.is_empty()
        {
            let parsed = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .is_some_and(|text| text.eq_ignore_ascii_case("true"));
            if method_name == "parseBoolean" {
                return BuiltinResult::Handled(Value::from_bool(parsed));
            }
            let mut fields = HashMap::new();
            fields.insert("value".to_string(), Value::from_bool(parsed));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "java/lang/Boolean".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        }
        if method_name == "valueOf" && descriptor.starts_with("(Z)") && !args.is_empty() {
            // Boxing: a plain int could not be told from an `Integer`.
            let mut fields = HashMap::new();
            fields.insert("value".to_string(), Value::from_bool(args[0].as_bool()));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "java/lang/Boolean".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        }
        if (method_name == "valueOf" || method_name == "booleanValue") && !args.is_empty() {
            return BuiltinResult::Handled(Value::from_bool(args[0].as_bool()));
        }
    }

    if class_name == "java/lang/Math" {
        if method_name == "min" && args.len() >= 2 {
            if descriptor == "(JJ)J"
                || matches!(
                    (&args[0], &args[1]),
                    (Value::Long(_), _) | (_, Value::Long(_))
                )
            {
                return BuiltinResult::Handled(Value::Long(
                    args[0].as_long().min(args[1].as_long()),
                ));
            } else if descriptor == "(DD)D"
                || matches!(
                    (&args[0], &args[1]),
                    (Value::Double(_), _) | (_, Value::Double(_))
                )
            {
                return BuiltinResult::Handled(Value::Double(
                    args[0].as_double().min(args[1].as_double()),
                ));
            } else if descriptor == "(FF)F"
                || matches!(
                    (&args[0], &args[1]),
                    (Value::Float(_), _) | (_, Value::Float(_))
                )
            {
                return BuiltinResult::Handled(Value::Float(
                    args[0].as_float().min(args[1].as_float()),
                ));
            } else {
                return BuiltinResult::Handled(Value::Int(args[0].as_int().min(args[1].as_int())));
            }
        } else if method_name == "max" && args.len() >= 2 {
            if descriptor == "(JJ)J"
                || matches!(
                    (&args[0], &args[1]),
                    (Value::Long(_), _) | (_, Value::Long(_))
                )
            {
                return BuiltinResult::Handled(Value::Long(
                    args[0].as_long().max(args[1].as_long()),
                ));
            } else if descriptor == "(DD)D"
                || matches!(
                    (&args[0], &args[1]),
                    (Value::Double(_), _) | (_, Value::Double(_))
                )
            {
                return BuiltinResult::Handled(Value::Double(
                    args[0].as_double().max(args[1].as_double()),
                ));
            } else if descriptor == "(FF)F"
                || matches!(
                    (&args[0], &args[1]),
                    (Value::Float(_), _) | (_, Value::Float(_))
                )
            {
                return BuiltinResult::Handled(Value::Float(
                    args[0].as_float().max(args[1].as_float()),
                ));
            } else {
                return BuiltinResult::Handled(Value::Int(args[0].as_int().max(args[1].as_int())));
            }
        } else if method_name == "abs" && !args.is_empty() {
            if descriptor == "(J)J" || matches!(&args[0], Value::Long(_)) {
                return BuiltinResult::Handled(Value::Long(args[0].as_long().abs()));
            } else if descriptor == "(D)D" || matches!(&args[0], Value::Double(_)) {
                return BuiltinResult::Handled(Value::Double(args[0].as_double().abs()));
            } else if descriptor == "(F)F" || matches!(&args[0], Value::Float(_)) {
                return BuiltinResult::Handled(Value::Float(args[0].as_float().abs()));
            } else {
                return BuiltinResult::Handled(Value::Int(args[0].as_int().abs()));
            }
        } else if method_name == "ceil" && !args.is_empty() {
            return BuiltinResult::Handled(Value::Double(args[0].as_double().ceil()));
        } else if method_name == "floor" && !args.is_empty() {
            return BuiltinResult::Handled(Value::Double(args[0].as_double().floor()));
        } else if method_name == "sqrt" && !args.is_empty() {
            return BuiltinResult::Handled(Value::Double(args[0].as_double().sqrt()));
        } else if method_name == "sin" && !args.is_empty() {
            return BuiltinResult::Handled(Value::Double(args[0].as_double().sin()));
        } else if method_name == "cos" && !args.is_empty() {
            return BuiltinResult::Handled(Value::Double(args[0].as_double().cos()));
        } else if method_name == "pow" && args.len() >= 2 {
            return BuiltinResult::Handled(Value::Double(
                args[0].as_double().powf(args[1].as_double()),
            ));
        } else if method_name == "round" && !args.is_empty() {
            if descriptor == "(F)I" || matches!(&args[0], Value::Float(_)) {
                return BuiltinResult::Handled(Value::Int(args[0].as_float().round() as i32));
            } else {
                return BuiltinResult::Handled(Value::Long(args[0].as_double().round() as i64));
            }
        }
    }

    // -------------------------------------------------------------
    // 4. java/lang/String & StringBuilder
    // -------------------------------------------------------------
    // java/text/NumberFormat is reached from `NumberFormat.getInstance()` in mod
    // code that formats counts for translated labels. The JDK implementation
    // depends on locale providers the runtime does not carry, so answer the two
    // operations those labels need.
    if class_name == "java/text/NumberFormat" || class_name == "java/text/DecimalFormat" {
        if method_name == "getInstance" || method_name == "getNumberInstance" {
            let id = heap.alloc(JavaObject::Instance {
                class_name: "java/text/NumberFormat".into(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "format" && args.len() >= 2 {
            let value = &args[1];
            let text = match value {
                Value::Int(i) => i.to_string(),
                Value::Long(l) => l.to_string(),
                Value::Float(f) => f.to_string(),
                Value::Double(d) => d.to_string(),
                Value::Object(Some(id)) => {
                    heap.get_string(*id).map(str::to_string).unwrap_or_default()
                }
                Value::Object(None) => String::new(),
            };
            let sid = heap.alloc_string(text);
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        }
    }

    // `Appendable.append` on a `StringBuilder` (Gson's `AppendableWriter` appends
    // through the interface, not the class).
    if class_name == "java/lang/Appendable" && method_name == "append" && args.len() >= 2 {
        if let Some(this_id) = args[0].as_obj() {
            if matches!(heap.get(this_id), Some(JavaObject::String(_))) {
                let piece = if descriptor.starts_with("(C)") {
                    char::from_u32(args[1].as_int() as u32)
                        .map(|c| c.to_string())
                        .unwrap_or_default()
                } else {
                    let text = match &args[1] {
                        Value::Object(None) => "null".to_string(),
                        other => text_of_value(heap, other),
                    };
                    if args.len() >= 4 {
                        let start = args[2].as_int().max(0) as usize;
                        let end = args[3].as_int().max(0) as usize;
                        text.chars()
                            .skip(start)
                            .take(end.saturating_sub(start))
                            .collect()
                    } else {
                        text
                    }
                };
                if let Some(JavaObject::String(s)) = heap.get_mut(this_id) {
                    s.push_str(&piece);
                }
                return BuiltinResult::Handled(Value::Object(Some(this_id)));
            }
        }
    }

    if class_name == "java/lang/StringBuilder" {
        if method_name == "<init>" {
            let initial = if args.len() > 1 {
                if let Some(id) = args[1].as_obj() {
                    heap.get_string(id).unwrap_or("").to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            if let Some(this_id) = args.get(0).and_then(|v| v.as_obj()) {
                if let Some(obj) = heap.get_mut(this_id) {
                    *obj = JavaObject::String(initial);
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "append" {
            let appended = if args.len() > 1 {
                // `append(C)` receives the UTF-16 code unit as an int. Appending
                // its number instead turns `StringBuilder.append('.')` into the
                // text "46", which silently corrupts keys Mod Menu builds at
                // runtime (for example its numeric translation fallbacks).
                if descriptor == "(C)Ljava/lang/StringBuilder;" {
                    char::from_u32(args[1].as_int() as u32)
                        .map(|c| c.to_string())
                        .unwrap_or_default()
                } else if descriptor.starts_with("(Z)") {
                    // `append(boolean)` receives 0/1 as an int.
                    (args[1].as_int() != 0).to_string()
                } else if descriptor.starts_with("([C)") {
                    // `append(char[])`: Iceberg's tooltip text sink builds a
                    // line one copied codepoint at a time.
                    args[1]
                        .as_obj()
                        .and_then(|id| match heap.get(id) {
                            Some(JavaObject::Array { elements, .. }) => Some(
                                elements
                                    .iter()
                                    .filter_map(|value| char::from_u32(value.as_int() as u32))
                                    .collect::<String>(),
                            ),
                            _ => None,
                        })
                        .unwrap_or_default()
                } else {
                    match &args[1] {
                        Value::Int(i) => i.to_string(),
                        Value::Long(l) => l.to_string(),
                        Value::Float(f) => f.to_string(),
                        Value::Double(d) => d.to_string(),
                        Value::Object(Some(_)) => text_of_value(heap, &args[1]),
                        Value::Object(None) => "null".to_string(),
                    }
                }
            } else {
                String::new()
            };
            if let Some(this_id) = args.get(0).and_then(|v| v.as_obj()) {
                if let Some(JavaObject::String(s)) = heap.get_mut(this_id) {
                    s.push_str(&appended);
                }
                return BuiltinResult::Handled(Value::Object(Some(this_id)));
            }
        } else if method_name == "replace" && args.len() >= 4 {
            // `EditBox.insertText` rewrites the field through
            // `StringBuilder.replace`. Falling through to the JDK class leaves
            // the runtime string unchanged, so typing appears to do nothing.
            if let Some(this_id) = args[0].as_obj() {
                let text = args[3]
                    .as_obj()
                    .and_then(|id| heap.get_string(id))
                    .map(str::to_string)
                    .unwrap_or_default();
                if let Some(JavaObject::String(s)) = heap.get_mut(this_id) {
                    let mut chars: Vec<char> = s.chars().collect();
                    let start = args[1].as_int().max(0) as usize;
                    let end = args[2].as_int().max(0) as usize;
                    let start = start.min(chars.len());
                    let end = end.min(chars.len()).max(start);
                    chars.splice(start..end, text.chars());
                    *s = chars.into_iter().collect();
                }
                return BuiltinResult::Handled(Value::Object(Some(this_id)));
            }
        } else if method_name == "insert" && args.len() >= 3 {
            if let Some(this_id) = args[0].as_obj() {
                let text = if descriptor.starts_with("(ILjava/lang/String;)") {
                    args[2]
                        .as_obj()
                        .and_then(|id| heap.get_string(id))
                        .map(str::to_string)
                } else {
                    Some(value_to_string(heap, &args[2]))
                };
                if let Some(JavaObject::String(s)) = heap.get_mut(this_id) {
                    let mut chars: Vec<char> = s.chars().collect();
                    let at = (args[1].as_int().max(0) as usize).min(chars.len());
                    if let Some(text) = text {
                        chars.splice(at..at, text.chars());
                    }
                    *s = chars.into_iter().collect();
                }
                return BuiltinResult::Handled(Value::Object(Some(this_id)));
            }
        } else if (method_name == "delete" && args.len() >= 3)
            || (method_name == "deleteCharAt" && args.len() >= 2)
        {
            if let Some(this_id) = args[0].as_obj() {
                if let Some(JavaObject::String(s)) = heap.get_mut(this_id) {
                    let mut chars: Vec<char> = s.chars().collect();
                    let (start, end) = if method_name == "deleteCharAt" {
                        let at = args[1].as_int().max(0) as usize;
                        (at, at.saturating_add(1))
                    } else {
                        let a = args[1].as_int().max(0) as usize;
                        let b = args[2].as_int().max(0) as usize;
                        (a, b)
                    };
                    let start = start.min(chars.len());
                    let end = end.min(chars.len()).max(start);
                    chars.drain(start..end);
                    *s = chars.into_iter().collect();
                }
                return BuiltinResult::Handled(Value::Object(Some(this_id)));
            }
        } else if method_name == "setLength" && args.len() >= 2 {
            if let Some(this_id) = args[0].as_obj() {
                if let Some(JavaObject::String(s)) = heap.get_mut(this_id) {
                    let len = args[1].as_int().max(0) as usize;
                    let mut chars: Vec<char> = s.chars().collect();
                    chars.resize(len, '\0');
                    *s = chars.into_iter().collect();
                }
                return BuiltinResult::Handled(Value::NULL);
            }
        } else if method_name == "charAt" && args.len() >= 2 {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::String(s)) = heap.get(id) {
                    let ch = s
                        .chars()
                        .nth(args[1].as_int().max(0) as usize)
                        .unwrap_or('\0');
                    return BuiltinResult::Handled(Value::Int(ch as i32));
                }
            }
            return BuiltinResult::Handled(Value::Int(0));
        } else if method_name == "length" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::String(s)) = heap.get(id) {
                    return BuiltinResult::Handled(Value::Int(s.chars().count() as i32));
                }
            }
            return BuiltinResult::Handled(Value::Int(0));
        } else if method_name == "reverse" && !args.is_empty() {
            if let Some(this_id) = args[0].as_obj() {
                if let Some(JavaObject::String(s)) = heap.get_mut(this_id) {
                    *s = s.chars().rev().collect();
                }
                return BuiltinResult::Handled(Value::Object(Some(this_id)));
            }
        } else if method_name == "toString" && !args.is_empty() {
            if let Some(this_id) = args[0].as_obj() {
                let s = heap.get_string(this_id).unwrap_or("").to_string();
                let sid = heap.alloc_string(s);
                return BuiltinResult::Handled(Value::Object(Some(sid)));
            }
        }
    }

    if class_name == "java/lang/String" {
        if method_name == "toString"
            && descriptor == "()Ljava/lang/String;"
            && args
                .first()
                .and_then(Value::as_obj)
                .is_some_and(|id| heap.get_string(id).is_some())
        {
            // `String.toString()` is the string itself (also when reached
            // through `CharSequence`, as `StringJoiner` does).
            return BuiltinResult::Handled(args[0].clone());
        } else if method_name == "intern" && !args.is_empty() {
            // `Heap::alloc_string` interns by text; a `new String(...)` is a
            // separate object, so answer the interned one.
            let text = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .map(str::to_string);
            return BuiltinResult::Handled(match text {
                Some(text) => Value::Object(Some(heap.alloc_string(text))),
                None => args[0].clone(),
            });
        } else if method_name == "valueOf" && !args.is_empty() {
            let s = match &args[0] {
                Value::Int(i) if descriptor.starts_with("(Z)") => (*i != 0).to_string(),
                Value::Int(i) if descriptor.starts_with("(C)") => char::from_u32(*i as u32)
                    .map(String::from)
                    .unwrap_or_default(),
                Value::Int(i) => i.to_string(),
                Value::Long(l) => l.to_string(),
                Value::Float(f) => f.to_string(),
                Value::Double(d) => d.to_string(),
                Value::Object(Some(id)) => heap.get_string(*id).unwrap_or("").to_string(),
                Value::Object(None) => "null".to_string(),
            };
            let sid = heap.alloc_string(s);
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if method_name == "equals" && args.len() >= 2 {
            let s1 = args[0].as_obj().and_then(|id| heap.get_string(id));
            let s2 = args[1].as_obj().and_then(|id| heap.get_string(id));
            return BuiltinResult::Handled(Value::from_bool(s1 == s2));
        } else if method_name == "length" && !args.is_empty() {
            let len = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                // `String.length` counts UTF-16 code units, not bytes or
                // code points: an astral character is two.
                .map(|s| s.encode_utf16().count() as i32)
                .unwrap_or(0);
            return BuiltinResult::Handled(Value::Int(len));
        } else if method_name == "compareTo" && args.len() >= 2 {
            let s1 = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let s2 = args[1]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let res = match s1.cmp(s2) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            return BuiltinResult::Handled(Value::Int(res));
        } else if method_name == "compareToIgnoreCase" && args.len() >= 2 {
            let s1 = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let s2 = args[1]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let res = match s1.to_lowercase().cmp(&s2.to_lowercase()) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            return BuiltinResult::Handled(Value::Int(res));
        } else if method_name == "toLowerCase" && !args.is_empty() {
            let s = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let sid = heap.alloc_string(s.to_lowercase());
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if method_name == "toUpperCase" && !args.is_empty() {
            let s = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let sid = heap.alloc_string(s.to_uppercase());
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if (method_name == "trim" || method_name == "strip") && !args.is_empty() {
            let s = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("")
                .trim()
                .to_string();
            let sid = heap.alloc_string(s);
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if method_name == "substring" && args.len() >= 2 {
            let s = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let chars: Vec<char> = s.chars().collect();
            let start = args[1].as_int().max(0) as usize;
            let end = args
                .get(2)
                .map(|v| v.as_int().max(0) as usize)
                .unwrap_or(chars.len())
                .min(chars.len());
            let out: String = chars
                .get(start.min(chars.len())..end.max(start.min(chars.len())))
                .unwrap_or_default()
                .iter()
                .collect();
            let sid = heap.alloc_string(out);
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if method_name == "charAt" && args.len() >= 2 {
            let s = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let ch = s
                .chars()
                .nth(args[1].as_int().max(0) as usize)
                .unwrap_or('\0');
            return BuiltinResult::Handled(Value::Int(ch as i32));
        } else if method_name == "contains" && args.len() >= 2 {
            let s1 = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let s2 = args[1]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            return BuiltinResult::Handled(Value::from_bool(s1.contains(s2)));
        } else if method_name == "startsWith" && args.len() >= 2 {
            let s1 = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let s2 = args[1]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            return BuiltinResult::Handled(Value::from_bool(s1.starts_with(s2)));
        } else if method_name == "endsWith" && args.len() >= 2 {
            let s1 = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let s2 = args[1]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            return BuiltinResult::Handled(Value::from_bool(s1.ends_with(s2)));
        } else if method_name == "isEmpty" && !args.is_empty() {
            let s = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            return BuiltinResult::Handled(Value::from_bool(s.is_empty()));
        } else if method_name == "hashCode" && !args.is_empty() {
            let s = args[0]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let mut h: i32 = 0;
            for b in s.bytes() {
                h = h.wrapping_mul(31).wrapping_add(b as i32);
            }
            return BuiltinResult::Handled(Value::Int(h));
        } else if method_name == "format" && !args.is_empty() {
            let (fmt_str, varargs_val) = if args.len() >= 3 {
                (
                    args[1]
                        .as_obj()
                        .and_then(|id| heap.get_string(id))
                        .unwrap_or(""),
                    args.get(2),
                )
            } else {
                (
                    args[0]
                        .as_obj()
                        .and_then(|id| heap.get_string(id))
                        .unwrap_or(""),
                    args.get(1),
                )
            };
            let mut formatted = fmt_str.to_string();
            if let Some(Value::Object(Some(arr_id))) = varargs_val {
                if let Some(JavaObject::Array { elements, .. }) = heap.get(*arr_id) {
                    if fmt_str == "#%06X" || fmt_str == "#%06x" {
                        let val = elements.first().map(|v| v.as_int()).unwrap_or(0);
                        formatted = format!("#{:06X}", val);
                    } else if fmt_str == "%s" {
                        let val = elements
                            .first()
                            .and_then(|v| v.as_obj())
                            .and_then(|id| heap.get_string(id))
                            .unwrap_or("");
                        formatted = val.to_string();
                    } else if fmt_str == "%d" {
                        let val = elements.first().map(|v| v.as_int()).unwrap_or(0);
                        formatted = format!("{val}");
                    }
                }
            }
            let sid = heap.alloc_string(formatted);
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        }
    }

    // -------------------------------------------------------------
    // 5. java/util/Objects
    // -------------------------------------------------------------
    if class_name == "java/util/Objects" {
        if method_name == "requireNonNull" && !args.is_empty() {
            return BuiltinResult::Handled(args[0].clone());
        } else if method_name == "equals" && args.len() >= 2 {
            return BuiltinResult::Handled(Value::from_bool(args[0] == args[1]));
        } else if method_name == "hashCode" && !args.is_empty() {
            return BuiltinResult::Handled(Value::Int(args[0].as_int()));
        } else if method_name == "isNull" && !args.is_empty() {
            return BuiltinResult::Handled(Value::from_bool(args[0].is_null()));
        } else if method_name == "nonNull" && !args.is_empty() {
            return BuiltinResult::Handled(Value::from_bool(!args[0].is_null()));
        } else if method_name == "toString" && !args.is_empty() {
            if args[0].is_null() && args.len() >= 2 {
                return BuiltinResult::Handled(args[1].clone());
            }
            let sid = heap.alloc_string(format!("{:?}", args[0]));
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        }
    }

    // -------------------------------------------------------------
    // 6. java/util/Optional
    // -------------------------------------------------------------
    if class_name == "java/util/Optional" {
        if method_name == "empty" {
            let id = heap.alloc(JavaObject::Instance {
                class_name: "java/util/Optional".into(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if (method_name == "of" || method_name == "ofNullable") && !args.is_empty() {
            let mut fields = HashMap::new();
            if !args[0].is_null() {
                fields.insert("value".into(), args[0].clone());
            }
            let id = heap.alloc(JavaObject::Instance {
                class_name: "java/util/Optional".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "isPresent" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    let has_val = fields.get("value").map(|v| !v.is_null()).unwrap_or(false);
                    return BuiltinResult::Handled(Value::from_bool(has_val));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "isEmpty" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    let has_val = fields.get("value").map(|v| !v.is_null()).unwrap_or(false);
                    return BuiltinResult::Handled(Value::from_bool(!has_val));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(true));
        } else if method_name == "get" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(v) = fields.get("value") {
                        return BuiltinResult::Handled(v.clone());
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "orElse" && args.len() >= 2 {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(v) = fields.get("value") {
                        if !v.is_null() {
                            return BuiltinResult::Handled(v.clone());
                        }
                    }
                }
            }
            return BuiltinResult::Handled(args[1].clone());
        }
    }

    // -------------------------------------------------------------
    // 7. Collections (List, Map, Set, Iterator, Collections)
    // -------------------------------------------------------------
    if class_name == "java/util/Collections" {
        if method_name == "emptyList" {
            let id = heap.alloc_list(Vec::new());
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "emptyMap" {
            let id = heap.alloc_map(Vec::new());
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "emptySet" {
            let id = heap.alloc_set(Vec::new());
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "singletonList" && !args.is_empty() {
            let id = heap.alloc_list(vec![args[0].clone()]);
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "singleton" && !args.is_empty() {
            let id = heap.alloc_set(vec![args[0].clone()]);
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "singletonMap" && args.len() >= 2 {
            let id = heap.alloc_map(vec![(args[0].clone(), args[1].clone())]);
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if (method_name == "unmodifiableList"
            || method_name == "unmodifiableMap"
            || method_name == "unmodifiableSet")
            && !args.is_empty()
        {
            return BuiltinResult::Handled(args[0].clone());
        }
    }

    // List & ArrayList & LinkedList
    if class_name == "java/util/List"
        || class_name == "java/util/ArrayList"
        || class_name == "java/util/LinkedList"
    {
        if method_name == "<init>" {
            // `new ArrayList(collection)` copies it; an `int` capacity
            // argument is not a collection and starts empty.
            let initial = args
                .get(1)
                .map(|source| collection_items(heap, source))
                .unwrap_or_default();
            if let Some(this_id) = args.get(0).and_then(|v| v.as_obj()) {
                if let Some(obj) = heap.get_mut(this_id) {
                    *obj = JavaObject::List(initial);
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "size" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::List(items)) = heap.get(id) {
                    return BuiltinResult::Handled(Value::Int(items.len() as i32));
                }
            }
            return BuiltinResult::Handled(Value::Int(0));
        } else if method_name == "isEmpty" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::List(items)) = heap.get(id) {
                    return BuiltinResult::Handled(Value::from_bool(items.is_empty()));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(true));
        } else if method_name == "get" && args.len() >= 2 {
            let idx = args[1].as_int().max(0) as usize;
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::List(items)) = heap.get(id) {
                    let v = items.get(idx).cloned().unwrap_or(Value::NULL);
                    return BuiltinResult::Handled(v);
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "add" && args.len() >= 2 {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::List(items)) = heap.get_mut(id) {
                    if args.len() == 2 {
                        items.push(args[1].clone());
                    } else {
                        let idx = args[1].as_int().max(0) as usize;
                        let item = args[2].clone();
                        if idx <= items.len() {
                            items.insert(idx, item);
                        } else {
                            items.push(item);
                        }
                    }
                    return BuiltinResult::Handled(Value::from_bool(true));
                }
            }
        } else if method_name == "remove" && args.len() >= 2 {
            // `List.remove(int)` removes by index; the `Object` overload removes
            // the first equal element. Screens rebuild filtered lists with the
            // index form, and without it the removal silently does nothing.
            let indexed = descriptor.starts_with("(I)");
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::List(items)) = heap.get_mut(id) {
                    if indexed {
                        let idx = args[1].as_int().max(0) as usize;
                        let removed = if idx < items.len() {
                            items.remove(idx)
                        } else {
                            Value::NULL
                        };
                        return BuiltinResult::Handled(removed);
                    }
                    if let Some(pos) = items.iter().position(|item| item == &args[1]) {
                        items.remove(pos);
                        return BuiltinResult::Handled(Value::from_bool(true));
                    }
                    return BuiltinResult::Handled(Value::from_bool(false));
                }
            }
            return BuiltinResult::Handled(if indexed {
                Value::NULL
            } else {
                Value::from_bool(false)
            });
        } else if method_name == "addAll" && args.len() >= 2 {
            let added = collection_items(heap, &args[1]);
            if let Some(id) = args[0].as_obj() {
                match heap.get_mut(id) {
                    Some(JavaObject::List(items)) | Some(JavaObject::Set(items)) => {
                        let count = added.len();
                        items.extend(added);
                        return BuiltinResult::Handled(Value::from_bool(count > 0));
                    }
                    _ => {}
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "clear" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::List(items)) = heap.get_mut(id) {
                    items.clear();
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if matches!(method_name, "contains" | "indexOf" | "lastIndexOf") && args.len() == 2 {
            if let Some(JavaObject::List(items)) = args[0].as_obj().and_then(|id| heap.get(id)) {
                let position = if method_name == "lastIndexOf" {
                    items.iter().rposition(|item| *item == args[1])
                } else {
                    items.iter().position(|item| *item == args[1])
                };
                return BuiltinResult::Handled(if method_name == "contains" {
                    Value::from_bool(position.is_some())
                } else {
                    Value::Int(position.map_or(-1, |p| p as i32))
                });
            }
        } else if method_name == "set" && args.len() == 3 {
            if let Some(JavaObject::List(items)) = args[0].as_obj().and_then(|id| heap.get_mut(id))
            {
                let index = args[1].as_int().max(0) as usize;
                if index < items.len() {
                    let old = std::mem::replace(&mut items[index], args[2].clone());
                    return BuiltinResult::Handled(old);
                }
            }
        } else if matches!(method_name, "getFirst" | "getLast") && args.len() == 1 {
            if let Some(JavaObject::List(items)) = args[0].as_obj().and_then(|id| heap.get(id)) {
                let item = if method_name == "getFirst" {
                    items.first()
                } else {
                    items.last()
                };
                return BuiltinResult::Handled(item.cloned().unwrap_or(Value::NULL));
            }
        } else if method_name == "containsAll" && args.len() == 2 {
            let wanted = collection_items(heap, &args[1]);
            if let Some(JavaObject::List(items)) = args[0].as_obj().and_then(|id| heap.get(id)) {
                return BuiltinResult::Handled(Value::from_bool(
                    wanted.iter().all(|w| items.contains(w)),
                ));
            }
        } else if method_name == "iterator" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::List(items)) = heap.get(id) {
                    let iter_id = heap.alloc(JavaObject::Iterator {
                        items: items.clone(),
                        cursor: 0,
                        source: Some(id),
                    });
                    return BuiltinResult::Handled(Value::Object(Some(iter_id)));
                }
            }
        } else if method_name == "subList" && args.len() == 3 {
            // A copy: writes through the sublist do not reach the parent.
            if let Some(JavaObject::List(items)) = args[0].as_obj().and_then(|id| heap.get(id)) {
                let from = (args[1].as_int().max(0) as usize).min(items.len());
                let to = (args[2].as_int().max(0) as usize).clamp(from, items.len());
                let part = items[from..to].to_vec();
                let lid = heap.alloc_list(part);
                return BuiltinResult::Handled(Value::Object(Some(lid)));
            }
        } else if method_name == "toArray" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::List(items)) = heap.get(id) {
                    let aid = heap.alloc(JavaObject::Array {
                        elem_type: "java/lang/Object".into(),
                        elements: items.clone(),
                    });
                    return BuiltinResult::Handled(Value::Object(Some(aid)));
                }
            }
        } else if method_name == "stream" && !args.is_empty() {
            let items = if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::List(items)) = heap.get(id) {
                    items.clone()
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            let sid = heap.alloc_stream(items);
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        }
    }

    // `Collection.toArray(T[])`: a copy typed like the array passed in.
    if matches!(
        class_name,
        "java/util/Collection" | "java/util/List" | "java/util/Set"
    ) && method_name == "toArray"
        && args.len() == 2
    {
        let items = match args[0].as_obj().and_then(|id| heap.get(id)) {
            Some(JavaObject::List(items)) | Some(JavaObject::Set(items)) => Some(items.clone()),
            _ => None,
        };
        let elem_type = match args[1].as_obj().and_then(|id| heap.get(id)) {
            Some(JavaObject::Array { elem_type, .. }) => Some(elem_type.clone()),
            _ => None,
        };
        if let (Some(elements), Some(elem_type)) = (items, elem_type) {
            let array = heap.alloc(JavaObject::Array {
                elem_type,
                elements,
            });
            return BuiltinResult::Handled(Value::Object(Some(array)));
        }
    }

    // Collection & Iterable
    if class_name == "java/util/Collection" || class_name == "java/lang/Iterable" {
        if matches!(method_name, "contains" | "containsAll") && args.len() == 2 {
            let items = match args[0].as_obj().and_then(|id| heap.get(id)) {
                Some(JavaObject::List(items)) | Some(JavaObject::Set(items)) => Some(items.clone()),
                _ => None,
            };
            if let Some(items) = items {
                let found = if method_name == "contains" {
                    items.contains(&args[1])
                } else {
                    collection_items(heap, &args[1])
                        .iter()
                        .all(|wanted| items.contains(wanted))
                };
                return BuiltinResult::Handled(Value::from_bool(found));
            }
        }
        if method_name == "iterator" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                let items = match heap.get(id) {
                    Some(JavaObject::List(items)) => Some(items.clone()),
                    Some(JavaObject::Set(items)) => Some(items.clone()),
                    _ => None,
                };
                if let Some(items) = items {
                    let iter_id = heap.alloc(JavaObject::Iterator {
                        items,
                        cursor: 0,
                        source: Some(id),
                    });
                    return BuiltinResult::Handled(Value::Object(Some(iter_id)));
                }
            }
        } else if method_name == "size" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                let len = match heap.get(id) {
                    Some(JavaObject::List(items)) => items.len(),
                    Some(JavaObject::Set(items)) => items.len(),
                    _ => 0,
                };
                return BuiltinResult::Handled(Value::Int(len as i32));
            }
            return BuiltinResult::Handled(Value::Int(0));
        } else if method_name == "isEmpty" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                let empty = match heap.get(id) {
                    Some(JavaObject::List(items)) => items.is_empty(),
                    Some(JavaObject::Set(items)) => items.is_empty(),
                    _ => true,
                };
                return BuiltinResult::Handled(Value::from_bool(empty));
            }
            return BuiltinResult::Handled(Value::from_bool(true));
        } else if method_name == "stream" && !args.is_empty() {
            let items = if let Some(id) = args[0].as_obj() {
                match heap.get(id) {
                    Some(JavaObject::List(items)) => items.clone(),
                    Some(JavaObject::Set(items)) => items.clone(),
                    Some(JavaObject::Array { elements, .. }) => elements.clone(),
                    Some(JavaObject::Stream(items)) => items.clone(),
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };
            let sid = heap.alloc_stream(items);
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        }
    }

    // java.util.Arrays
    if class_name == "java/util/Arrays" {
        if matches!(method_name, "copyOf" | "copyOfRange") && args.len() >= 2 {
            let Some(id) = args[0].as_obj() else {
                return BuiltinResult::Handled(Value::NULL);
            };
            let Some(JavaObject::Array {
                elem_type,
                elements,
            }) = heap.get(id)
            else {
                return BuiltinResult::NotHandled;
            };
            let elem_type = elem_type.clone();
            let pad = match elem_type.as_str() {
                "boolean" | "byte" | "short" | "char" | "int" => Value::Int(0),
                "long" => Value::Long(0),
                "float" => Value::Float(0.0),
                "double" => Value::Double(0.0),
                _ => Value::NULL,
            };
            let (from, to) = if method_name == "copyOf" {
                (0, args[1].as_int().max(0) as usize)
            } else {
                (
                    args[1].as_int().max(0) as usize,
                    args.get(2).map_or(0, |v| v.as_int().max(0) as usize),
                )
            };
            let copy: Vec<Value> = (from..to)
                .map(|i| elements.get(i).cloned().unwrap_or_else(|| pad.clone()))
                .collect();
            let nid = heap.alloc(JavaObject::Array {
                elem_type,
                elements: copy,
            });
            return BuiltinResult::Handled(Value::Object(Some(nid)));
        } else if method_name == "fill" && args.len() >= 2 {
            let (from, to, value) = if args.len() >= 4 {
                (
                    args[1].as_int().max(0) as usize,
                    args[2].as_int().max(0) as usize,
                    args[3].clone(),
                )
            } else {
                (0, usize::MAX, args[1].clone())
            };
            if let Some(JavaObject::Array { elements, .. }) =
                args[0].as_obj().and_then(|id| heap.get_mut(id))
            {
                let to = to.min(elements.len());
                for slot in elements.iter_mut().take(to).skip(from) {
                    *slot = value.clone();
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "stream" && !args.is_empty() {
            let items = if let Some(id) = args[0].as_obj() {
                match heap.get(id) {
                    Some(JavaObject::Array { elements, .. }) => elements.clone(),
                    Some(JavaObject::List(items)) => items.clone(),
                    Some(JavaObject::Set(items)) => items.clone(),
                    Some(JavaObject::Stream(items)) => items.clone(),
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };
            let sid = heap.alloc_stream(items);
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if method_name == "asList" && !args.is_empty() {
            let items = if let Some(id) = args[0].as_obj() {
                match heap.get(id) {
                    Some(JavaObject::Array { elements, .. }) => elements.clone(),
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            };
            let lid = heap.alloc_list(items);
            return BuiltinResult::Handled(Value::Object(Some(lid)));
        }
    }

    // java.util.Objects
    if class_name == "java/util/Objects" {
        if method_name == "nonNull" && !args.is_empty() {
            return BuiltinResult::Handled(Value::from_bool(!args[0].is_null()));
        } else if method_name == "isNull" && !args.is_empty() {
            return BuiltinResult::Handled(Value::from_bool(args[0].is_null()));
        } else if method_name == "requireNonNull" && !args.is_empty() {
            return BuiltinResult::Handled(args[0].clone());
        } else if method_name == "equals" && args.len() >= 2 {
            return BuiltinResult::Handled(Value::from_bool(args[0] == args[1]));
        }
    }

    // IntStream.range / rangeClosed / of
    if class_name == "java/util/stream/IntStream" {
        match method_name {
            "range" | "rangeClosed" if args.len() == 2 => {
                let end = args[1].as_int() + i32::from(method_name == "rangeClosed");
                let items = (args[0].as_int()..end).map(Value::Int).collect();
                let sid = heap.alloc_stream(items);
                return BuiltinResult::Handled(Value::Object(Some(sid)));
            }
            "of" if args.len() == 1 => {
                let items = match args[0].as_obj().and_then(|id| heap.get(id)) {
                    Some(JavaObject::Array { elements, .. }) => elements.clone(),
                    _ => vec![args[0].clone()],
                };
                let sid = heap.alloc_stream(items);
                return BuiltinResult::Handled(Value::Object(Some(sid)));
            }
            _ => {}
        }
    }

    // ConcurrentHashMap.newKeySet(): a set; the interpreter is single-threaded.
    if class_name == "java/util/concurrent/ConcurrentHashMap" && method_name == "newKeySet" {
        let sid = heap.alloc_set(Vec::new());
        return BuiltinResult::Handled(Value::Object(Some(sid)));
    }

    // The immutable collection factories (`List.of`, `Set.of`, `Map.of`,
    // `List.copyOf`, ...). They return JDK-internal classes the heap does not
    // model, so a caller that later invokes `size`/`get` on the result would
    // see an empty collection. Prism builds its `DynamicColor` values list
    // with `List.of`, so a tooltip color derived from it would be transparent.
    if matches!(class_name, "java/util/List" | "java/util/Set")
        && matches!(method_name, "of" | "copyOf")
    {
        let items = if method_name == "copyOf" {
            collection_items(heap, args.first().unwrap_or(&Value::NULL))
        } else if descriptor.contains("[Ljava/lang/Object;") {
            args.first()
                .and_then(|value| value.as_obj())
                .and_then(|id| match heap.get(id) {
                    Some(JavaObject::Array { elements, .. }) => Some(elements.clone()),
                    _ => None,
                })
                .unwrap_or_default()
        } else {
            args.to_vec()
        };
        let id = if class_name == "java/util/Set" {
            heap.alloc_set(items)
        } else {
            heap.alloc_list(items)
        };
        return BuiltinResult::Handled(Value::Object(Some(id)));
    }

    if class_name == "java/util/Map" && matches!(method_name, "of" | "ofEntries" | "copyOf") {
        let entries: Vec<(Value, Value)> = if method_name == "copyOf" {
            match args
                .first()
                .and_then(|value| value.as_obj())
                .and_then(|id| heap.get(id))
            {
                Some(JavaObject::Map(entries)) => entries.clone(),
                _ => Vec::new(),
            }
        } else if method_name == "ofEntries" {
            collection_items(heap, &args[0])
                .into_iter()
                .filter_map(|entry| {
                    let id = entry.as_obj()?;
                    match heap.get(id)? {
                        JavaObject::Instance { fields, .. } => {
                            Some((fields.get("key")?.clone(), fields.get("value")?.clone()))
                        }
                        _ => None,
                    }
                })
                .collect()
        } else {
            args.chunks_exact(2)
                .map(|pair| (pair[0].clone(), pair[1].clone()))
                .collect()
        };
        let id = heap.alloc(JavaObject::Map(entries));
        return BuiltinResult::Handled(Value::Object(Some(id)));
    }

    // Map & HashMap & LinkedHashMap & ConcurrentHashMap
    if class_name == "java/util/Map"
        || class_name == "java/util/HashMap"
        || class_name == "java/util/LinkedHashMap"
        || class_name == "java/util/concurrent/ConcurrentHashMap"
        || class_name == "java/util/concurrent/ConcurrentMap"
    {
        if method_name == "<init>" {
            if let Some(this_id) = args.get(0).and_then(|v| v.as_obj()) {
                if let Some(obj) = heap.get_mut(this_id) {
                    *obj = JavaObject::Map(Vec::new());
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "put" && args.len() >= 3 {
            let key = args[1].clone();
            let val = args[2].clone();
            if let Some(id) = args[0].as_obj() {
                let existing = map_key_position(heap, id, &key);
                if let Some(JavaObject::Map(entries)) = heap.get_mut(id) {
                    if let Some(pos) = existing {
                        let old = std::mem::replace(&mut entries[pos].1, val);
                        return BuiltinResult::Handled(old);
                    }
                    entries.push((key, val));
                    return BuiltinResult::Handled(Value::NULL);
                }
            }
        } else if method_name == "putAll" && args.len() >= 2 {
            let incoming = match args[1].as_obj().and_then(|id| heap.get(id)) {
                Some(JavaObject::Map(entries)) => entries.clone(),
                _ => Vec::new(),
            };
            if let Some(id) = args[0].as_obj() {
                for (key, val) in incoming {
                    let existing = map_key_position(heap, id, &key);
                    if let Some(JavaObject::Map(entries)) = heap.get_mut(id) {
                        match existing {
                            Some(pos) => entries[pos].1 = val,
                            None => entries.push((key, val)),
                        }
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "putIfAbsent" && args.len() >= 3 {
            if let Some(id) = args[0].as_obj() {
                let existing = map_key_position(heap, id, &args[1]);
                if let Some(JavaObject::Map(entries)) = heap.get_mut(id) {
                    if let Some(pos) = existing {
                        let v = &mut entries[pos].1;
                        if v.is_null() {
                            *v = args[2].clone();
                            return BuiltinResult::Handled(Value::NULL);
                        }
                        return BuiltinResult::Handled(v.clone());
                    }
                    entries.push((args[1].clone(), args[2].clone()));
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "containsValue" && args.len() >= 2 {
            let found = matches!(
                args[0].as_obj().and_then(|id| heap.get(id)),
                Some(JavaObject::Map(entries)) if entries.iter().any(|(_, v)| *v == args[1])
            );
            return BuiltinResult::Handled(Value::from_bool(found));
        } else if method_name == "copyOf" && args.len() == 1 {
            // `Map.copyOf(map)` is static: the map is the first argument.
            if let Some(JavaObject::Map(entries)) = args[0].as_obj().and_then(|id| heap.get(id)) {
                let copy = entries.clone();
                let id = heap.alloc(JavaObject::Map(copy));
                return BuiltinResult::Handled(Value::Object(Some(id)));
            }
        } else if method_name == "get" && args.len() >= 2 {
            if let Some(id) = args[0].as_obj() {
                if let Some(pos) = map_key_position(heap, id, &args[1]) {
                    if let Some(JavaObject::Map(entries)) = heap.get(id) {
                        return BuiltinResult::Handled(entries[pos].1.clone());
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "getOrDefault" && args.len() >= 3 {
            if let Some(id) = args[0].as_obj() {
                if let Some(pos) = map_key_position(heap, id, &args[1]) {
                    if let Some(JavaObject::Map(entries)) = heap.get(id) {
                        return BuiltinResult::Handled(entries[pos].1.clone());
                    }
                }
            }
            return BuiltinResult::Handled(args[2].clone());
        } else if method_name == "containsKey" && args.len() >= 2 {
            if let Some(id) = args[0].as_obj() {
                if map_key_position(heap, id, &args[1]).is_some() {
                    return BuiltinResult::Handled(Value::from_bool(true));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "remove" && args.len() >= 2 {
            if let Some(id) = args[0].as_obj() {
                let existing = map_key_position(heap, id, &args[1]);
                if let Some(JavaObject::Map(entries)) = heap.get_mut(id) {
                    if let Some(pos) = existing {
                        let (_, v) = entries.remove(pos);
                        return BuiltinResult::Handled(v);
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "clear" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Map(entries)) = heap.get_mut(id) {
                    entries.clear();
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "size" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Map(entries)) = heap.get(id) {
                    return BuiltinResult::Handled(Value::Int(entries.len() as i32));
                }
            }
            return BuiltinResult::Handled(Value::Int(0));
        } else if method_name == "isEmpty" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Map(entries)) = heap.get(id) {
                    return BuiltinResult::Handled(Value::from_bool(entries.is_empty()));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(true));
        } else if method_name == "keySet" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Map(entries)) = heap.get(id) {
                    let keys: Vec<Value> = entries.iter().map(|(k, _)| k.clone()).collect();
                    let sid = heap.alloc_set(keys);
                    return BuiltinResult::Handled(Value::Object(Some(sid)));
                }
            }
        } else if method_name == "values" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Map(entries)) = heap.get(id) {
                    let vals: Vec<Value> = entries.iter().map(|(_, v)| v.clone()).collect();
                    let lid = heap.alloc_list(vals);
                    return BuiltinResult::Handled(Value::Object(Some(lid)));
                }
            }
        } else if method_name == "entrySet" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Map(entries)) = heap.get(id) {
                    let mut entry_objs = Vec::new();
                    for (k, v) in entries.clone() {
                        let mut f = HashMap::new();
                        f.insert("key".into(), k);
                        f.insert("value".into(), v);
                        let eid = heap.alloc(JavaObject::Instance {
                            class_name: "java/util/Map$Entry".into(),
                            fields: f,
                        });
                        entry_objs.push(Value::Object(Some(eid)));
                    }
                    let sid = heap.alloc_set(entry_objs);
                    return BuiltinResult::Handled(Value::Object(Some(sid)));
                }
            }
        }
    }

    if class_name == "java/util/Map$Entry" {
        if method_name == "getKey" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(k) = fields.get("key") {
                        return BuiltinResult::Handled(k.clone());
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "getValue" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(v) = fields.get("value") {
                        return BuiltinResult::Handled(v.clone());
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    // Set & HashSet & LinkedHashSet
    if class_name == "java/util/Set"
        || class_name == "java/util/HashSet"
        || class_name == "java/util/LinkedHashSet"
    {
        if method_name == "stream" && args.len() == 1 {
            if let Some(JavaObject::Set(items)) = args[0].as_obj().and_then(|id| heap.get(id)) {
                let items = items.clone();
                let sid = heap.alloc_stream(items);
                return BuiltinResult::Handled(Value::Object(Some(sid)));
            }
        }
        if method_name == "<init>" {
            let mut initial: Vec<Value> = Vec::new();
            if let Some(src) = args.get(1) {
                if src.as_obj().is_some() {
                    for item in collection_items(heap, src) {
                        if !initial.contains(&item) {
                            initial.push(item);
                        }
                    }
                }
            }
            if let Some(this_id) = args.get(0).and_then(|v| v.as_obj()) {
                if let Some(obj) = heap.get_mut(this_id) {
                    *obj = JavaObject::Set(initial);
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "add" && args.len() >= 2 {
            let item = args[1].clone();
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Set(items)) = heap.get_mut(id) {
                    if !items.contains(&item) {
                        items.push(item);
                        return BuiltinResult::Handled(Value::from_bool(true));
                    }
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "remove" && args.len() >= 2 {
            let item = &args[1];
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Set(items)) = heap.get_mut(id) {
                    if let Some(pos) = items.iter().position(|x| x == item) {
                        items.remove(pos);
                        return BuiltinResult::Handled(Value::from_bool(true));
                    }
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "clear" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Set(items)) = heap.get_mut(id) {
                    items.clear();
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "contains" && args.len() >= 2 {
            let item = &args[1];
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Set(items)) = heap.get(id) {
                    return BuiltinResult::Handled(Value::from_bool(items.contains(item)));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "size" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Set(items)) = heap.get(id) {
                    return BuiltinResult::Handled(Value::Int(items.len() as i32));
                }
            }
            return BuiltinResult::Handled(Value::Int(0));
        } else if method_name == "isEmpty" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Set(items)) = heap.get(id) {
                    return BuiltinResult::Handled(Value::from_bool(items.is_empty()));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(true));
        } else if matches!(
            method_name,
            "addAll" | "removeAll" | "retainAll" | "containsAll"
        ) && args.len() >= 2
        {
            let other = collection_items(heap, &args[1]);
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Set(items)) = heap.get_mut(id) {
                    let changed = match method_name {
                        "addAll" => {
                            let before = items.len();
                            for item in other {
                                if !items.contains(&item) {
                                    items.push(item);
                                }
                            }
                            items.len() != before
                        }
                        "removeAll" => {
                            let before = items.len();
                            items.retain(|item| !other.contains(item));
                            items.len() != before
                        }
                        "retainAll" => {
                            let before = items.len();
                            items.retain(|item| other.contains(item));
                            items.len() != before
                        }
                        _ => other.iter().all(|item| items.contains(item)),
                    };
                    return BuiltinResult::Handled(Value::from_bool(changed));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "iterator" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Set(items)) = heap.get(id) {
                    let iter_id = heap.alloc(JavaObject::Iterator {
                        items: items.clone(),
                        cursor: 0,
                        source: Some(id),
                    });
                    return BuiltinResult::Handled(Value::Object(Some(iter_id)));
                }
            }
        }
    }

    // Iterator
    if class_name == "java/util/Iterator" {
        if method_name == "hasNext" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Iterator { items, cursor, .. }) = heap.get(id) {
                    return BuiltinResult::Handled(Value::from_bool(*cursor < items.len()));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "next" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Iterator { items, cursor, .. }) = heap.get_mut(id) {
                    if *cursor < items.len() {
                        let item = items[*cursor].clone();
                        *cursor += 1;
                        return BuiltinResult::Handled(item);
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "remove" && args.len() == 1 {
            // Remove the element `next` last returned from the list or set the
            // iterator walks. The snapshot drops it too, so the cursor still
            // lines up with the source's indices.
            let Some(id) = args[0].as_obj() else {
                return BuiltinResult::Handled(Value::NULL);
            };
            let Some(JavaObject::Iterator {
                items,
                cursor,
                source: Some(source),
            }) = heap.get_mut(id)
            else {
                return BuiltinResult::NotHandled;
            };
            let Some(index) = cursor.checked_sub(1) else {
                // A JVM throws IllegalStateException; the runtime has no
                // cross-frame exceptions, so leave the collection as it is.
                log::warn!("Iterator.remove before next(); nothing removed");
                return BuiltinResult::Handled(Value::NULL);
            };
            let removed = items.remove(index);
            *cursor = index;
            let source = *source;
            match heap.get_mut(source) {
                Some(JavaObject::List(list)) if index < list.len() => {
                    list.remove(index);
                }
                Some(JavaObject::Set(set)) => set.retain(|item| *item != removed),
                _ => {}
            }
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    // -------------------------------------------------------------
    // 8. java/nio/file/Path, Paths, Files & File
    // -------------------------------------------------------------
    if class_name == "jdk/internal/util/ReferencedKeyMap" && method_name == "removeStaleReferences"
    {
        return BuiltinResult::Handled(Value::NULL);
    }

    if class_name.starts_with("java/lang/ref/ReferenceQueue") {
        if method_name == "poll" {
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    if class_name == "java/lang/ThreadLocal" {
        if method_name == "<init>" {
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "get" {
            if let Some(id) = args.get(0).and_then(|v| v.as_obj()) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(v) = fields.get("value") {
                        return BuiltinResult::Handled(v.clone());
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "set" && args.len() >= 2 {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(id) {
                    fields.insert("value".into(), args[1].clone());
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "remove" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(id) {
                    fields.remove("value");
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    if class_name == "java/nio/file/Files" {
        if method_name == "exists" || method_name == "isDirectory" || method_name == "isRegularFile"
        {
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "createDirectories" {
            return BuiltinResult::Handled(args.get(0).cloned().unwrap_or(Value::NULL));
        } else if method_name == "newBufferedWriter" {
            let wid = heap.alloc(JavaObject::Instance {
                class_name: "java/io/BufferedWriter".into(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(wid)));
        } else if method_name == "newBufferedReader" {
            let rid = heap.alloc(JavaObject::Instance {
                class_name: "java/io/BufferedReader".into(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(rid)));
        } else if method_name == "newInputStream" && !args.is_empty() {
            // `ModContainer.getPath` returns a path carrying the mod jar and
            // the entry; the bytes are the mod's own resource, so its
            // `NativeImage.read` sees the real PNG instead of a placeholder.
            let mut bytes = None;
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    let field = |name: &str| {
                        fields
                            .get(name)
                            .and_then(|value| value.as_obj())
                            .and_then(|id| heap.get_string(id))
                            .map(str::to_string)
                    };
                    if let (Some(jar), Some(entry)) = (field("jar"), field("entry")) {
                        bytes = read_mod_resource(&jar, field("nested").as_deref(), &entry);
                    }
                }
            }
            let stream = heap.alloc_native(ByteStream {
                data: bytes.unwrap_or_default(),
                pos: 0,
            });
            return BuiltinResult::Handled(Value::Object(Some(stream)));
        }
    }

    if let Some(result) = byte_stream_method(heap, class_name, method_name, args) {
        return result;
    }

    if class_name == "java/io/InputStream" || class_name == "java/io/ByteArrayInputStream" {
        if method_name == "close" {
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    if class_name == "java/io/Writer"
        || class_name == "java/io/BufferedWriter"
        || class_name == "java/io/StringWriter"
    {
        // A library's own `Writer` subclass (Gson's `AppendableWriter`) has real
        // `write` methods; only the JDK writers with no host behind them are inert.
        let library_writer = args
            .first()
            .and_then(|v| v.as_obj())
            .and_then(|id| heap.get(id))
            .is_some_and(|o| matches!(o, JavaObject::Instance { class_name, .. } if !class_name.starts_with("java/")));
        if !library_writer
            && (method_name == "write" || method_name == "close" || method_name == "flush")
        {
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    if class_name == "java/io/Reader" || class_name == "java/io/BufferedReader" {
        if method_name == "readLine" || method_name == "close" {
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    // -------------------------------------------------------------
    // 9. Either (com/mojang/datafixers/util/Either)
    // -------------------------------------------------------------
    if class_name == "com/mojang/datafixers/util/Either" {
        if method_name == "left" && descriptor.starts_with("(Ljava/lang/Object;)") {
            let mut fields = HashMap::new();
            fields.insert("left".into(), args[0].clone());
            fields.insert("is_left".into(), Value::from_bool(true));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "com/mojang/datafixers/util/Either".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "right" && descriptor.starts_with("(Ljava/lang/Object;)") {
            let mut fields = HashMap::new();
            fields.insert("right".into(), args[0].clone());
            fields.insert("is_left".into(), Value::from_bool(false));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "com/mojang/datafixers/util/Either".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "left" && descriptor == "()Ljava/util/Optional;" {
            // Returns Optional<L>
            let val = args[0]
                .as_obj()
                .and_then(|id| {
                    if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                        fields.get("left").cloned()
                    } else {
                        None
                    }
                })
                .unwrap_or(Value::NULL);
            let mut opt_fields = HashMap::new();
            if !val.is_null() {
                opt_fields.insert("value".into(), val);
            }
            let opt_id = heap.alloc(JavaObject::Instance {
                class_name: "java/util/Optional".into(),
                fields: opt_fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(opt_id)));
        } else if method_name == "right" && args.len() == 1 {
            // Returns Optional<R>
            let val = args[0]
                .as_obj()
                .and_then(|id| {
                    if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                        fields.get("right").cloned()
                    } else {
                        None
                    }
                })
                .unwrap_or(Value::NULL);
            let mut opt_fields = HashMap::new();
            if !val.is_null() {
                opt_fields.insert("value".into(), val);
            }
            let opt_id = heap.alloc(JavaObject::Instance {
                class_name: "java/util/Optional".into(),
                fields: opt_fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(opt_id)));
        }
    }

    // -------------------------------------------------------------
    // 10. Identifier / ResourceLocation
    // -------------------------------------------------------------
    if class_name == "net/minecraft/resources/ResourceLocation"
        || class_name == "net/minecraft/resources/Identifier"
    {
        if method_name == "<init>" {
            let (ns, path) = if args.len() >= 3 {
                let ns_str = args[1]
                    .as_obj()
                    .and_then(|id| heap.get_string(id))
                    .unwrap_or("minecraft")
                    .to_string();
                let path_str = args[2]
                    .as_obj()
                    .and_then(|id| heap.get_string(id))
                    .unwrap_or("")
                    .to_string();
                (ns_str, path_str)
            } else if args.len() >= 2 {
                let full = args[1]
                    .as_obj()
                    .and_then(|id| heap.get_string(id))
                    .unwrap_or("");
                if let Some((n, p)) = full.split_once(':') {
                    (n.to_string(), p.to_string())
                } else {
                    ("minecraft".to_string(), full.to_string())
                }
            } else {
                ("minecraft".to_string(), "".to_string())
            };
            let ns_id = heap.alloc_string(ns);
            let p_id = heap.alloc_string(path);
            if let Some(this_id) = args.get(0).and_then(|v| v.as_obj()) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this_id) {
                    fields.insert("namespace".into(), Value::Object(Some(ns_id)));
                    fields.insert("path".into(), Value::Object(Some(p_id)));
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "fromNamespaceAndPath"
            || method_name == "of"
            || method_name == "parse"
            || method_name == "tryParse"
        {
            let (ns, path) = if args.len() >= 2 {
                let ns_str = args[0]
                    .as_obj()
                    .and_then(|id| heap.get_string(id))
                    .unwrap_or("minecraft")
                    .to_string();
                let path_str = args[1]
                    .as_obj()
                    .and_then(|id| heap.get_string(id))
                    .unwrap_or("")
                    .to_string();
                (ns_str, path_str)
            } else if !args.is_empty() {
                let full = args[0]
                    .as_obj()
                    .and_then(|id| heap.get_string(id))
                    .unwrap_or("");
                if let Some((n, p)) = full.split_once(':') {
                    (n.to_string(), p.to_string())
                } else {
                    ("minecraft".to_string(), full.to_string())
                }
            } else {
                ("minecraft".to_string(), "".to_string())
            };
            let id = heap.alloc_identifier(&ns, &path);
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "getNamespace" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(ns) = fields.get("namespace") {
                        return BuiltinResult::Handled(ns.clone());
                    }
                }
            }
            let sid = heap.alloc_string("minecraft");
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if method_name == "getPath" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(p) = fields.get("path") {
                        return BuiltinResult::Handled(p.clone());
                    }
                }
            }
            let sid = heap.alloc_string("");
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if method_name == "toString" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    let ns = fields
                        .get("namespace")
                        .and_then(|v| v.as_obj())
                        .and_then(|i| heap.get_string(i))
                        .unwrap_or("minecraft");
                    let p = fields
                        .get("path")
                        .and_then(|v| v.as_obj())
                        .and_then(|i| heap.get_string(i))
                        .unwrap_or("");
                    let sid = heap.alloc_string(format!("{ns}:{p}"));
                    return BuiltinResult::Handled(Value::Object(Some(sid)));
                }
            }
        }
    }

    // -------------------------------------------------------------
    // 11. Logging: org/slf4j & org/apache/logging/log4j
    // -------------------------------------------------------------
    // Netty's own logger factory is the one Minecraft's connection code and
    // Netty's leak detector build their loggers from; it answers a logger the
    // same way slf4j's factory does.
    if class_name == "io/netty/util/internal/logging/InternalLoggerFactory"
        && (method_name == "getInstance" || method_name == "newInstance")
    {
        let mut fields = HashMap::new();
        let nid = heap.alloc_string("HFC");
        fields.insert("name".into(), Value::Object(Some(nid)));
        let lid = heap.alloc(JavaObject::Instance {
            class_name: "io/netty/util/internal/logging/InternalLogger".into(),
            fields,
        });
        return BuiltinResult::Handled(Value::Object(Some(lid)));
    }

    if (class_name == "org/slf4j/LoggerFactory"
        || class_name == "org/apache/logging/log4j/LogManager"
        || class_name == "org/apache/log4j/LogManager"
        // `StatusLogger.getLogger()` is the logger a logger factory would
        // return elsewhere: Log4j's own bootstrap code logs through it.
        || class_name == "org/apache/logging/log4j/status/StatusLogger")
        && (method_name == "getLogger"
            || method_name == "getRootLogger"
            || method_name == "getContext"
            || method_name == "getFormatterLogger")
    {
        let name = if let Some(Some(id)) = args.get(0).map(|v| v.as_obj()) {
            heap.get_string(id).unwrap_or("HFC").to_string()
        } else {
            "HFC".to_string()
        };
        let mut fields = HashMap::new();
        let nid = heap.alloc_string(name);
        fields.insert("name".into(), Value::Object(Some(nid)));
        // The logger carries the class of the type the caller asked for, so
        // `instanceof` and `checkcast` see the interface it was produced as.
        let logger_class = if class_name.ends_with("StatusLogger") {
            "org/apache/logging/log4j/status/StatusLogger"
        } else if class_name.starts_with("org/slf4j") {
            "org/slf4j/Logger"
        } else {
            "org/apache/logging/log4j/Logger"
        };
        let lid = heap.alloc(JavaObject::Instance {
            class_name: logger_class.into(),
            fields,
        });
        return BuiltinResult::Handled(Value::Object(Some(lid)));
    }

    // Log4j's `StatusLogger` is a distinct type (`Logger` the abstract class,
    // `StatusLogger` its own), and a static of one is not the other: `instanceof`
    // and `checkcast` must not confuse them.
    if class_name == "org/slf4j/Logger"
        || class_name == "org/apache/logging/log4j/Logger"
        || class_name == "org/apache/logging/log4j/status/StatusLogger"
        || class_name == "org/apache/log4j/Logger"
        || class_name.contains("Logger")
    {
        if method_name.starts_with("is") && method_name.ends_with("Enabled") {
            return BuiltinResult::Handled(Value::Int(1));
        }
        if method_name == "getName" {
            if let Some(Some(this_id)) = args.get(0).map(|v| v.as_obj()) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(this_id) {
                    if let Some(val) = fields.get("name") {
                        return BuiltinResult::Handled(val.clone());
                    }
                }
            }
            let sid = heap.alloc_string("HFC");
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        }
        let msg = args
            .get(1)
            .and_then(|v| v.as_obj())
            .and_then(|id| heap.get_string(id))
            .unwrap_or("");
        log::info!("[MOD] {msg}");
        return BuiltinResult::Handled(Value::NULL);
    }

    // `ThreadLocal`-family reads of a `Thread` object: the runtime has one
    // thread, so a thread's own group is the one a JVM gives every thread.
    if class_name == "java/lang/Thread"
        && matches!(
            method_name,
            "getThreadGroup" | "getUncaughtExceptionHandler" | "getContextClassLoader"
        )
    {
        if method_name == "getThreadGroup" {
            let group = match heap.get_singleton("thread.group") {
                Some(id) => id,
                None => {
                    let mut fields = HashMap::new();
                    fields.insert(
                        "name".to_string(),
                        Value::Object(Some(heap.alloc_string("main"))),
                    );
                    let id = heap.alloc(JavaObject::Instance {
                        class_name: "java/lang/ThreadGroup".into(),
                        fields,
                    });
                    heap.set_singleton("thread.group", id);
                    id
                }
            };
            return BuiltinResult::Handled(Value::Object(Some(group)));
        }
        return BuiltinResult::Handled(Value::NULL);
    }

    // -------------------------------------------------------------
    // 12. Fabric Loader API
    // -------------------------------------------------------------
    if class_name == "net/fabricmc/loader/api/FabricLoader" {
        // The loader's directories are the game folder the runtime runs in.
        if matches!(method_name, "getConfigDir" | "getGameDir") {
            let dir = crate::fs_builtins::game_dir(heap);
            let path = if method_name == "getConfigDir" {
                crate::vfs::join(&dir, "config")
            } else {
                dir
            };
            let text = heap.alloc_string(path);
            let mut fields = HashMap::new();
            fields.insert("path".to_string(), Value::Object(Some(text)));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "java/nio/file/Path".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        }
        if method_name == "getInstance" {
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/fabricmc/loader/api/FabricLoader".into(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "isModLoaded" {
            let mod_id = args
                .get(1)
                .and_then(|v| v.as_obj())
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let loaded = heap.is_mod_loaded(mod_id);
            return BuiltinResult::Handled(Value::from_bool(loaded));
        } else if let Some(dir) = crate::fs_builtins::loader_dir(heap, method_name) {
            return BuiltinResult::Handled(dir);
        } else if method_name == "getConfigDir" || method_name == "getGameDir" {
            let mut fields = HashMap::new();
            let sid = heap.alloc_string("config");
            fields.insert("path".into(), Value::Object(Some(sid)));
            let pid = heap.alloc(JavaObject::Instance {
                class_name: "java/nio/file/Path".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(pid)));
        } else if method_name == "getAllMods" {
            let mut list = Vec::new();
            let defaults = [
                (
                    "fabricloader",
                    "Fabric Loader",
                    "0.16.10",
                    "Fabric Mod Loader",
                ),
                ("minecraft", "Minecraft", "26.3", "Minecraft: Java Edition"),
                ("java", "Java", "25", "Java Runtime Environment"),
            ];
            for (mid, mname, mver, mdesc) in defaults {
                list.push((
                    mid.to_string(),
                    mname.to_string(),
                    mver.to_string(),
                    mdesc.to_string(),
                ));
            }
            for meta in heap.loaded_mods.values() {
                if !list.iter().any(|(id, _, _, _)| id == &meta.id) {
                    list.push((
                        meta.id.clone(),
                        meta.name.clone(),
                        meta.version.clone(),
                        meta.description.clone(),
                    ));
                }
            }
            let mut containers = Vec::new();
            for (mid, mname, mver, mdesc) in list {
                let mut meta_fields = HashMap::new();
                let mid_id = heap.alloc_string(mid);
                let mname_id = heap.alloc_string(mname);
                let mdesc_id = heap.alloc_string(mdesc);
                meta_fields.insert("id".into(), Value::Object(Some(mid_id)));
                meta_fields.insert("name".into(), Value::Object(Some(mname_id)));
                meta_fields.insert("description".into(), Value::Object(Some(mdesc_id)));

                let mut ver_fields = HashMap::new();
                let ver_str_id = heap.alloc_string(mver);
                ver_fields.insert("friendlyString".into(), Value::Object(Some(ver_str_id)));
                let ver_id = heap.alloc(JavaObject::Instance {
                    class_name: "net/fabricmc/loader/api/Version".into(),
                    fields: ver_fields,
                });
                meta_fields.insert("version".into(), Value::Object(Some(ver_id)));

                let meta_id = heap.alloc(JavaObject::Instance {
                    class_name: "net/fabricmc/loader/api/metadata/ModMetadata".into(),
                    fields: meta_fields,
                });

                let mut container_fields = HashMap::new();
                container_fields.insert("metadata".into(), Value::Object(Some(meta_id)));
                let cid = heap.alloc(JavaObject::Instance {
                    class_name: "net/fabricmc/loader/api/ModContainer".into(),
                    fields: container_fields,
                });
                containers.push(Value::Object(Some(cid)));
            }
            let list_id = heap.alloc_list(containers);
            return BuiltinResult::Handled(Value::Object(Some(list_id)));
        } else if method_name == "getModContainer" && args.len() >= 2 {
            let mod_id = args[1]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("")
                .to_string();
            // A mod that is not installed has no container: an empty Optional.
            let mut opt_fields = HashMap::new();
            if heap.is_mod_loaded(&mod_id) {
                let cid = fabric_mod_container(heap, &mod_id);
                opt_fields.insert("value".into(), Value::Object(Some(cid)));
            }
            let opt_id = heap.alloc(JavaObject::Instance {
                class_name: "java/util/Optional".into(),
                fields: opt_fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(opt_id)));
        } else if method_name == "getEntrypointContainers" || method_name == "getEntrypoints" {
            let lid = heap.alloc_list(Vec::new());
            return BuiltinResult::Handled(Value::Object(Some(lid)));
        } else if method_name == "getRawGameVersion" {
            // The same Minecraft version `getModContainer("minecraft")` reports.
            let version = heap.alloc_string("26.3");
            return BuiltinResult::Handled(Value::Object(Some(version)));
        } else if method_name == "isDevelopmentEnvironment" {
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "getEnvironmentType" {
            let mut f = HashMap::new();
            let name_id = heap.alloc_string("CLIENT");
            f.insert("name".into(), Value::Object(Some(name_id)));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/fabricmc/api/EnvType".into(),
                fields: f,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        }
    }

    // ModContainer & ModMetadata
    if class_name == "net/fabricmc/loader/api/ModContainer" {
        if method_name == "getMetadata" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(m) = fields.get("metadata") {
                        return BuiltinResult::Handled(m.clone());
                    }
                }
            }
        } else if method_name == "getPath" && args.len() >= 2 {
            // `ModContainer.getPath("assets/<id>/icon.png")` is how a mod (and
            // Mod Menu for it) reads its own icon. Carry the jar and entry in
            // the path object; `Files.newInputStream` opens it.
            let entry = args[1]
                .as_obj()
                .and_then(|id| heap.get_string(id))
                .unwrap_or("")
                .to_string();
            let mod_id = args[0]
                .as_obj()
                .and_then(|id| match heap.get(id) {
                    Some(JavaObject::Instance { fields, .. }) => fields.get("metadata"),
                    _ => None,
                })
                .and_then(|metadata| metadata.as_obj())
                .and_then(|id| match heap.get(id) {
                    Some(JavaObject::Instance { fields, .. }) => fields.get("id"),
                    _ => None,
                })
                .and_then(|id| id.as_obj())
                .and_then(|id| heap.get_string(id))
                .map(str::to_string)
                .unwrap_or_default();
            let (jar, nested) = heap
                .loaded_mods
                .get(&mod_id)
                .map(|meta| (meta.jar.clone(), meta.nested_jar.clone()))
                .unwrap_or((None, None));
            let mut fields = HashMap::new();
            if let Some(jar) = jar {
                let sid = heap.alloc_string(jar);
                fields.insert("jar".into(), Value::Object(Some(sid)));
            }
            if let Some(nested) = nested {
                let sid = heap.alloc_string(nested);
                fields.insert("nested".into(), Value::Object(Some(sid)));
            }
            let eid = heap.alloc_string(entry);
            fields.insert("entry".into(), Value::Object(Some(eid)));
            let pid = heap.alloc(JavaObject::Instance {
                class_name: "java/nio/file/Path".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(pid)));
        } else if method_name == "getContainingMod" {
            let mod_id = args
                .first()
                .and_then(|v| v.as_obj())
                .and_then(|id| match heap.get(id) {
                    Some(JavaObject::Instance { fields, .. }) => fields.get("metadata").cloned(),
                    _ => None,
                })
                .and_then(|m| m.as_obj())
                .and_then(|id| match heap.get(id) {
                    Some(JavaObject::Instance { fields, .. }) => fields.get("id").cloned(),
                    _ => None,
                })
                .and_then(|v| v.as_obj())
                .and_then(|id| heap.get_string(id).map(str::to_string))
                .unwrap_or_default();
            let mut opt_fields = HashMap::new();
            if let Some(parent) = containing_mod_id(heap, &mod_id) {
                let cid = fabric_mod_container(heap, &parent);
                opt_fields.insert("value".into(), Value::Object(Some(cid)));
            }
            let opt_id = heap.alloc(JavaObject::Instance {
                class_name: "java/util/Optional".into(),
                fields: opt_fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(opt_id)));
        } else if method_name == "getOrigin" {
            let oid = heap.alloc(JavaObject::Instance {
                class_name: "net/fabricmc/loader/api/metadata/ModOrigin".into(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(oid)));
        }
    }

    if class_name.starts_with("net/fabricmc/loader/api/metadata/CustomValue") {
        if let Some(result) = custom_value_method(heap, method_name, args) {
            return result;
        }
    }

    if class_name == "net/fabricmc/loader/api/metadata/ModMetadata" {
        if method_name == "getId" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(mid) = fields.get("id") {
                        return BuiltinResult::Handled(mid.clone());
                    }
                }
            }
            let sid = heap.alloc_string("mod");
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if method_name == "getName" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(n) = fields.get("name") {
                        return BuiltinResult::Handled(n.clone());
                    }
                }
            }
            let sid = heap.alloc_string("Mod");
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if method_name == "getVersion" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(v) = fields.get("version") {
                        return BuiltinResult::Handled(v.clone());
                    }
                }
            }
            let mut vf = HashMap::new();
            let sid = heap.alloc_string("1.0.0");
            vf.insert("friendlyString".into(), Value::Object(Some(sid)));
            let vid = heap.alloc(JavaObject::Instance {
                class_name: "net/fabricmc/loader/api/Version".into(),
                fields: vf,
            });
            return BuiltinResult::Handled(Value::Object(Some(vid)));
        } else if method_name == "getDescription" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(description) = fields.get("description") {
                        return BuiltinResult::Handled(description.clone());
                    }
                }
            }
            let sid = heap.alloc_string("");
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if method_name == "getProvides"
            || method_name == "getAuthors"
            || method_name == "getContributors"
            || method_name == "getLicense"
        {
            let lid = heap.alloc_list(Vec::new());
            return BuiltinResult::Handled(Value::Object(Some(lid)));
        } else if method_name == "containsCustomValue" {
            // Fabric Loader keeps unknown top-level `fabric.mod.json` keys as
            // custom values. Mod Menu reads `fabric-api:module-lifecycle`
            // here, which is what marks every Fabric API module as a library
            // (and gives it a parent), so answering false leaves dozens of
            // modules in the mod list.
            let key = args
                .get(1)
                .and_then(|value| value.as_obj())
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let found = metadata_raw(heap, &args[0])
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .map(|json| {
                    // Fabric Loader keeps custom values under `"custom"`, not
                    // at the top level (`"custom": {"modmenu": ...}`).
                    json.get("custom")
                        .and_then(|custom| custom.get(key))
                        .is_some()
                })
                .unwrap_or(false);
            return BuiltinResult::Handled(Value::from_bool(found));
        } else if method_name == "getCustomValue" {
            // `custom` entries of fabric.mod.json, wrapped for CustomValue.
            let key = args
                .get(1)
                .and_then(|value| value.as_obj())
                .and_then(|id| heap.get_string(id))
                .unwrap_or("")
                .to_string();
            let found = metadata_raw(heap, &args[0])
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .and_then(|json| json.get("custom")?.get(&key).cloned());
            return BuiltinResult::Handled(match found {
                Some(json) => custom_value(heap, &json),
                None => Value::NULL,
            });
        } else if method_name == "getIconPath" {
            // `fabric.mod.json`'s `icon` is a path or a size -> path map. Read
            // it so a mod's own declared icon is used, not just the default
            // `assets/<id>/icon.png` fallback.
            let size = args.get(1).map(Value::as_int).unwrap_or(0).max(0);
            let path = metadata_raw(heap, &args[0])
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .and_then(|json| {
                    let icon = json.get("icon")?;
                    if let Some(path) = icon.as_str() {
                        return Some(path.to_string());
                    }
                    let icons = icon.as_object()?;
                    let mut sizes: Vec<(i32, &str)> = icons
                        .iter()
                        .filter_map(|(key, value)| {
                            Some((key.parse::<i32>().ok()?, value.as_str()?))
                        })
                        .collect();
                    sizes.sort_by_key(|(side, _)| *side);
                    // Fabric Loader's `MapEntry`: the first side covering the
                    // requested size, else the largest one.
                    sizes
                        .iter()
                        .find(|(side, _)| *side >= size)
                        .or_else(|| sizes.last())
                        .map(|(_, path)| (*path).to_string())
                });
            let mut fields = HashMap::new();
            if let Some(path) = path {
                let sid = heap.alloc_string(path);
                fields.insert("value".into(), Value::Object(Some(sid)));
            }
            let opt_id = heap.alloc(JavaObject::Instance {
                class_name: "java/util/Optional".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(opt_id)));
        } else if method_name == "getEnvironment" {
            let mut f = HashMap::new();
            let name_id = heap.alloc_string("CLIENT");
            f.insert("name".into(), Value::Object(Some(name_id)));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/fabricmc/loader/api/metadata/ModEnvironment".into(),
                fields: f,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "getType" {
            let sid = heap.alloc_string("fabric");
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        }
    }

    if class_name == "net/fabricmc/loader/api/Version" {
        if method_name == "getFriendlyString" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(s) = fields.get("friendlyString") {
                        return BuiltinResult::Handled(s.clone());
                    }
                }
            }
            let sid = heap.alloc_string("1.0.0");
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        }
    }

    // -------------------------------------------------------------
    // 13. Fabric Event API: EventFactory & Event
    // -------------------------------------------------------------
    // Fabric's networking and command APIs are host-side: a packet is never
    // sent and a command is never run, so their registration calls do nothing.
    // Constructors and static initializers are not registration calls: they
    // must run their own bytecode, or a class's event/registry statics stay
    // null.
    if !matches!(method_name, "<init>" | "<clinit>")
        && (class_name.contains("networking")
            || class_name.contains("Networking")
            || class_name.starts_with("net/fabricmc/fabric/api/command"))
    {
        if method_name.starts_with("register") || method_name == "isSupported" {
            return BuiltinResult::Handled(Value::Int(1));
        }
        if method_name == "canSend" {
            return BuiltinResult::Handled(Value::Int(0));
        }
        let id = heap.alloc(JavaObject::Instance {
            class_name: class_name.into(),
            fields: HashMap::new(),
        });
        return BuiltinResult::Handled(Value::Object(Some(id)));
    }

    if class_name == "net/fabricmc/fabric/api/event/EventFactory" {
        if method_name == "createArrayBacked" {
            let mut fields = HashMap::new();
            let listeners_id = heap.alloc(JavaObject::Array {
                elem_type: "java/lang/Object".into(),
                elements: Vec::new(),
            });
            fields.insert("listeners".into(), Value::Object(Some(listeners_id)));
            if args.len() > 1 {
                fields.insert("invokerFactory".into(), args[1].clone());
            }
            let eid = heap.alloc(JavaObject::Instance {
                class_name: "net/fabricmc/fabric/api/event/Event".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(eid)));
        }
    }

    if class_name == "net/fabricmc/fabric/api/event/Event"
        || class_name == "com/anthonyhilyard/iceberg/events/Event"
        || class_name == "com/anthonyhilyard/iceberg/events/TypeTrackedEvent"
        || class_name.ends_with("/Event")
        || class_name.contains("event/Event")
    {
        if method_name == "register" && args.len() >= 2 {
            if let Some(this_id) = args[0].as_obj() {
                let listener = if args.len() >= 3 {
                    args[2].clone()
                } else {
                    args[1].clone()
                };
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this_id) {
                    if let Some(Value::Object(Some(arr_id))) = fields.get("listeners") {
                        let arr_id = *arr_id;
                        if let Some(JavaObject::Array { elements, .. }) = heap.get_mut(arr_id) {
                            elements.push(listener);
                        }
                    } else {
                        let arr_id = heap.alloc(JavaObject::Array {
                            elem_type: "java/lang/Object".into(),
                            elements: vec![listener],
                        });
                        if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this_id) {
                            fields.insert("listeners".into(), Value::Object(Some(arr_id)));
                        }
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "invoker" && !args.is_empty() {
            if let Some(this_id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(this_id) {
                    if let Some(Value::Object(Some(arr_id))) = fields.get("listeners") {
                        if let Some(JavaObject::Array { elements, .. }) = heap.get(*arr_id) {
                            if let Some(first) = elements.first() {
                                return BuiltinResult::Handled(first.clone());
                            }
                        }
                    }
                }
            }
            return BuiltinResult::Handled(args[0].clone());
        } else if method_name == "listenerCount" && !args.is_empty() {
            if let Some(this_id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(this_id) {
                    if let Some(Value::Object(Some(arr_id))) = fields.get("listeners") {
                        if let Some(JavaObject::Array { elements, .. }) = heap.get(*arr_id) {
                            return BuiltinResult::Handled(Value::Int(elements.len() as i32));
                        }
                    }
                }
            }
            return BuiltinResult::Handled(Value::Int(0));
        }
    }

    // -------------------------------------------------------------
    // 14. JamLib API
    // -------------------------------------------------------------
    if class_name == "io/github/jamalam360/jamlib/JamLib" {
        if method_name == "checkForJarRenaming" {
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    if class_name == "io/github/jamalam360/jamlib/api/platform/Platform" {
        if method_name == "getModLoader" {
            let mut fields = HashMap::new();
            let name_id = heap.alloc_string("Fabric");
            fields.insert("name".into(), Value::Object(Some(name_id)));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "io/github/jamalam360/jamlib/api/platform/ModLoader".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        }
    }

    if class_name == "io/github/jamalam360/jamlib/api/platform/ModLoader" {
        if method_name == "name" {
            let id = heap.alloc_string("Fabric");
            return BuiltinResult::Handled(Value::Object(Some(id)));
        }
    }

    if class_name == "io/github/jamalam360/jamlib/api/events/core/CancellableEvent"
        || class_name == "io/github/jamalam360/jamlib/api/events/core/Event"
    {
        if method_name == "listen" && args.len() >= 2 {
            if let Some(this_id) = args[0].as_obj() {
                if let Some(listener) = args.last() {
                    let mut arr_id_opt = None;
                    if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this_id) {
                        fields.insert("listener".into(), listener.clone());
                        if let Some(Value::Object(Some(arr_id))) = fields.get("listeners") {
                            arr_id_opt = Some(*arr_id);
                        }
                    }
                    if let Some(arr_id) = arr_id_opt {
                        if let Some(JavaObject::Array { elements, .. }) = heap.get_mut(arr_id) {
                            elements.push(listener.clone());
                        }
                    } else {
                        let arr_id = heap.alloc(JavaObject::Array {
                            elem_type: "java/lang/Object".into(),
                            elements: vec![listener.clone()],
                        });
                        if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this_id) {
                            fields.insert("listeners".into(), Value::Object(Some(arr_id)));
                        }
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    if class_name == "io/github/jamalam360/jamlib/api/events/core/EventResult" {
        if method_name == "pass" {
            let id = heap.alloc_event_result(EventResult::pass());
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "cancel" && !args.is_empty() {
            let val = args[0].clone();
            let id = heap.alloc_event_result(EventResult::cancel(val));
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if (method_name == "isCancelled" || method_name == "wasCancelled")
            && !args.is_empty()
        {
            if let Some(id) = args[0].as_obj() {
                if let Some(res) = heap.get_native::<EventResult>(id) {
                    return BuiltinResult::Handled(Value::from_bool(res.cancelled));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "value" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(res) = heap.get_native::<EventResult>(id) {
                    return BuiltinResult::Handled(res.value.clone());
                }
            }
        }
    }

    // -------------------------------------------------------------
    // 14b. Client GUI
    // -------------------------------------------------------------
    // Minecraft's client singleton. Mods read `font`, `gui`, `options`,
    // `level`, `player` and `window` from it, and open their screens through
    // `minecraft.gui.setScreen`.
    if class_name == "net/minecraft/client/Minecraft" {
        if method_name == "getInstance" {
            if let Some(id) = heap.get_singleton("net/minecraft/client/Minecraft") {
                return BuiltinResult::Handled(Value::Object(Some(id)));
            }
            let font = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/client/gui/Font".into(),
                fields: [("lineHeight".to_string(), Value::Int(9))].into(),
            });
            let gui = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/client/gui/Gui".into(),
                fields: [("screen".to_string(), Value::NULL)].into(),
            });
            let options = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/client/Options".into(),
                fields: [
                    ("guiScale".to_string(), Value::Int(2)),
                    ("renderDistance".to_string(), Value::Int(12)),
                    ("fullscreen".to_string(), Value::from_bool(false)),
                ]
                .into(),
            });
            let window = heap.alloc(JavaObject::Instance {
                class_name: "com/mojang/blaze3d/platform/Window".into(),
                fields: [
                    ("width".to_string(), Value::Int(1280)),
                    ("height".to_string(), Value::Int(720)),
                    ("guiScaledWidth".to_string(), Value::Int(640)),
                    ("guiScaledHeight".to_string(), Value::Int(360)),
                    ("guiScale".to_string(), Value::Int(2)),
                ]
                .into(),
            });
            let level = heap.alloc_level(Level::new(true));
            let player = heap.alloc_player(Player::new(0));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/client/Minecraft".into(),
                fields: [
                    ("font".to_string(), Value::Object(Some(font))),
                    ("gui".to_string(), Value::Object(Some(gui))),
                    ("options".to_string(), Value::Object(Some(options))),
                    ("window".to_string(), Value::Object(Some(window))),
                    ("level".to_string(), Value::Object(Some(level))),
                    ("player".to_string(), Value::Object(Some(player))),
                    ("screen".to_string(), Value::NULL),
                ]
                .into(),
            });
            heap.set_singleton("net/minecraft/client/Minecraft", id);
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "getWindow" {
            if let Some(id) = heap.get_singleton("net/minecraft/client/Minecraft") {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(window) = fields.get("window") {
                        return BuiltinResult::Handled(window.clone());
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "hasShiftDown" {
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "setScreen" && args.len() >= 2 {
            let screen = args[1].clone();
            if let Some(mc) = heap.get_singleton("net/minecraft/client/Minecraft") {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(mc) {
                    fields.insert("screen".into(), screen.clone());
                    if let Some(gui) = fields.get("gui").and_then(|v| v.as_obj()) {
                        if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(gui) {
                            fields.insert("screen".into(), screen);
                        }
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "getTextureManager" {
            let tm = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/client/renderer/texture/TextureManager".into(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(tm)));
        }
    }

    // Texture loading is a host boundary: the runtime never touches the GPU.
    // The PNG bytes a mod reads are kept with the `NativeImage` and carried
    // through `DynamicTexture` to `TextureManager.register`, where the host
    // can pick them up and draw the texture itself.
    if class_name == "com/mojang/blaze3d/platform/NativeImage" {
        if method_name == "read" {
            let data = args
                .first()
                .and_then(|value| value.as_obj())
                .and_then(|id| heap.get_native::<ByteStream>(id))
                .map(|stream| stream.data.clone())
                .unwrap_or_default();
            let (width, height) = png_size(&data).unwrap_or((64, 64));
            let id = heap.alloc_native(PngImage {
                width,
                height,
                png: data,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "getWidth" || method_name == "getHeight" {
            if let Some(id) = args.first().and_then(|value| value.as_obj()) {
                if let Some(image) = heap.get_native::<PngImage>(id) {
                    return BuiltinResult::Handled(Value::Int(if method_name == "getWidth" {
                        image.width as i32
                    } else {
                        image.height as i32
                    }));
                }
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    let key = if method_name == "getWidth" {
                        "width"
                    } else {
                        "height"
                    };
                    return BuiltinResult::Handled(Value::Int(
                        fields.get(key).map(|value| value.as_int()).unwrap_or(64),
                    ));
                }
            }
            return BuiltinResult::Handled(Value::Int(64));
        }
    }

    if class_name == "net/minecraft/client/renderer/texture/DynamicTexture"
        && method_name == "<init>"
    {
        if let Some(this) = args.first().and_then(|value| value.as_obj()) {
            if let Some(image) = args.get(2).cloned() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this) {
                    fields.insert("image".into(), image);
                }
            }
        }
        return BuiltinResult::Handled(Value::NULL);
    }

    // A texture a mod asks the manager for (Legendary Tooltips measures its
    // frame sheet before slicing it). The runtime has no GPU texture, but the
    // loaded mod jars do hold the PNG, so the size it answers is the image's
    // real size. `AbstractTexture.getTexture()` carries that to `GpuTexture`.
    if class_name == "net/minecraft/client/renderer/texture/TextureManager"
        && method_name == "getTexture"
        && args.len() >= 2
    {
        let mut fields = HashMap::new();
        fields.insert("identifier".to_string(), args[1].clone());
        let id = heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/client/renderer/texture/AbstractTexture".into(),
            fields,
        });
        return BuiltinResult::Handled(Value::Object(Some(id)));
    }

    if class_name == "net/minecraft/client/renderer/texture/AbstractTexture"
        && method_name == "getTexture"
    {
        let identifier = args
            .first()
            .and_then(|value| value.as_obj())
            .and_then(|id| match heap.get(id) {
                Some(JavaObject::Instance { fields, .. }) => fields.get("identifier").cloned(),
                _ => None,
            })
            .map(|value| identifier_of(heap, &value))
            .unwrap_or_default();
        let (width, height) = heap
            .gui_texture_sizes
            .get(&identifier)
            .copied()
            .unwrap_or((0, 0));
        let mut fields = HashMap::new();
        fields.insert("width".to_string(), Value::Int(width as i32));
        fields.insert("height".to_string(), Value::Int(height as i32));
        let id = heap.alloc(JavaObject::Instance {
            class_name: "com/mojang/renderpearl/api/textures/GpuTexture".into(),
            fields,
        });
        return BuiltinResult::Handled(Value::Object(Some(id)));
    }

    if class_name == "com/mojang/renderpearl/api/textures/GpuTexture"
        && matches!(method_name, "getWidth" | "getHeight")
    {
        let field = if method_name == "getWidth" {
            "width"
        } else {
            "height"
        };
        let size = args
            .first()
            .and_then(|value| value.as_obj())
            .and_then(|id| match heap.get(id) {
                Some(JavaObject::Instance { fields, .. }) => fields.get(field),
                _ => None,
            })
            .map(|value| value.as_int())
            .unwrap_or(0);
        return BuiltinResult::Handled(Value::Int(size));
    }

    // Iceberg's custom item renderer draws a rotated item model into an
    // offscreen target, which the runtime has no GPU for. The host can draw
    // the model itself, so record the request instead.
    if class_name == "com/anthonyhilyard/iceberg/renderer/CustomItemRenderer" {
        let request = match method_name {
            "renderDetailModelIntoGUI" if args.len() >= 6 => Some((
                args[5].clone(),
                args[1].clone(),
                args[2].as_int(),
                args[3].as_int(),
                args[4].clone(),
                // `drawAndBlit` projects the model onto a 16x16 GUI square
                // (`setupOrtho(.., 16, 16)`); the caller's pose scales it.
                16,
            )),
            "renderItemIntoGUI" if args.len() >= 7 => Some((
                args[1].clone(),
                args[2].clone(),
                args[3].as_int(),
                args[4].as_int(),
                args[6].clone(),
                16,
            )),
            "renderItemModelIntoGUIWithAlpha" if args.len() >= 6 => Some((
                args[1].clone(),
                args[2].clone(),
                args[3].as_int(),
                args[4].as_int(),
                Value::NULL,
                16,
            )),
            _ => None,
        };
        if let Some((graphics, stack, x, y, rotation, size)) = request {
            let item = stack
                .as_obj()
                .and_then(|id| heap.get_native::<ItemStack>(id))
                .map(|stack| stack.item.id.clone())
                .unwrap_or_default();
            if !item.is_empty() {
                let rotation = quaternion_y_degrees(heap, &rotation);
                if let Some(id) = graphics.as_obj() {
                    push_gui_draw(
                        heap,
                        id,
                        GuiDraw::Item {
                            item,
                            x,
                            y,
                            size,
                            rotation,
                        },
                    );
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    if class_name == "net/minecraft/client/renderer/texture/TextureManager"
        && method_name == "register"
        && args.len() >= 3
    {
        let texture = identifier_of(heap, &args[1]);
        let image = args[2]
            .as_obj()
            .and_then(|id| match heap.get(id) {
                Some(JavaObject::Instance { fields, .. }) => fields.get("image"),
                _ => None,
            })
            .and_then(|value| value.as_obj())
            .and_then(|id| heap.get_native::<PngImage>(id))
            .cloned();
        if let (false, Some(image)) = (texture.is_empty(), image) {
            heap.textures.insert(texture, image);
        }
        return BuiltinResult::Handled(Value::NULL);
    }

    // The render-tick event's `DeltaTracker` carries the real frame time the
    // bridge measured; tooltip shine and model rotation read it.
    if class_name == "net/minecraft/client/DeltaTracker"
        && matches!(
            method_name,
            "getRealtimeDeltaTicks" | "getGameTimeDeltaTicks"
        )
    {
        let delta = args
            .first()
            .and_then(|value| value.as_obj())
            .and_then(|id| match heap.get(id) {
                Some(JavaObject::Instance { fields, .. }) => {
                    fields.get("delta").map(Value::as_float)
                }
                _ => None,
            })
            .unwrap_or(0.0);
        return BuiltinResult::Handled(Value::Float(delta));
    }

    if class_name == "net/minecraft/client/Options" && method_name == "guiScale" {
        let id = heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/client/OptionInstance".into(),
            fields: [("value".to_string(), Value::Int(2))].into(),
        });
        return BuiltinResult::Handled(Value::Object(Some(id)));
    }

    if class_name == "net/minecraft/client/OptionInstance" && method_name == "get" {
        return BuiltinResult::Handled(Value::Int(2));
    }

    if class_name == "net/minecraft/client/renderer/texture/AbstractTexture"
        && method_name == "close"
    {
        return BuiltinResult::Handled(Value::NULL);
    }

    // Fabric key mappings: the host owns the keyboard, the runtime keeps the
    // mapping objects so ClientTickEvents listeners can consume clicks.
    if class_name == "net/minecraft/client/KeyMapping" {
        if method_name == "<init>" {
            if let Some(this_id) = args.first().and_then(|v| v.as_obj()) {
                let mut fields = HashMap::new();
                if let Some(name) = args.get(1).cloned() {
                    fields.insert("name".into(), name);
                }
                if let Some(key) = args.get(3).cloned() {
                    fields.insert("key".into(), key);
                }
                if let Some(category) = args.get(4).cloned() {
                    fields.insert("category".into(), category);
                }
                fields.insert("clickCount".into(), Value::Int(0));
                if let Some(JavaObject::Instance { fields: f, .. }) = heap.get_mut(this_id) {
                    *f = fields;
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "consumeClick" {
            if let Some(this_id) = args.first().and_then(|v| v.as_obj()) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this_id) {
                    let count = fields.get("clickCount").map(|v| v.as_int()).unwrap_or(0);
                    if count > 0 {
                        fields.insert("clickCount".into(), Value::Int(count - 1));
                        return BuiltinResult::Handled(Value::from_bool(true));
                    }
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "isDown" {
            if let Some(this_id) = args.first().and_then(|v| v.as_obj()) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(this_id) {
                    return BuiltinResult::Handled(Value::from_bool(
                        fields.get("isDown").map(|v| v.as_bool()).unwrap_or(false),
                    ));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "setKey" && args.len() >= 2 {
            if let Some(this_id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this_id) {
                    fields.insert("key".into(), args[1].clone());
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "getKey" {
            if let Some(this_id) = args.first().and_then(|v| v.as_obj()) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(this_id) {
                    return BuiltinResult::Handled(
                        fields.get("key").cloned().unwrap_or(Value::Int(-1)),
                    );
                }
            }
            return BuiltinResult::Handled(Value::Int(-1));
        } else if method_name == "matches" && args.len() >= 3 {
            let key = args[0]
                .as_obj()
                .and_then(|id| match heap.get(id) {
                    Some(JavaObject::Instance { fields, .. }) => {
                        fields.get("key").map(|v| v.as_int())
                    }
                    _ => None,
                })
                .unwrap_or(-1);
            return BuiltinResult::Handled(Value::from_bool(key == args[1].as_int()));
        }
    }

    if class_name == "net/fabricmc/fabric/api/client/keymapping/v1/KeyMappingHelper"
        && method_name == "registerKeyMapping"
    {
        let mapping = args.first().cloned().unwrap_or(Value::NULL);
        let registry = heap
            .get_singleton("hfc:fabric_key_mappings")
            .unwrap_or_else(|| heap.alloc_list(Vec::new()));
        heap.set_singleton("hfc:fabric_key_mappings", registry);
        if let Some(JavaObject::List(items)) = heap.get_mut(registry) {
            items.push(mapping.clone());
        }
        return BuiltinResult::Handled(mapping);
    }

    if class_name == "com/mojang/blaze3d/platform/InputConstants$Key" && method_name == "getValue" {
        if let Some(this_id) = args.first().and_then(|v| v.as_obj()) {
            if let Some(JavaObject::Instance { fields, .. }) = heap.get(this_id) {
                if let Some(value) = fields.get("value") {
                    return BuiltinResult::Handled(value.clone());
                }
            }
        }
        return BuiltinResult::Handled(Value::Int(-1));
    }

    // Gui.setScreen shows a screen and keeps it where mods read it back from.
    if class_name == "net/minecraft/client/gui/Gui" {
        if method_name == "setScreen" && args.len() >= 2 {
            let screen = args[1].clone();
            if let Some(gui) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(gui) {
                    fields.insert("screen".into(), screen.clone());
                }
            }
            if let Some(mc) = heap.get_singleton("net/minecraft/client/Minecraft") {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(mc) {
                    fields.insert("screen".into(), screen);
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if (method_name == "screen" || method_name == "getScreen") && !args.is_empty() {
            if let Some(gui) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(gui) {
                    return BuiltinResult::Handled(
                        fields.get("screen").cloned().unwrap_or(Value::NULL),
                    );
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    // StringSplitter: wrapped text is returned as plain strings. The real
    // implementation builds LineComponent objects whose final fields are not
    // populated by the runtime, which turns every wrapped label into `null`.
    if class_name == "net/minecraft/client/StringSplitter" {
        if method_name == "splitLines" && args.len() >= 3 {
            let text = text_of_value(heap, &args[1]);
            let max = args[2].as_int().max(1);
            let mut lines = Vec::new();
            let mut current = String::new();
            for word in text.split_inclusive(' ') {
                if !current.is_empty() && text_width(&(current.clone() + word)) > max {
                    lines.push(heap.alloc_string(current.trim_end().to_string()));
                    current.clear();
                }
                current.push_str(word);
            }
            if !current.is_empty() {
                lines.push(heap.alloc_string(current.trim_end().to_string()));
            }
            let list = heap.alloc_list(
                lines
                    .into_iter()
                    .map(|id| Value::Object(Some(id)))
                    .collect(),
            );
            return BuiltinResult::Handled(Value::Object(Some(list)));
        }
    }

    // Font: metrics used by screens for layout. GuiGraphicsExtractor carries
    // the actual string, so measurement only needs a stable approximation.
    if class_name == "net/minecraft/client/gui/Font" {
        if method_name == "width" && args.len() >= 2 {
            return BuiltinResult::Handled(Value::Int(text_width(&text_of_value(heap, &args[1]))));
        } else if method_name == "lineHeight" {
            return BuiltinResult::Handled(Value::Int(9));
        } else if method_name == "plainSubstrByWidth" && args.len() >= 3 {
            let text = text_of_value(heap, &args[1]);
            let max = args[2].as_int();
            let mut out = String::new();
            for c in text.chars() {
                let mut next = out.clone();
                next.push(c);
                if text_width(&next) > max {
                    break;
                }
                out = next;
            }
            let sid = heap.alloc_string(out);
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if method_name == "split" && args.len() >= 3 {
            let text = text_of_value(heap, &args[1]);
            let max = args[2].as_int().max(1);
            let mut lines = Vec::new();
            let mut current = String::new();
            for word in text.split_inclusive(' ') {
                if !current.is_empty() && text_width(&(current.clone() + word)) > max {
                    lines.push(heap.alloc_string(current.trim_end().to_string()));
                    current.clear();
                }
                current.push_str(word);
            }
            if !current.is_empty() {
                lines.push(heap.alloc_string(current.trim_end().to_string()));
            }
            let values = lines
                .into_iter()
                .map(|id| Value::Object(Some(id)))
                .collect();
            let list = heap.alloc_list(values);
            return BuiltinResult::Handled(Value::Object(Some(list)));
        } else if method_name == "wordWrapHeight" && args.len() >= 3 {
            let text = text_of_value(heap, &args[1]);
            let max = args[2].as_int().max(1);
            let lines = (text_width(&text) + max - 1) / max;
            return BuiltinResult::Handled(Value::Int(lines.max(1) * 9));
        }
    }

    // A widget's click sound is a host boundary: the runtime has no audio and
    // the host does not replay mod sounds. Running the real path resolves
    // `SoundEvents.UI_BUTTON_CLICK`, which initializes Minecraft's whole sound
    // registry just to drop the sound (measured at ~30 s on a click), so the
    // widget hook is a no-op.
    if class_name == "net/minecraft/client/gui/components/AbstractWidget"
        && (method_name == "playDownSound" || method_name == "playButtonClickSound")
    {
        return BuiltinResult::Handled(Value::NULL);
    }

    // The desktop is the host's: links, folders and the clipboard become
    // requests the host performs (`FabricBridge::take_host_requests`).
    if class_name == "com/mojang/blaze3d/Blaze3D"
        && (method_name == "openUri" || method_name == "openPath")
    {
        let target = args
            .first()
            .and_then(|v| v.as_obj())
            .and_then(|id| match heap.get(id) {
                Some(JavaObject::String(s)) => Some(s.clone()),
                Some(JavaObject::Instance { fields, .. }) => fields
                    .get("string")
                    .or_else(|| fields.get("path"))
                    .and_then(|v| v.as_obj())
                    .and_then(|id| heap.get_string(id))
                    .map(str::to_string),
                _ => None,
            });
        if let Some(target) = target {
            heap.host_requests.push(if method_name == "openUri" {
                crate::bridge::HostRequest::OpenUri(target)
            } else {
                crate::bridge::HostRequest::OpenPath(target)
            });
        }
        return BuiltinResult::Handled(Value::NULL);
    }
    if class_name == "com/mojang/blaze3d/platform/ClipboardManager" {
        if method_name == "setClipboard" {
            let text = args
                .get(1)
                .map(|v| text_of_value(heap, v))
                .unwrap_or_default();
            if !text.is_empty() {
                heap.clipboard = text.clone();
                heap.host_requests
                    .push(crate::bridge::HostRequest::SetClipboard(text));
            }
            return BuiltinResult::Handled(Value::NULL);
        }
        if method_name == "getClipboard" {
            let text = heap.clipboard.clone();
            return BuiltinResult::Handled(Value::Object(Some(heap.alloc_string(text))));
        }
    }

    // Widgets draw their labels through an `ActiveTextCollector` returned by
    // `GuiGraphicsExtractor.textRenderer()`. The collector is the graphics
    // object itself here, so accepting text appends to the same display list.
    if class_name == "net/minecraft/client/gui/ActiveTextCollector" {
        // The text is the first parameter that is text (`Component`,
        // `FormattedCharSequence`, `String`); overloads put it first
        // (`accept(Component, x, y)`) or last (`accept(x, y, FormattedCharSequence)`).
        let text_index = descriptor_object_params(descriptor)
            .into_iter()
            .find(|(_, ty)| {
                matches!(
                    ty.as_str(),
                    "net/minecraft/network/chat/Component"
                        | "net/minecraft/util/FormattedCharSequence"
                        | "java/lang/String"
                )
            })
            .map(|(index, _)| index + 1);
        let text_value = text_index.and_then(|i| args.get(i));
        let text = text_value
            .map(|v| text_of_value(heap, v))
            .unwrap_or_default();
        let (x, y) = match method_name {
            "acceptScrollingWithDefaultCenter" if args.len() >= 6 => {
                let (x0, x1, y0, y1) = (
                    args[2].as_int(),
                    args[3].as_int(),
                    args[4].as_int(),
                    args[5].as_int(),
                );
                (
                    x0 + (x1 - x0 - text_width(&text)) / 2,
                    y0 + (y1 - y0 - 8) / 2,
                )
            }
            "acceptScrolling" if args.len() >= 6 => (args[2].as_int(), args[4].as_int()),
            "accept" if args.len() >= 6 => (args[2].as_int(), args[3].as_int()),
            // `accept(x, y, text)` and `accept(text, x, y)`: the two ints around
            // the text are the position.
            "accept" if args.len() >= 4 && text_index == Some(3) => {
                (args[1].as_int(), args[2].as_int())
            }
            "accept" if args.len() >= 4 => (args[2].as_int(), args[3].as_int()),
            _ => (0, 0),
        };
        if let Some(graphics) = args.first().and_then(|v| v.as_obj()) {
            push_gui_draw(
                heap,
                graphics,
                GuiDraw::Text {
                    text,
                    x,
                    y,
                    color: 0xFFFF_FFFFu32 as i32,
                },
            );
        }
        return BuiltinResult::Handled(Value::NULL);
    }

    // GuiGraphicsExtractor display-list recording. The host replays this list;
    // the runtime never touches the GPU.
    if class_name == "net/minecraft/client/gui/GuiGraphicsExtractor" {
        if method_name == "textRenderer" || method_name == "textRendererForWidget" {
            return BuiltinResult::Handled(args[0].clone());
        }
        let mut draw = None;
        let pipeline =
            descriptor.starts_with("(Lcom/mojang/renderpearl/api/pipeline/RenderPipeline;");
        if method_name == "fill" && args.len() >= 7 && pipeline && descriptor.contains("IIIII)") {
            // `fill(pipeline, x1, y1, x2, y2, color)`.
            draw = Some(GuiDraw::Fill {
                x1: args[2].as_int(),
                y1: args[3].as_int(),
                x2: args[4].as_int(),
                y2: args[5].as_int(),
                color: args[6].as_int(),
            });
        } else if method_name == "fill" && args.len() >= 6 && !pipeline {
            draw = Some(GuiDraw::Fill {
                x1: args[1].as_int(),
                y1: args[2].as_int(),
                x2: args[3].as_int(),
                y2: args[4].as_int(),
                color: args[5].as_int(),
            });
        } else if method_name == "fillGradient" && args.len() >= 7 {
            let (from, to) = (args[5].as_int(), args[6].as_int());
            draw = Some(if from == to {
                GuiDraw::Fill {
                    x1: args[1].as_int(),
                    y1: args[2].as_int(),
                    x2: args[3].as_int(),
                    y2: args[4].as_int(),
                    color: from,
                }
            } else {
                GuiDraw::Gradient {
                    x1: args[1].as_int(),
                    y1: args[2].as_int(),
                    x2: args[3].as_int(),
                    y2: args[4].as_int(),
                    color_from: from,
                    color_to: to,
                    horizontal: false,
                }
            });
        } else if (method_name == "horizontalLine" || method_name == "verticalLine")
            && args.len() >= 5
        {
            draw = Some(GuiDraw::Fill {
                x1: args[1].as_int(),
                y1: args[2].as_int(),
                x2: args[3].as_int(),
                y2: args[4].as_int(),
                color: args
                    .get(5)
                    .map(Value::as_int)
                    .unwrap_or(0xFFFF_FFFFu32 as i32),
            });
        } else if (method_name == "text" || method_name == "centeredText") && args.len() >= 6 {
            let mut text = text_of_value(heap, &args[2]);
            if text.starts_with("[Object ") || text == "null" {
                text.clear();
            }
            let mut x = args[3].as_int();
            if method_name == "centeredText" {
                x -= text_width(&text) / 2;
            }
            let mut color = args[5].as_int();
            if color == -1
                && let Some(sequence_color) = formatted_sequence_color(heap, &args[2])
            {
                color = sequence_color;
            }
            draw = Some(GuiDraw::Text {
                text,
                x,
                y: args[4].as_int(),
                color,
            });
        } else if method_name == "textWithWordWrap" && args.len() >= 7 {
            let mut text = text_of_value(heap, &args[2]);
            if text.starts_with("[Object ") || text == "null" {
                text.clear();
            }
            draw = Some(GuiDraw::Text {
                text,
                x: args[3].as_int(),
                y: args[4].as_int(),
                color: args
                    .get(6)
                    .map(Value::as_int)
                    .unwrap_or(0xFFFF_FFFFu32 as i32),
            });
        } else if (method_name == "blit" || method_name == "blitSprite") && args.len() >= 5 {
            // Pipeline-taking overloads carry the pipeline before the texture.
            let texture = identifier_argument(heap, args, 1);
            // `args[0]` is the receiver, so every Java parameter is one higher
            // than its descriptor slot.
            let direct_uv = descriptor.starts_with("(Lnet/minecraft/resources/Identifier;")
                && descriptor.ends_with("FFFF)V")
                && args.len() >= 10;
            let (x, y, width, height, u0, u1, v0, v1) = if method_name == "blitSprite" {
                (
                    args.get(3).map(Value::as_int).unwrap_or(0),
                    args.get(4).map(Value::as_int).unwrap_or(0),
                    args.get(5).map(Value::as_int).unwrap_or(16).max(1),
                    args.get(6).map(Value::as_int).unwrap_or(16).max(1),
                    0.0,
                    1.0,
                    0.0,
                    1.0,
                )
            } else if pipeline && args.len() >= 11 {
                // `blit(pipeline, texture, x, y, u, v, uWidth, vHeight,
                // texWidth, texHeight)`: `u`/`v` are texels and the destination
                // keeps the sampled size.
                let u = args[5].as_float();
                let v = args[6].as_float();
                let u_width = args[7].as_int() as f32;
                let v_height = args[8].as_int() as f32;
                let tex_width = args[9].as_int() as f32;
                let tex_height = args[10].as_int() as f32;
                let (u0, u1) = if tex_width > 0.0 {
                    (u / tex_width, (u + u_width) / tex_width)
                } else {
                    (0.0, 1.0)
                };
                let (v0, v1) = if tex_height > 0.0 {
                    (v / tex_height, (v + v_height) / tex_height)
                } else {
                    (0.0, 1.0)
                };
                (
                    args[3].as_int(),
                    args[4].as_int(),
                    u_width as i32,
                    v_height as i32,
                    u0,
                    u1,
                    v0,
                    v1,
                )
            } else if direct_uv {
                // `blit(texture, x, y, width, height, u0, u1, v0, v1)`: the
                // corners are already normalised, which is how a mod draws a
                // nine-slice frame (one corner or edge of a larger sheet).
                (
                    args[2].as_int(),
                    args[3].as_int(),
                    args[4].as_int(),
                    args[5].as_int(),
                    args[6].as_float(),
                    args[7].as_float(),
                    args[8].as_float(),
                    args[9].as_float(),
                )
            } else {
                (
                    args.get(2).map(Value::as_int).unwrap_or(0),
                    args.get(3).map(Value::as_int).unwrap_or(0),
                    args.get(4).map(Value::as_int).unwrap_or(16),
                    args.get(5).map(Value::as_int).unwrap_or(16),
                    0.0,
                    1.0,
                    0.0,
                    1.0,
                )
            };
            let finite = [u0, u1, v0, v1].iter().all(|value| value.is_finite());
            let (u0, u1, v0, v1) = if finite {
                (u0, u1, v0, v1)
            } else {
                (0.0, 1.0, 0.0, 1.0)
            };
            draw = Some(GuiDraw::Blit {
                texture,
                x,
                y,
                width: width.max(1),
                height: height.max(1),
                u0,
                u1,
                v0,
                v1,
            });
        } else if (method_name == "item" || method_name == "fakeItem") && args.len() >= 4 {
            let (stack, x, y) = match args.len() {
                6 => (2, 3, 4),
                _ => (1, 2, 3),
            };
            let item = args
                .get(stack)
                .and_then(|value| value.as_obj())
                .and_then(|id| heap.get_native::<ItemStack>(id))
                .map(|stack| stack.item.id.clone())
                .unwrap_or_default();
            if !item.is_empty() {
                draw = Some(GuiDraw::Item {
                    item,
                    x: args.get(x).map(Value::as_int).unwrap_or(0),
                    y: args.get(y).map(Value::as_int).unwrap_or(0),
                    size: 16,
                    rotation: 0.0,
                });
            }
        } else if method_name == "enableScissor" && args.len() >= 5 {
            draw = Some(GuiDraw::Scissor {
                x1: args[1].as_int(),
                y1: args[2].as_int(),
                x2: args[3].as_int(),
                y2: args[4].as_int(),
            });
        } else if method_name == "disableScissor" {
            draw = Some(GuiDraw::ScissorOff);
        } else if matches!(
            method_name,
            "itemDecorations"
                | "setTooltipForNextFrame"
                | "setComponentTooltipForNextFrame"
                | "tooltip"
                | "requestCursor"
                | "applyCursor"
                | "nextStratum"
                | "blurBeforeThisStratum"
                | "textHighlight"
                | "outline"
                | "textWithBackdrop"
                | "extractDeferredElements"
                | "setPreeditOverlay"
        ) {
            return BuiltinResult::Handled(Value::NULL);
        }
        if let Some(draw) = draw {
            if let Some(id) = args[0].as_obj() {
                push_gui_draw(heap, id, draw);
            }
            return BuiltinResult::Handled(if method_name == "containsPointInScissor" {
                Value::from_bool(true)
            } else if method_name == "guiWidth" {
                Value::Int(0)
            } else if method_name == "guiHeight" {
                Value::Int(0)
            } else {
                Value::NULL
            });
        }
        if method_name == "guiWidth" {
            if let Some(graphics) = args[0]
                .as_obj()
                .and_then(|id| heap.get_native::<GuiGraphics>(id))
            {
                return BuiltinResult::Handled(Value::Int(graphics.width));
            }
        } else if method_name == "guiHeight" {
            if let Some(graphics) = args[0]
                .as_obj()
                .and_then(|id| heap.get_native::<GuiGraphics>(id))
            {
                return BuiltinResult::Handled(Value::Int(graphics.height));
            }
        } else if method_name == "containsPointInScissor" {
            return BuiltinResult::Handled(Value::from_bool(true));
        }
    }

    // -------------------------------------------------------------
    // 15. Minecraft Types
    // -------------------------------------------------------------
    // BlockPos
    if class_name == "net/minecraft/core/BlockPos" {
        if method_name == "<init>" && args.len() >= 4 {
            let x = args[1].as_int();
            let y = args[2].as_int();
            let z = args[3].as_int();
            if let Some(this_id) = args[0].as_obj() {
                if let Some(obj) = heap.get_mut(this_id) {
                    *obj = JavaObject::Native(Box::new(BlockPos { x, y, z }));
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "getX" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(pos) = heap.get_native::<BlockPos>(id) {
                    return BuiltinResult::Handled(Value::Int(pos.x));
                }
            }
        } else if method_name == "getY" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(pos) = heap.get_native::<BlockPos>(id) {
                    return BuiltinResult::Handled(Value::Int(pos.y));
                }
            }
        } else if method_name == "getZ" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(pos) = heap.get_native::<BlockPos>(id) {
                    return BuiltinResult::Handled(Value::Int(pos.z));
                }
            }
        } else if method_name == "offset" && args.len() >= 2 {
            // `offset(Vec3i)` (a BlockPos is one) and `offset(int, int, int)`.
            if let Some(pos) = args[0]
                .as_obj()
                .and_then(|id| heap.get_native::<BlockPos>(id))
                .cloned()
            {
                let by = if args.len() >= 4 {
                    Some((args[1].as_int(), args[2].as_int(), args[3].as_int()))
                } else {
                    args[1]
                        .as_obj()
                        .and_then(|id| heap.get_native::<BlockPos>(id))
                        .map(|other| (other.x, other.y, other.z))
                };
                if let Some((dx, dy, dz)) = by {
                    let new_id = heap.alloc_block_pos(pos.x + dx, pos.y + dy, pos.z + dz);
                    return BuiltinResult::Handled(Value::Object(Some(new_id)));
                }
            }
        } else if method_name == "relative" && args.len() >= 2 {
            if let Some(id) = args[0].as_obj() {
                if let Some(pos) = heap.get_native::<BlockPos>(id).cloned() {
                    if let Some(dir) = direction_of(heap, &args[1]) {
                        let steps = args.get(2).map_or(1, |v| v.as_int());
                        let new_id = heap.alloc_block_pos(
                            pos.x + dir.x * steps,
                            pos.y + dir.y * steps,
                            pos.z + dir.z * steps,
                        );
                        return BuiltinResult::Handled(Value::Object(Some(new_id)));
                    }
                    if args.len() >= 3 {
                        let offset = args[2].as_int();
                        let new_id = heap.alloc_block_pos(pos.x + offset, pos.y, pos.z);
                        return BuiltinResult::Handled(Value::Object(Some(new_id)));
                    }
                }
            }
        } else if method_name == "distManhattan" && args.len() >= 2 {
            if let (Some(id1), Some(id2)) = (args[0].as_obj(), args[1].as_obj()) {
                if let (Some(p1), Some(p2)) = (
                    heap.get_native::<BlockPos>(id1),
                    heap.get_native::<BlockPos>(id2),
                ) {
                    let d = (p1.x - p2.x).abs() + (p1.y - p2.y).abs() + (p1.z - p2.z).abs();
                    return BuiltinResult::Handled(Value::Int(d));
                }
            }
        }
    }

    // BlockHitResult, and the `HitResult` superclass its getters are declared
    // on (`invokevirtual net/minecraft/world/phys/HitResult.getLocation`).
    if class_name == "net/minecraft/world/phys/BlockHitResult"
        || class_name == "net/minecraft/world/phys/HitResult"
    {
        if method_name == "<init>" {
            let mut found_pos = BlockPos { x: 0, y: 0, z: 0 };
            let mut found_dir = Direction::UP;
            let mut found_location = None;
            for arg in &args[1..] {
                if let Some(id) = arg.as_obj() {
                    if let Some(pos) = heap.get_native::<BlockPos>(id) {
                        found_pos = pos.clone();
                    } else if let Some(dir) = heap.get_native::<Direction>(id) {
                        found_dir = dir.clone();
                    } else if let Some(JavaObject::Instance { class_name, .. }) = heap.get(id) {
                        if class_name == "net/minecraft/world/phys/Vec3" {
                            found_location = Some(id);
                        }
                    }
                }
            }
            // Both 26.3 constructors put the inside flag after the position.
            let inside = args.get(4).is_some_and(Value::as_bool);
            if let Some(this_id) = args.get(0).and_then(|v| v.as_obj()) {
                if let Some(obj) = heap.get_mut(this_id) {
                    *obj = JavaObject::Native(Box::new(BlockHitResult {
                        pos: found_pos,
                        direction: found_dir,
                        location: found_location,
                        inside,
                    }));
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "getBlockPos" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(hit) = heap.get_native::<BlockHitResult>(id) {
                    let pid = heap.alloc_block_pos(hit.pos.x, hit.pos.y, hit.pos.z);
                    return BuiltinResult::Handled(Value::Object(Some(pid)));
                }
            }
        } else if method_name == "getDirection" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(hit) = heap.get_native::<BlockHitResult>(id) {
                    let did = heap.alloc_direction(hit.direction.clone());
                    return BuiltinResult::Handled(Value::Object(Some(did)));
                }
            }
        } else if method_name == "getLocation" && !args.is_empty() {
            // The real `Vec3` the click landed at, built by the bridge. With
            // none the call falls through: no location is invented.
            let location = args[0]
                .as_obj()
                .and_then(|id| heap.get_native::<BlockHitResult>(id))
                .and_then(|hit| hit.location);
            if let Some(location) = location {
                return BuiltinResult::Handled(Value::Object(Some(location)));
            }
        } else if method_name == "isInside" && !args.is_empty() {
            let inside = args[0]
                .as_obj()
                .and_then(|id| heap.get_native::<BlockHitResult>(id))
                .is_some_and(|hit| hit.inside);
            return BuiltinResult::Handled(Value::from_bool(inside));
        } else if method_name == "getType" && !args.is_empty() {
            // Every hit result the runtime builds is a block hit.
            let name = heap.alloc_string("BLOCK");
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/world/phys/HitResult$Type".into(),
                fields: [("name".to_string(), Value::Object(Some(name)))].into(),
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if (method_name == "withPosition" || method_name == "withDirection")
            && args.len() >= 2
        {
            let update = if let (Some(id), Some(argument)) = (args[0].as_obj(), args[1].as_obj()) {
                if let Some(hit) = heap.get_native::<BlockHitResult>(id).cloned() {
                    if method_name == "withPosition" {
                        heap.get_native::<BlockPos>(argument)
                            .cloned()
                            .map(|pos| (pos, hit.direction.clone(), hit.location, hit.inside))
                    } else {
                        heap.get_native::<Direction>(argument)
                            .cloned()
                            .map(|dir| (hit.pos.clone(), dir, hit.location, hit.inside))
                    }
                } else {
                    None
                }
            } else {
                None
            };
            if let Some((pos, dir, location, inside)) = update {
                let new_id = heap.alloc_hit_result_with(pos, dir, location, inside);
                return BuiltinResult::Handled(Value::Object(Some(new_id)));
            }
        }
    }

    // The runtime does not model a world border: the whole level is inside it.
    if class_name == "net/minecraft/world/level/border/WorldBorder"
        && matches!(method_name, "isWithinBounds" | "isInsideCloseToBorder")
    {
        return BuiltinResult::Handled(Value::from_bool(true));
    }

    // BlockState
    if class_name == "net/minecraft/world/level/block/state/BlockState" {
        if method_name == "canBeReplaced" && !args.is_empty() {
            // `BlockBehaviour.Properties.replaceable()`: air, fluids, plants.
            // The `minecraft:replaceable` tag lists exactly those.
            let id = args[0]
                .as_obj()
                .and_then(|id| heap.get_native::<BlockState>(id))
                .map(|state| state.block.id.clone());
            if let Some(id) = id {
                let replaceable = id == "minecraft:air"
                    || heap
                        .registries
                        .tag_members("BLOCK", "minecraft:replaceable")
                        .contains(&id);
                return BuiltinResult::Handled(Value::from_bool(replaceable));
            }
        }
        if method_name == "getProperties" && !args.is_empty() {
            // One property object per block-state property the runtime carries.
            let names: Vec<String> = args[0]
                .as_obj()
                .and_then(|id| heap.get_native::<BlockState>(id))
                .map(|state| state.properties.keys().cloned().collect())
                .unwrap_or_default();
            let props: Vec<Value> = names
                .into_iter()
                .map(|name| {
                    let mut fields = HashMap::new();
                    fields.insert(
                        "name".to_string(),
                        Value::Object(Some(heap.alloc_string(name))),
                    );
                    Value::Object(Some(
                        heap.alloc(JavaObject::Instance {
                            class_name:
                                "net/minecraft/world/level/block/state/properties/IntegerProperty"
                                    .into(),
                            fields,
                        }),
                    ))
                })
                .collect();
            let lid = heap.alloc_list(props);
            return BuiltinResult::Handled(Value::Object(Some(lid)));
        } else if method_name == "isAir" && !args.is_empty() {
            let air = args[0]
                .as_obj()
                .and_then(|id| heap.get_native::<BlockState>(id))
                .is_none_or(|state| state.block.id == "minecraft:air");
            return BuiltinResult::Handled(Value::from_bool(air));
        } else if method_name == "getBlock" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(state) = heap.get_native::<BlockState>(id) {
                    let bid = heap.alloc_native(state.block.clone());
                    return BuiltinResult::Handled(Value::Object(Some(bid)));
                }
            }
        } else if method_name == "getValue" && args.len() >= 2 {
            if let Some(id) = args[0].as_obj() {
                if let Some(state) = heap.get_native::<BlockState>(id) {
                    let age = state.get_age();
                    return BuiltinResult::Handled(Value::Int(age));
                }
            }
        } else if method_name == "setValue" && args.len() >= 3 {
            if let Some(id) = args[0].as_obj() {
                if let Some(state) = heap.get_native::<BlockState>(id) {
                    let mut new_state = state.clone();
                    let val = args[2].as_int().to_string();
                    new_state.properties.insert("age".into(), val);
                    let new_id = heap.alloc_block_state(new_state);
                    return BuiltinResult::Handled(Value::Object(Some(new_id)));
                }
            }
        } else if method_name == "is" && args.len() >= 2 {
            if let Some(id) = args[0].as_obj() {
                if let Some(state) = heap.get_native::<BlockState>(id) {
                    if let Some(tag_id) = args[1].as_obj() {
                        if let Some(tag) = heap.get_native::<TagKey>(tag_id) {
                            let full = if tag.id.contains(':') {
                                tag.id.clone()
                            } else {
                                format!("minecraft:{}", tag.id)
                            };
                            let members = heap.registries.tag_members("BLOCK", &full);
                            if !members.is_empty() {
                                return BuiltinResult::Handled(Value::from_bool(
                                    members.contains(&state.block.id),
                                ));
                            }
                            let matches = match tag.id.as_str() {
                                "crops" => state.block.is_crop,
                                "blacklist" | "radius_harvest_blacklist" => false,
                                "hoe_never_required" => true,
                                _ => false,
                            };
                            return BuiltinResult::Handled(Value::from_bool(matches));
                        }
                    }
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "getCloneItemStack" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(state) = heap.get_native::<BlockState>(id) {
                    let seed_id = match state.block.id.as_str() {
                        "minecraft:wheat" => "minecraft:wheat_seeds",
                        "minecraft:carrots" => "minecraft:carrot",
                        "minecraft:potatoes" => "minecraft:potato",
                        "minecraft:beetroots" => "minecraft:beetroot_seeds",
                        "minecraft:nether_wart" => "minecraft:nether_wart",
                        "minecraft:cocoa" => "minecraft:cocoa_beans",
                        _ => &state.block.id,
                    };
                    let stack_id = heap.alloc_item_stack(ItemStack::new(seed_id, 1));
                    return BuiltinResult::Handled(Value::Object(Some(stack_id)));
                }
            }
        } else if method_name == "spawnAfterBreak" {
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    // Block & CropBlock
    if class_name == "net/minecraft/world/level/block/CropBlock"
        || class_name == "net/minecraft/world/level/block/Block"
    {
        if method_name == "isMaxAge" && args.len() >= 2 {
            if let Some(id) = args[1].as_obj() {
                if let Some(state) = heap.get_native::<BlockState>(id) {
                    return BuiltinResult::Handled(Value::from_bool(state.is_max_age()));
                }
            }
        } else if method_name == "getMaxAge" {
            return BuiltinResult::Handled(Value::Int(7));
        } else if method_name == "getAgeProperty" {
            let pid = heap.alloc_native(IntegerProperty {
                name: "age".into(),
                min: 0,
                max: 7,
            });
            return BuiltinResult::Handled(Value::Object(Some(pid)));
        } else if method_name == "asItem" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(block) = heap.get_native::<Block>(id) {
                    let iid = heap.alloc_native(Item {
                        id: block.id.clone(),
                    });
                    return BuiltinResult::Handled(Value::Object(Some(iid)));
                }
            }
        } else if method_name == "getDrops" {
            let mut block_id = "minecraft:wheat".to_string();
            for arg in args {
                if let Some(id) = arg.as_obj() {
                    if let Some(s) = heap.get_native::<BlockState>(id) {
                        block_id = s.block.id.clone();
                        break;
                    }
                }
            }
            let drops = get_default_drops(&block_id);
            let drop_objs: Vec<Value> = drops
                .into_iter()
                .map(|(item_id, count)| {
                    let stk_id = heap.alloc_item_stack(ItemStack::new(item_id, count));
                    Value::Object(Some(stk_id))
                })
                .collect();
            let list_id = heap.alloc_list(drop_objs);
            return BuiltinResult::Handled(Value::Object(Some(list_id)));
        } else if method_name == "popResource" {
            let mut found_lvl: Option<usize> = None;
            let mut found_pos: Option<BlockPos> = None;
            let mut found_stk: Option<ItemStack> = None;
            for arg in args {
                if let Some(id) = arg.as_obj() {
                    if heap.get_native::<Level>(id).is_some() {
                        found_lvl = Some(id);
                    } else if let Some(p) = heap.get_native::<BlockPos>(id) {
                        found_pos = Some(p.clone());
                    } else if let Some(s) = heap.get_native::<ItemStack>(id) {
                        found_stk = Some(s.clone());
                    }
                }
            }
            if let (Some(lvl_id), Some(pos), Some(stk)) = (found_lvl, found_pos, found_stk) {
                if stk.count > 0 && !stk.is_empty() {
                    if let Some(level) = heap.get_native_mut::<Level>(lvl_id) {
                        level.dropped_stacks.push((pos, stk.item.id, stk.count));
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "dropStacks" {
            let mut found_state: Option<BlockState> = None;
            let mut found_lvl: Option<usize> = None;
            let mut found_pos: Option<BlockPos> = None;
            for arg in args {
                if let Some(id) = arg.as_obj() {
                    if let Some(s) = heap.get_native::<BlockState>(id) {
                        found_state = Some(s.clone());
                    } else if heap.get_native::<Level>(id).is_some() {
                        found_lvl = Some(id);
                    } else if let Some(p) = heap.get_native::<BlockPos>(id) {
                        found_pos = Some(p.clone());
                    }
                }
            }
            if let (Some(state), Some(lvl_id), Some(pos)) = (found_state, found_lvl, found_pos) {
                let drops = get_default_drops(&state.block.id);
                if let Some(level) = heap.get_native_mut::<Level>(lvl_id) {
                    for (item_id, count) in drops {
                        level.dropped_stacks.push((pos.clone(), item_id, count));
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    // A GPU buffer the host does not have: the runtime records draws as data,
    // so a buffer is a size and nothing more. Its bytecode checks the slice
    // against `size()`, which a stand-in buffer answers from the size the
    // caller asked for (`createBuffer`), not 0.
    if class_name.ends_with("renderpearl/api/device/GpuDevice")
        || class_name.ends_with("RenderPipeline")
    {
        // The host owns the GPU: a device makes buffer stand-ins that carry the
        // size the caller asked for, and a pipeline is an inert descriptor.
        if method_name == "createBuffer" && args.len() >= 3 {
            let size = match args.get(2) {
                Some(Value::Long(size)) => *size,
                Some(Value::Int(size)) => i64::from(*size),
                _ => match args
                    .get(2)
                    .and_then(|v| v.as_obj())
                    .and_then(|id| match heap.get(id) {
                        Some(JavaObject::Array { elements, .. }) => Some(elements.len() as i64),
                        _ => None,
                    }) {
                    Some(size) => size,
                    None => 0,
                },
            };
            let mut fields = HashMap::new();
            fields.insert("size".to_string(), Value::Long(size));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "com/mojang/renderpearl/api/buffers/GpuBuffer".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        }
        if method_name == "close" || method_name == "destroy" {
            return BuiltinResult::Handled(Value::NULL);
        }
        if method_name == "getName" {
            return BuiltinResult::Handled(args.first().cloned().unwrap_or(Value::NULL));
        }
    }

    if class_name.ends_with("renderpearl/api/buffers/GpuBuffer") {
        if method_name == "size" {
            let size = args
                .first()
                .and_then(|v| v.as_obj())
                .and_then(|id| match heap.get(id) {
                    Some(JavaObject::Instance { fields, .. }) => {
                        fields.get("size").map(Value::as_long)
                    }
                    _ => None,
                })
                .unwrap_or(0);
            return BuiltinResult::Handled(Value::Long(size));
        }
        if method_name == "usage" {
            return BuiltinResult::Handled(Value::Int(0));
        }
        if method_name == "isClosed" {
            return BuiltinResult::Handled(Value::from_bool(false));
        }
        if matches!(method_name, "close" | "destroy") {
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    // Level / ServerLevel
    if class_name == "net/minecraft/world/level/Level"
        || class_name == "net/minecraft/server/level/ServerLevel"
    {
        if method_name == "isClientSide" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(level) = heap.get_native::<Level>(id) {
                    return BuiltinResult::Handled(Value::from_bool(level.is_client));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "getBlockState" && args.len() >= 2 {
            if let (Some(id), Some(pos_id)) = (args[0].as_obj(), args[1].as_obj()) {
                if let (Some(level), Some(pos)) = (
                    heap.get_native::<Level>(id),
                    heap.get_native::<BlockPos>(pos_id),
                ) {
                    // A block a mod registered answers its own state object.
                    if let Some(own) = level.mod_states.get(&[pos.x, pos.y, pos.z]) {
                        return BuiltinResult::Handled(Value::Object(Some(*own)));
                    }
                    let state = level.get_block_state(pos);
                    let sid = heap.alloc_block_state(state);
                    return BuiltinResult::Handled(Value::Object(Some(sid)));
                }
            }
        } else if method_name == "isOutsideBuildHeight" && args.len() >= 2 {
            let y = match args[1]
                .as_obj()
                .and_then(|id| heap.get_native::<BlockPos>(id))
            {
                Some(pos) => pos.y,
                None => args[1].as_int(),
            };
            let bounds = args[0]
                .as_obj()
                .and_then(|id| heap.get_native::<Level>(id))
                .map(|level| (level.min_y, level.max_y));
            if let Some((min_y, max_y)) = bounds {
                return BuiltinResult::Handled(Value::from_bool(y < min_y || y >= max_y));
            }
        } else if method_name == "getWorldBorder" {
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/world/level/border/WorldBorder".into(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "getBlockEntity" && args.len() >= 2 {
            // The runtime's level snapshot holds block states, not block
            // entities: a mod asking for one gets null, as an empty chunk.
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "scheduleTick" && args.len() >= 4 {
            // The bridge hands the schedule to the host, which runs the
            // block's own tick when it is due.
            let pos = args[1]
                .as_obj()
                .and_then(|id| heap.get_native::<BlockPos>(id))
                .cloned();
            if let (Some(level_id), Some(pos), Some(block)) =
                (args[0].as_obj(), pos, args[2].as_obj())
            {
                let delay = args[3].as_int();
                if let Some(level) = heap.get_native_mut::<Level>(level_id) {
                    level.scheduled.push(([pos.x, pos.y, pos.z], block, delay));
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "sendParticles" {
            // 26.3's overloads (`javap -s net.minecraft.server.level.ServerLevel`):
            //   (ParticleOptions; DDD I DDD D)I
            //   (ParticleOptions; DDD I DDD D RandomizationType)I
            //   (ParticleOptions; DDD I DDD DD)I
            //   (ParticleOptions; DDD I DDD DD RandomizationType)I
            //   (ParticleOptions; ZZ DDD I DDD D)I
            //   (ParticleOptions; ZZ DDD I DDD D RandomizationType)I
            //   (ParticleOptions; ZZ DDD I DDD DD RandomizationType)I
            //   (ServerPlayer; ParticleOptions; ZZ DDD I DDD D)Z
            // Read the arguments where this descriptor puts them; an overload
            // the runtime does not know falls through instead of being read
            // with the wrong indices.
            if let Some(layout) = particle_layout(descriptor) {
                let id = args
                    .get(layout.particle)
                    .and_then(Value::as_obj)
                    .and_then(|id| heap.get_native::<ParticleType>(id))
                    .map(|particle| particle.id.clone());
                if let (Some(level_id), Some(id)) = (args[0].as_obj(), id) {
                    let call = ParticleCall {
                        id,
                        at: [
                            args[layout.x].as_double(),
                            args[layout.x + 1].as_double(),
                            args[layout.x + 2].as_double(),
                        ],
                        count: args[layout.count].as_int(),
                        spread: [
                            args[layout.spread].as_double(),
                            args[layout.spread + 1].as_double(),
                            args[layout.spread + 2].as_double(),
                        ],
                        speed: args[layout.speed].as_double(),
                    };
                    if let Some(level) = heap.get_native_mut::<Level>(level_id) {
                        level.particles.push(call);
                    }
                }
                let count = args.get(layout.count).map_or(0, Value::as_int);
                return BuiltinResult::Handled(if layout.returns_bool {
                    Value::from_bool(true)
                } else {
                    Value::Int(count)
                });
            }
        } else if method_name == "setBlock" && args.len() >= 4 {
            // `setBlock(pos, state, flags)`: the state is a native one or a
            // mod block's own object; the bridge reads which when it hands the
            // changes to the host.
            if let (Some(id), Some(pos_id), Some(st_id)) =
                (args[0].as_obj(), args[1].as_obj(), args[2].as_obj())
            {
                let pos = heap.get_native::<BlockPos>(pos_id).cloned();
                let native = heap.get_native::<BlockState>(st_id).cloned();
                if let Some(pos) = pos {
                    if let Some(level) = heap.get_native_mut::<Level>(id) {
                        let at = [pos.x, pos.y, pos.z];
                        match native {
                            Some(state) => {
                                level.mod_states.remove(&at);
                                level.blocks.insert(at, state);
                            }
                            None => {
                                level.mod_states.insert(at, st_id);
                            }
                        }
                        level.placed.push((at, st_id));
                        return BuiltinResult::Handled(Value::from_bool(true));
                    }
                }
            }
        } else if method_name == "setBlockAndUpdate" && args.len() >= 3 {
            if let (Some(id), Some(pos_id), Some(st_id)) =
                (args[0].as_obj(), args[1].as_obj(), args[2].as_obj())
            {
                let state_opt = heap.get_native::<BlockState>(st_id).cloned();
                let pos_opt = heap.get_native::<BlockPos>(pos_id).cloned();
                if let (Some(state), Some(pos)) = (state_opt, pos_opt) {
                    if let Some(level) = heap.get_native_mut::<Level>(id) {
                        level.set_block_and_update(&pos, state);
                        return BuiltinResult::Handled(Value::from_bool(true));
                    }
                }
            }
        } else if (method_name == "removeBlock" || method_name == "destroyBlock") && args.len() >= 2
        {
            // `removeBlock(pos, isMoving)` never drops; every `destroyBlock`
            // overload takes `dropBlock` as its boolean after the position.
            let drop = method_name == "destroyBlock" && args.get(2).is_some_and(Value::as_bool);
            if let (Some(id), Some(pos_id)) = (args[0].as_obj(), args[1].as_obj()) {
                if let Some(pos) = heap.get_native::<BlockPos>(pos_id).cloned() {
                    if let Some(level) = heap.get_native_mut::<Level>(id) {
                        return BuiltinResult::Handled(Value::from_bool(
                            level.destroy_block(&pos, drop),
                        ));
                    }
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        }
    }

    // `player.gameMode.destroyBlock(pos)` breaks the block in the player's level.
    if class_name == "net/minecraft/server/level/ServerPlayerGameMode"
        && method_name == "destroyBlock"
        && args.len() >= 2
    {
        let level_id = match args[0].as_obj().and_then(|id| heap.get(id)) {
            Some(JavaObject::Instance { fields, .. }) => {
                fields.get("level").and_then(|v| v.as_obj())
            }
            _ => None,
        };
        let pos = args[1]
            .as_obj()
            .and_then(|id| heap.get_native::<BlockPos>(id))
            .cloned();
        if let (Some(level_id), Some(pos)) = (level_id, pos) {
            if let Some(level) = heap.get_native_mut::<Level>(level_id) {
                // `ServerPlayerGameMode.destroyBlock` breaks the block with its
                // drops.
                return BuiltinResult::Handled(Value::from_bool(level.destroy_block(&pos, true)));
            }
        }
        return BuiltinResult::Handled(Value::from_bool(false));
    }

    if let Some(result) = registry_method(heap, class_name, method_name, args) {
        return result;
    }

    // A registry maps an object back to its identifier.
    if matches!(
        class_name,
        "net/minecraft/core/DefaultedRegistry"
            | "net/minecraft/core/Registry"
            | "net/minecraft/core/MappedRegistry"
            | "net/minecraft/core/DefaultedMappedRegistry"
    ) && method_name == "getKey"
        && args.len() >= 2
    {
        let id = args[1].as_obj().and_then(|id| {
            heap.get_native::<Block>(id)
                .map(|block| block.id.clone())
                .or_else(|| heap.get_native::<Item>(id).map(|item| item.id.clone()))
        });
        if let Some(id) = id {
            let (ns, path) = id.split_once(':').unwrap_or(("minecraft", &id));
            let ident = heap.alloc_identifier(ns, path);
            return BuiltinResult::Handled(Value::Object(Some(ident)));
        }
    }

    // Player
    if class_name == "net/minecraft/world/entity/player/Player"
        || class_name == "net/minecraft/server/level/ServerPlayer"
    {
        if method_name == "getUUID" && !args.is_empty() {
            // One object per player, so a Set or Map keyed by it matches.
            if let Some(pid) = args[0].as_obj() {
                if let Some(existing) = heap.get_native::<Player>(pid).and_then(|p| p.uuid) {
                    return BuiltinResult::Handled(Value::Object(Some(existing)));
                }
                let mut fields = HashMap::new();
                fields.insert("msb".to_string(), Value::Long(0));
                fields.insert("lsb".to_string(), Value::Long(pid as i64 + 1));
                let uuid = heap.alloc(JavaObject::Instance {
                    class_name: "java/util/UUID".into(),
                    fields,
                });
                if let Some(player) = heap.get_native_mut::<Player>(pid) {
                    player.uuid = Some(uuid);
                }
                return BuiltinResult::Handled(Value::Object(Some(uuid)));
            }
        } else if method_name == "gameMode" {
            let mut fields = HashMap::new();
            fields.insert(
                "name".to_string(),
                Value::Object(Some(heap.alloc_string("SURVIVAL"))),
            );
            fields.insert("ordinal".to_string(), Value::Int(0));
            let mode = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/world/level/GameType".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(mode)));
        } else if method_name == "getAttribute" {
            // No attribute system: a mod's modifier calls are no-ops.
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "getMainHandItem" || method_name == "getItemInHand" {
            // The acting hand answers the object the use methods got, so a
            // change the mod makes there is visible afterwards. The other
            // hand is not modelled and reads empty.
            let (acting_off, hand_stack, hand_item) = match args
                .first()
                .and_then(|v| v.as_obj())
                .and_then(|id| heap.get_native::<Player>(id))
            {
                Some(player) => (
                    player.hand_is_off,
                    player.hand_stack,
                    player.hand_item.clone(),
                ),
                None => (false, None, None),
            };
            let requested_off = if method_name == "getItemInHand" {
                args.get(1)
                    .and_then(|hand| interaction_hand_is_off(heap, hand))
                    .unwrap_or(false)
            } else {
                false
            };
            let value = match hand_stack {
                Some(stack) if acting_off == requested_off => Value::Object(Some(stack)),
                Some(_) => Value::Object(Some(heap.alloc_item_stack(ItemStack::empty()))),
                None => Value::Object(Some(
                    heap.alloc_item_stack(hand_item.unwrap_or_else(ItemStack::empty)),
                )),
            };
            return BuiltinResult::Handled(value);
        } else if method_name == "setItemInHand" && args.len() >= 3 {
            // The host cannot apply a hand-item replacement yet (stage 1): the
            // bridge refuses the use when it sees one.
            if let Some(id) = args[0].as_obj() {
                let stack = args[2].as_obj();
                let requested_off = args
                    .get(1)
                    .and_then(|hand| interaction_hand_is_off(heap, hand))
                    .unwrap_or(false);
                if let Some(player) = heap.get_native_mut::<Player>(id) {
                    player.hand_replaced = Some(stack);
                    if player.hand_is_off == requested_off {
                        player.hand_stack = stack;
                    }
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "isSpectator" || method_name == "isCrouching" {
            let crouching = method_name == "isCrouching"
                && args
                    .first()
                    .and_then(|v| v.as_obj())
                    .and_then(|id| heap.get_native::<Player>(id))
                    .is_some_and(|p| p.is_crouching);
            return BuiltinResult::Handled(Value::from_bool(crouching));
        } else if method_name == "level" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(p) = heap.get_native::<Player>(id) {
                    return BuiltinResult::Handled(Value::Object(Some(p.level_id)));
                }
            }
        } else if method_name == "position" {
            let pid = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/world/phys/Vec3".into(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(pid)));
        } else if let Some(player) = args
            .first()
            .and_then(|v| v.as_obj())
            .and_then(|id| heap.get_native::<Player>(id))
            .filter(|_| {
                matches!(
                    method_name,
                    "getYRot"
                        | "getXRot"
                        | "getDirection"
                        | "getX"
                        | "getY"
                        | "getZ"
                        | "isShiftKeyDown"
                )
            })
        {
            return BuiltinResult::Handled(match method_name {
                "getYRot" => Value::Float(player.yaw),
                "getXRot" => Value::Float(player.pitch),
                "getDirection" => player.direction.clone().unwrap_or(Value::NULL),
                "getX" => Value::Double(player.position[0]),
                "getY" => Value::Double(player.position[1]),
                "getZ" => Value::Double(player.position[2]),
                _ => Value::from_bool(player.is_crouching),
            });
        } else if method_name == "getAbilities" {
            let mut fields = HashMap::new();
            fields.insert("instabuild".into(), Value::from_bool(false));
            let aid = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/world/entity/player/Abilities".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(aid)));
        } else if method_name == "getFoodData" {
            let fid = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/world/food/FoodData".into(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(fid)));
        } else if method_name == "getRandom" {
            let rid = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/util/RandomSource".into(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(rid)));
        } else if method_name == "awardStat"
            || method_name == "playSound"
            || method_name == "sendSystemMessage"
            || method_name == "causeFoodExhaustion"
            || method_name == "giveExperiencePoints"
        {
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    // FoodData
    if class_name == "net/minecraft/world/food/FoodData" && method_name == "getFoodLevel" {
        return BuiltinResult::Handled(Value::Int(20));
    }

    // ItemStack
    // The runtime carries no block hardness/tool data and no enchantments:
    // report "no requirement" and "no enchantments" instead of dropping the
    // call, and keep the gap visible in docs/architecture/fabric-runtime.md.
    if class_name == "net/minecraft/world/level/block/state/BlockState"
        && method_name == "requiresCorrectToolForDrops"
    {
        return BuiltinResult::Handled(Value::from_bool(false));
    }
    if class_name == "net/minecraft/world/item/ItemStack"
        && matches!(
            method_name,
            "getMaxDamage" | "getDamageValue" | "isDamageableItem"
        )
    {
        let stack = args
            .first()
            .and_then(|value| value.as_obj())
            .and_then(|id| heap.get_native::<ItemStack>(id));
        return BuiltinResult::Handled(match method_name {
            "getMaxDamage" => Value::Int(stack.map_or(0, |stack| stack.max_damage)),
            "getDamageValue" => Value::Int(stack.map_or(0, |stack| stack.damage)),
            _ => Value::from_bool(stack.is_some_and(|stack| stack.max_damage > 0)),
        });
    }
    if class_name == "net/minecraft/world/item/ItemStack" && method_name == "getEnchantments" {
        let list: Vec<(String, i32)> = args
            .first()
            .and_then(|v| v.as_obj())
            .and_then(|id| heap.get_native::<ItemStack>(id))
            .map(|stack| stack.enchantments.clone())
            .unwrap_or_default();
        let mut entries = Vec::new();
        for (enchantment, level) in list {
            let (ns, path) = enchantment
                .split_once(':')
                .unwrap_or(("minecraft", &enchantment));
            let location = heap.alloc_identifier(ns, path);
            let key = resource_key(heap, "ENCHANTMENT", location);
            let mut holder = HashMap::new();
            holder.insert("key".to_string(), key);
            let holder = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/core/Holder$Reference".into(),
                fields: holder,
            });
            let mut entry = HashMap::new();
            entry.insert("key".to_string(), Value::Object(Some(holder)));
            entry.insert("value".to_string(), Value::Int(level));
            entries.push(Value::Object(Some(heap.alloc(JavaObject::Instance {
                class_name: "it/unimi/dsi/fastutil/objects/Object2IntMap$Entry".into(),
                fields: entry,
            }))));
        }
        let mut fields = HashMap::new();
        fields.insert(
            "entries".to_string(),
            Value::Object(Some(heap.alloc(JavaObject::Set(entries)))),
        );
        let id = heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/world/item/enchantment/ItemEnchantments".into(),
            fields,
        });
        return BuiltinResult::Handled(Value::Object(Some(id)));
    }
    if class_name == "net/minecraft/world/item/enchantment/ItemEnchantments"
        && matches!(method_name, "entrySet" | "keySet")
    {
        let entries = args
            .first()
            .and_then(|v| v.as_obj())
            .and_then(|id| match heap.get(id) {
                Some(JavaObject::Instance { fields, .. }) => fields.get("entries").cloned(),
                _ => None,
            })
            .unwrap_or_else(|| Value::Object(Some(heap.alloc(JavaObject::Set(Vec::new())))));
        return BuiltinResult::Handled(entries);
    }
    if class_name == "it/unimi/dsi/fastutil/objects/Object2IntMap$Entry"
        && matches!(method_name, "getKey" | "getIntValue" | "getValue")
    {
        let field = if method_name == "getKey" {
            "key"
        } else {
            "value"
        };
        if let Some(JavaObject::Instance { fields, .. }) = args
            .first()
            .and_then(|v| v.as_obj())
            .and_then(|id| heap.get(id))
        {
            if let Some(v) = fields.get(field) {
                return BuiltinResult::Handled(v.clone());
            }
        }
    }
    if matches!(
        class_name,
        "net/minecraft/core/Holder" | "net/minecraft/core/Holder$Reference"
    ) && method_name == "is"
        && args.len() >= 2
    {
        let own = args[0].as_obj().and_then(|id| match heap.get(id) {
            Some(JavaObject::Instance { fields, .. }) => fields.get("key").cloned(),
            _ => None,
        });
        if let Some(own) = own {
            let same = match (
                resource_key_text(heap, &own),
                resource_key_text(heap, &args[1]),
            ) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            };
            return BuiltinResult::Handled(Value::from_bool(same));
        }
    }
    if class_name == "net/minecraft/resources/ResourceKey" {
        match method_name {
            "create" if args.len() >= 2 => {
                let kind = registry_kind(heap, &args[0]).unwrap_or_default();
                if let Some(location) = args[1].as_obj() {
                    return BuiltinResult::Handled(resource_key(heap, &kind, location));
                }
            }
            "location" | "identifier" if !args.is_empty() => {
                if let Some(JavaObject::Instance { fields, .. }) =
                    args[0].as_obj().and_then(|id| heap.get(id))
                {
                    if let Some(v) = fields.get("location") {
                        return BuiltinResult::Handled(v.clone());
                    }
                }
            }
            _ => {}
        }
    }
    // A `BlockState` the runtime models carries `propertyKeys`/`propertyValues`;
    // `StateHolder.getValue(property)` reads them so a mod's `tick` or
    // `getStateForPlacement` gets the value the state was built with.
    if matches!(
        class_name,
        "net/minecraft/world/level/block/state/BlockState"
            | "net/minecraft/world/level/block/state/StateHolder"
            | "net/minecraft/world/level/block/state/BlockBehaviour$BlockStateBase"
    ) && method_name == "getValue"
        && args.len() == 2
    {
        let (keys, values) = match args[0].as_obj().and_then(|id| heap.get(id)) {
            Some(JavaObject::Instance { fields, .. }) => (
                fields.get("propertyKeys").and_then(Value::as_obj),
                fields.get("propertyValues").and_then(Value::as_obj),
            ),
            _ => (None, None),
        };
        let (Some(keys), Some(values)) = (keys, values) else {
            return BuiltinResult::NotHandled;
        };
        let keys = match heap.get(keys) {
            Some(JavaObject::Array { elements, .. }) => elements.clone(),
            _ => return BuiltinResult::NotHandled,
        };
        let values = match heap.get(values) {
            Some(JavaObject::Array { elements, .. }) => elements.clone(),
            _ => return BuiltinResult::NotHandled,
        };
        if let Some(position) = keys.iter().position(|key| *key == args[1]) {
            return BuiltinResult::Handled(values.get(position).cloned().unwrap_or(Value::NULL));
        }
        return BuiltinResult::NotHandled;
    }
    if class_name == "net/minecraft/world/item/ItemStack" {
        if method_name == "getItem" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(stack) = heap.get_native::<ItemStack>(id) {
                    let iid = heap.alloc_native(stack.item.clone());
                    return BuiltinResult::Handled(Value::Object(Some(iid)));
                }
            }
        } else if method_name == "isEmpty" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(stack) = heap.get_native::<ItemStack>(id) {
                    return BuiltinResult::Handled(Value::from_bool(stack.is_empty()));
                }
            }
            return BuiltinResult::Handled(Value::from_bool(true));
        } else if method_name == "getCount" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(stack) = heap.get_native::<ItemStack>(id) {
                    return BuiltinResult::Handled(Value::Int(stack.count));
                }
            }
            return BuiltinResult::Handled(Value::Int(0));
        } else if method_name == "setCount" && args.len() >= 2 {
            let new_count = args[1].as_int();
            if let Some(id) = args[0].as_obj() {
                if let Some(stack) = heap.get_native_mut::<ItemStack>(id) {
                    stack.count = new_count;
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if (method_name == "shrink" || method_name == "grow") && args.len() >= 2 {
            let amount = args[1].as_int();
            if let Some(id) = args[0].as_obj() {
                if let Some(stack) = heap.get_native_mut::<ItemStack>(id) {
                    stack.count = if method_name == "shrink" {
                        (stack.count - amount).max(0)
                    } else {
                        stack.count.saturating_add(amount)
                    };
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "consume" && args.len() >= 3 {
            // `ItemStack.consume(int, LivingEntity)`: creative mode is not
            // modelled, so it shrinks like the survival path.
            let amount = args[1].as_int().max(0);
            if let Some(id) = args[0].as_obj() {
                if let Some(stack) = heap.get_native_mut::<ItemStack>(id) {
                    stack.count = (stack.count - amount).max(0);
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "split" && args.len() >= 2 {
            let amount = args[1].as_int().max(0);
            if let Some(id) = args[0].as_obj() {
                let split = heap.get_native_mut::<ItemStack>(id).map(|stack| {
                    let take = amount.min(stack.count);
                    let mut split = stack.clone();
                    split.count = take;
                    stack.count -= take;
                    split
                });
                if let Some(split) = split {
                    let split_id = heap.alloc_item_stack(split);
                    return BuiltinResult::Handled(Value::Object(Some(split_id)));
                }
            }
            return BuiltinResult::Handled(Value::NULL);
        } else if method_name == "is" && args.len() >= 2 {
            if let Some(id) = args[0].as_obj() {
                if let Some(stack) = heap.get_native::<ItemStack>(id) {
                    if let Some(tag_id) = args[1].as_obj() {
                        if let Some(tag) = heap.get_native::<TagKey>(tag_id) {
                            if tag.id.contains("hoes") {
                                return BuiltinResult::Handled(Value::from_bool(stack.is_hoe()));
                            }
                        }
                    }
                }
            }
            return BuiltinResult::Handled(Value::from_bool(false));
        } else if method_name == "hurtAndBreak" {
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    // Item
    if class_name == "net/minecraft/world/item/Item" {
        return BuiltinResult::Handled(Value::NULL);
    }

    // Stats
    if class_name == "net/minecraft/stats/StatType" && method_name == "get" {
        let sid = heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/stats/Stat".into(),
            fields: HashMap::new(),
        });
        return BuiltinResult::Handled(Value::Object(Some(sid)));
    }

    // Fabric ResourceManagerHelper
    if class_name == "net/fabricmc/fabric/api/resource/ResourceManagerHelper" {
        if method_name == "get" {
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/fabricmc/fabric/api/resource/ResourceManagerHelper".into(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "registerReloadListener" {
            return BuiltinResult::Handled(Value::NULL);
        }
    }

    // Component & MutableComponent
    if class_name == "net/minecraft/network/chat/Component"
        || class_name == "net/minecraft/network/chat/MutableComponent"
    {
        if method_name == "literal" {
            let text = args
                .first()
                .and_then(|v| v.as_obj())
                .and_then(|id| heap.get_string(id))
                .unwrap_or("")
                .to_string();
            let str_id = heap.alloc_string(text);
            let mut fields = HashMap::new();
            fields.insert("text".into(), Value::Object(Some(str_id)));
            let sibs_id = heap.alloc_list(Vec::new());
            fields.insert("siblings".into(), Value::Object(Some(sibs_id)));
            fields.insert("color".into(), Value::Int(0xFFFFFFFF_u32 as i32));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/network/chat/MutableComponent".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "translatable"
            || method_name == "translatableEscape"
            || method_name == "translatableWithFallback"
        {
            let key = args
                .first()
                .and_then(|v| v.as_obj())
                .and_then(|id| heap.get_string(id))
                .unwrap_or("")
                .to_string();

            let (fallback, arg_start) =
                if method_name == "translatableWithFallback" && args.len() >= 2 {
                    let fb = args[1]
                        .as_obj()
                        .and_then(|id| heap.get_string(id))
                        .map(str::to_string);
                    (fb, 2)
                } else {
                    (None, 1)
                };

            let mut format_args = Vec::new();
            if args.len() > arg_start {
                if let Some(arr_id) = args[arg_start].as_obj() {
                    if let Some(JavaObject::Array { elements, .. }) = heap.get(arr_id) {
                        for elem in elements {
                            format_args.push(value_to_string(heap, elem));
                        }
                    } else {
                        for arg in &args[arg_start..] {
                            format_args.push(value_to_string(heap, arg));
                        }
                    }
                } else {
                    for arg in &args[arg_start..] {
                        format_args.push(value_to_string(heap, arg));
                    }
                }
            }

            let template = heap
                .get_translation(&key)
                .map(str::to_string)
                .or(fallback)
                .unwrap_or_else(|| key.clone());

            let translated = format_translation(&template, &format_args);
            let str_id = heap.alloc_string(translated);
            let key_id = heap.alloc_string(key);
            let mut fields = HashMap::new();
            fields.insert("text".into(), Value::Object(Some(str_id)));
            // The key it was built from, so a caller that can translate it
            // itself (a container's title) keeps it.
            fields.insert("key".into(), Value::Object(Some(key_id)));
            let sibs_id = heap.alloc_list(Vec::new());
            fields.insert("siblings".into(), Value::Object(Some(sibs_id)));
            fields.insert("color".into(), Value::Int(0xFFFFFFFF_u32 as i32));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/network/chat/MutableComponent".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "getVisualOrderText" {
            let text = args
                .first()
                .map(|v| text_of_value(heap, v))
                .unwrap_or_default();
            let sid = heap.alloc_string(text);
            let mut fields = HashMap::new();
            fields.insert("text".into(), Value::Object(Some(sid)));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/util/FormattedCharSequence".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "empty" {
            let str_id = heap.alloc_string("");
            let mut fields = HashMap::new();
            fields.insert("text".into(), Value::Object(Some(str_id)));
            let sibs_id = heap.alloc_list(Vec::new());
            fields.insert("siblings".into(), Value::Object(Some(sibs_id)));
            fields.insert("color".into(), Value::Int(0xFFFFFFFF_u32 as i32));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/network/chat/MutableComponent".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "nullToEmpty" {
            let str_arg = args
                .first()
                .and_then(|v| v.as_obj())
                .and_then(|id| heap.get_string(id))
                .unwrap_or("")
                .to_string();
            let str_id = heap.alloc_string(str_arg);
            let mut fields = HashMap::new();
            fields.insert("text".into(), Value::Object(Some(str_id)));
            let sibs_id = heap.alloc_list(Vec::new());
            fields.insert("siblings".into(), Value::Object(Some(sibs_id)));
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/network/chat/MutableComponent".into(),
                fields,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "append" && args.len() >= 2 {
            let this_id = args[0].as_obj();
            let other = args[1].clone();
            if let Some(this_id) = this_id {
                let other_text = if let Some(oid) = other.as_obj() {
                    if let Some(s) = heap.get_string(oid) {
                        Some(s.to_string())
                    } else if let Some(JavaObject::Instance { fields, .. }) = heap.get(oid) {
                        fields
                            .get("text")
                            .and_then(|v| v.as_obj())
                            .and_then(|sid| heap.get_string(sid))
                            .map(str::to_string)
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(txt) = other_text {
                    let cur_str =
                        if let Some(JavaObject::Instance { fields, .. }) = heap.get(this_id) {
                            fields
                                .get("text")
                                .and_then(|v| v.as_obj())
                                .and_then(|sid| heap.get_string(sid))
                                .unwrap_or("")
                                .to_string()
                        } else {
                            String::new()
                        };
                    let combined = format!("{cur_str}{txt}");
                    let new_str_id = heap.alloc_string(combined);
                    if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this_id) {
                        fields.insert("text".into(), Value::Object(Some(new_str_id)));
                    }
                }
            }
            return BuiltinResult::Handled(args[0].clone());
        } else if (method_name == "withStyle" || method_name == "setStyle") && !args.is_empty() {
            if args.len() >= 2 {
                if let Some(style_id) = args[1].as_obj() {
                    let extracted_color =
                        if let Some(JavaObject::Instance { fields, .. }) = heap.get(style_id) {
                            fields.get("color").map(|v| v.as_int())
                        } else {
                            None
                        };
                    if let Some(c) = extracted_color {
                        if let Some(this_id) = args[0].as_obj() {
                            if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this_id)
                            {
                                fields.insert("color".into(), Value::Int(c));
                            }
                        }
                    }
                }
            }
            return BuiltinResult::Handled(args[0].clone());
        } else if method_name == "getString" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if heap.get_string(id).is_some() {
                    return BuiltinResult::Handled(Value::Object(Some(id)));
                } else if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(Value::Object(Some(sid))) = fields.get("text") {
                        return BuiltinResult::Handled(Value::Object(Some(*sid)));
                    }
                }
            }
            let sid = heap.alloc_string("");
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if (method_name == "copy" || method_name == "plainCopy") && !args.is_empty() {
            return BuiltinResult::Handled(args[0].clone());
        } else if method_name == "getSiblings" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(sibs) = fields.get("siblings") {
                        return BuiltinResult::Handled(sibs.clone());
                    }
                }
            }
            let list_id = heap.alloc_list(Vec::new());
            return BuiltinResult::Handled(Value::Object(Some(list_id)));
        }
    }

    // TextColor
    if class_name == "net/minecraft/network/chat/TextColor" {
        if method_name == "fromRgb" && !args.is_empty() {
            let c = args[0].as_int();
            let mut f = HashMap::new();
            f.insert("value".into(), Value::Int(c));
            f.insert("color".into(), Value::Int(c));
            let id = heap.alloc(JavaObject::Instance {
                class_name: class_name.into(),
                fields: f,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "fromLegacyFormat" && !args.is_empty() {
            let color = args[0]
                .as_obj()
                .and_then(|id| {
                    if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                        fields.get("color").map(|v| v.as_int())
                    } else {
                        None
                    }
                })
                .unwrap_or(0xFFFFFF);
            let mut f = HashMap::new();
            f.insert("value".into(), Value::Int(color));
            f.insert("color".into(), Value::Int(color));
            let id = heap.alloc(JavaObject::Instance {
                class_name: class_name.into(),
                fields: f,
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "getValue" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(v) = fields.get("value") {
                        return BuiltinResult::Handled(v.clone());
                    }
                }
            }
            return BuiltinResult::Handled(Value::Int(0xFFFFFF));
        }
    }

    // ChatFormatting
    if class_name == "net/minecraft/ChatFormatting" {
        if method_name == "getColor" && !args.is_empty() {
            if let Some(id) = args[0].as_obj() {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                    if let Some(c) = fields.get("color") {
                        return BuiltinResult::Handled(c.clone());
                    }
                }
            }
            return BuiltinResult::Handled(Value::Int(0xFFFFFF));
        }
    }

    // net/minecraft/locale/Language
    if class_name == "net/minecraft/locale/Language" {
        if method_name == "getInstance" {
            let id = heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/locale/Language".into(),
                fields: HashMap::new(),
            });
            return BuiltinResult::Handled(Value::Object(Some(id)));
        } else if method_name == "has" {
            let key = args
                .get(1)
                .or_else(|| args.first())
                .and_then(|v| v.as_obj())
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let has = heap.has_translation(key);
            return BuiltinResult::Handled(Value::from_bool(has));
        } else if method_name == "getOrDefault" {
            let key = args
                .get(1)
                .or_else(|| args.first())
                .and_then(|v| v.as_obj())
                .and_then(|id| heap.get_string(id))
                .unwrap_or("")
                .to_string();
            let fallback = args
                .get(2)
                .and_then(|v| v.as_obj())
                .and_then(|id| heap.get_string(id))
                .map(str::to_string)
                .unwrap_or_else(|| key.clone());
            let val = heap.get_translation(&key).unwrap_or(&fallback).to_string();
            let sid = heap.alloc_string(val);
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if method_name == "getVisualOrder" {
            let text = args
                .get(1)
                .filter(|v| !v.is_null())
                .map(|v| text_of_value(heap, v))
                .unwrap_or_default();
            let color = args.get(1).and_then(|value| {
                value.as_obj().and_then(|id| match heap.get(id) {
                    Some(JavaObject::Instance { fields, .. }) => {
                        fields.get("color").map(Value::as_int)
                    }
                    _ => None,
                })
            });
            return BuiltinResult::Handled(alloc_formatted_char_sequence_colored(
                heap, text, color,
            ));
        }
    }

    // net/minecraft/client/resources/language/I18n
    if class_name == "net/minecraft/client/resources/language/I18n" {
        if method_name == "get" {
            let key = args
                .first()
                .and_then(|v| v.as_obj())
                .and_then(|id| heap.get_string(id))
                .unwrap_or("")
                .to_string();
            let mut format_args = Vec::new();
            if args.len() > 1 {
                if let Some(arr_id) = args[1].as_obj() {
                    if let Some(JavaObject::Array { elements, .. }) = heap.get(arr_id) {
                        for elem in elements {
                            format_args.push(value_to_string(heap, elem));
                        }
                    } else {
                        for arg in &args[1..] {
                            format_args.push(value_to_string(heap, arg));
                        }
                    }
                } else {
                    for arg in &args[1..] {
                        format_args.push(value_to_string(heap, arg));
                    }
                }
            }
            let template = heap.get_translation(&key).unwrap_or(&key).to_string();
            let val = format_translation(&template, &format_args);
            let sid = heap.alloc_string(val);
            return BuiltinResult::Handled(Value::Object(Some(sid)));
        } else if method_name == "has" {
            let key = args
                .first()
                .and_then(|v| v.as_obj())
                .and_then(|id| heap.get_string(id))
                .unwrap_or("");
            let has = heap.has_translation(key);
            return BuiltinResult::Handled(Value::from_bool(has));
        }
    }

    BuiltinResult::NotHandled
}

/// A `Direction` value: the runtime's native one, or the real enum constant
/// (an instance holding the constant's name) Minecraft's own `Direction`
/// bytecode makes.
pub(crate) fn direction_of(heap: &Heap, value: &Value) -> Option<Direction> {
    let id = value.as_obj()?;
    if let Some(direction) = heap.get_native::<Direction>(id) {
        return Some(direction.clone());
    }
    let JavaObject::Instance { class_name, fields } = heap.get(id)? else {
        return None;
    };
    if class_name != "net/minecraft/core/Direction" {
        return None;
    }
    let name = heap.get_string(fields.get("name")?.as_obj()?)?;
    Some(match name {
        "down" | "DOWN" => Direction::DOWN,
        "up" | "UP" => Direction::UP,
        "north" | "NORTH" => Direction::NORTH,
        "south" | "SOUTH" => Direction::SOUTH,
        "west" | "WEST" => Direction::WEST,
        "east" | "EAST" => Direction::EAST,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{ParticleLayout, particle_layout};

    #[test]
    fn every_26_3_send_particles_overload_keeps_its_argument_positions() {
        let plain = ParticleLayout {
            particle: 1,
            x: 2,
            count: 5,
            spread: 6,
            speed: 9,
            returns_bool: false,
        };
        let flagged = ParticleLayout {
            particle: 1,
            x: 4,
            count: 7,
            spread: 8,
            speed: 11,
            returns_bool: false,
        };
        let player = ParticleLayout {
            particle: 2,
            x: 5,
            count: 8,
            spread: 9,
            speed: 12,
            returns_bool: true,
        };
        // The descriptors `javap -s net.minecraft.server.level.ServerLevel`
        // prints for sendParticles.
        for descriptor in [
            "(Lnet/minecraft/core/particles/ParticleOptions;DDDIDDDD)I",
            "(Lnet/minecraft/core/particles/ParticleOptions;DDDIDDDDDD)I",
            "(Lnet/minecraft/core/particles/ParticleOptions;DDDIDDDDLnet/minecraft/network/protocol/game/ClientboundLevelParticlesPacket$RandomizationType;)I",
            "(Lnet/minecraft/core/particles/ParticleOptions;DDDIDDDDDDLnet/minecraft/network/protocol/game/ClientboundLevelParticlesPacket$RandomizationType;)I",
        ] {
            assert_eq!(particle_layout(descriptor), Some(plain), "{descriptor}");
        }
        for descriptor in [
            "(Lnet/minecraft/core/particles/ParticleOptions;ZZDDDIDDDD)I",
            "(Lnet/minecraft/core/particles/ParticleOptions;ZZDDDIDDDDLnet/minecraft/network/protocol/game/ClientboundLevelParticlesPacket$RandomizationType;)I",
            "(Lnet/minecraft/core/particles/ParticleOptions;ZZDDDIDDDDDDLnet/minecraft/network/protocol/game/ClientboundLevelParticlesPacket$RandomizationType;)I",
        ] {
            assert_eq!(particle_layout(descriptor), Some(flagged), "{descriptor}");
        }
        assert_eq!(
            particle_layout(
                "(Lnet/minecraft/server/level/ServerPlayer;Lnet/minecraft/core/particles/ParticleOptions;ZZDDDIDDDD)Z"
            ),
            Some(player)
        );
        // An overload the runtime does not know is not guessed at.
        assert_eq!(
            particle_layout("(Lnet/minecraft/core/particles/ParticleOptions;I)I"),
            None
        );
        assert_eq!(particle_layout("()V"), None);
    }
}
