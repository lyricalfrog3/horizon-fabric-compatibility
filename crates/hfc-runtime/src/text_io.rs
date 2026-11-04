//! Text on top of byte streams: charsets, readers, writers and `String` byte
//! conversion. The JDK's own readers/writers go through `StreamDecoder` and
//! native channel code the runtime does not have, so these are native objects.

use crate::builtins::BuiltinResult;
use crate::fs_builtins::FileSink;
use crate::heap::Heap;
use crate::minecraft::ByteStream;
use crate::value::{JavaObject, NativeObject, Value};
use std::any::Any;
use std::collections::HashMap;

/// A reader over decoded characters.
#[derive(Debug)]
pub struct CharReader {
    pub chars: Vec<char>,
    pub pos: usize,
}

/// A writer that accumulates text (`StringWriter`).
#[derive(Debug, Default)]
pub struct StringSink {
    pub text: String,
}

macro_rules! native {
    ($ty:ty, $name:expr) => {
        impl NativeObject for $ty {
            fn class_name(&self) -> &str {
                $name
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn as_any_mut(&mut self) -> &mut dyn Any {
                self
            }
        }
    };
}
native!(CharReader, "java/io/Reader");
native!(StringSink, "java/io/StringWriter");

fn string_of(heap: &Heap, value: &Value) -> Option<String> {
    heap.get_string(value.as_obj()?).map(str::to_string)
}

fn alloc_str(heap: &mut Heap, s: impl Into<String>) -> Value {
    Value::Object(Some(heap.alloc_string(s)))
}

fn byte_array(heap: &mut Heap, bytes: &[u8]) -> Value {
    let id = heap.alloc(JavaObject::Array {
        elem_type: "byte".into(),
        elements: bytes.iter().map(|b| Value::Int(*b as i8 as i32)).collect(),
    });
    Value::Object(Some(id))
}

fn array_values(heap: &Heap, value: &Value) -> Option<Vec<Value>> {
    match heap.get(value.as_obj()?)? {
        JavaObject::Array { elements, .. } => Some(elements.clone()),
        _ => None,
    }
}

fn charset_value(heap: &mut Heap, name: &str) -> Value {
    let mut fields = HashMap::new();
    let name = alloc_str(heap, name);
    fields.insert("name".to_string(), name);
    let id = heap.alloc(JavaObject::Instance {
        class_name: "java/nio/charset/Charset".into(),
        fields,
    });
    Value::Object(Some(id))
}

/// Replace the half-built `this` of a constructor with a native object.
fn become_native<T: NativeObject + 'static>(heap: &mut Heap, this: &Value, native: T) -> bool {
    let Some(id) = this.as_obj() else {
        return false;
    };
    match heap.get_mut(id) {
        Some(slot) => {
            *slot = JavaObject::Native(Box::new(native));
            true
        }
        None => false,
    }
}

fn decode(bytes: &[u8]) -> Vec<char> {
    String::from_utf8_lossy(bytes).chars().collect()
}

pub fn text_method(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    let handled = |v: Value| Some(BuiltinResult::Handled(v));

    match (class_name, method_name) {
        ("java/nio/charset/Charset", "forName") => {
            let name = args
                .first()
                .and_then(|v| string_of(heap, v))
                .unwrap_or_else(|| "UTF-8".into());
            return handled(charset_value(heap, &name.to_uppercase()));
        }
        ("java/nio/charset/Charset", "defaultCharset") => {
            return handled(charset_value(heap, "UTF-8"));
        }
        ("java/nio/charset/Charset", "name" | "displayName" | "toString") => {
            let id = args.first()?.as_obj()?;
            if let Some(JavaObject::Instance { fields, .. }) = heap.get(id) {
                return handled(fields.get("name")?.clone());
            }
        }
        ("java/lang/String", "getBytes") => {
            let text = string_of(heap, args.first()?)?;
            return handled(byte_array(heap, text.as_bytes()));
        }
        ("java/lang/String", "getChars") if args.len() == 5 => {
            // getChars(srcBegin, srcEnd, dst, dstBegin)
            let text = string_of(heap, &args[0])?;
            let (from, to) = (
                args[1].as_int().max(0) as usize,
                args[2].as_int().max(0) as usize,
            );
            let at = args[4].as_int().max(0) as usize;
            let chars: Vec<char> = text
                .chars()
                .skip(from)
                .take(to.saturating_sub(from))
                .collect();
            if let Some(JavaObject::Array { elements, .. }) =
                args[3].as_obj().and_then(|id| heap.get_mut(id))
            {
                for (i, c) in chars.iter().enumerate() {
                    if let Some(slot) = elements.get_mut(at + i) {
                        *slot = Value::Int(*c as i32);
                    }
                }
            }
            return handled(Value::NULL);
        }
        ("java/lang/String", "indexOf" | "lastIndexOf") if args.len() >= 2 => {
            let text: Vec<char> = string_of(heap, &args[0])?.chars().collect();
            let needle: Vec<char> = match &args[1] {
                Value::Int(c) => vec![char::from_u32(*c as u32)?],
                other => string_of(heap, other)?.chars().collect(),
            };
            let last = method_name == "lastIndexOf";
            let found = if needle.is_empty() || needle.len() > text.len() {
                if needle.is_empty() { Some(0) } else { None }
            } else if last {
                let from = args.get(2).map_or(text.len() - needle.len(), |v| {
                    (v.as_int().max(0) as usize).min(text.len() - needle.len())
                });
                (0..=from)
                    .rev()
                    .find(|&i| text[i..i + needle.len()] == needle[..])
            } else {
                let from = args.get(2).map_or(0, |v| v.as_int().max(0) as usize);
                (from..=text.len() - needle.len())
                    .find(|&i| text[i..i + needle.len()] == needle[..])
            };
            return handled(Value::Int(found.map_or(-1, |i| i as i32)));
        }
        ("java/lang/String", "replace") if args.len() == 3 => {
            let text = string_of(heap, &args[0])?;
            let as_text = |v: &Value, heap: &Heap| match v {
                Value::Int(c) => char::from_u32(*c as u32).map(String::from),
                other => string_of(heap, other),
            };
            let (from, to) = (as_text(&args[1], heap)?, as_text(&args[2], heap)?);
            return handled(alloc_str(heap, text.replace(&from, &to)));
        }
        ("java/lang/String", "concat") if args.len() == 2 => {
            let text = string_of(heap, &args[0])? + &string_of(heap, &args[1])?;
            return handled(alloc_str(heap, text));
        }
        ("java/lang/String", "isBlank") => {
            let text = string_of(heap, args.first()?)?;
            return handled(Value::from_bool(text.trim().is_empty()));
        }
        ("java/lang/String", "equalsIgnoreCase") if args.len() == 2 => {
            let a = string_of(heap, &args[0])?;
            let equal =
                string_of(heap, &args[1]).is_some_and(|b| a.to_lowercase() == b.to_lowercase());
            return handled(Value::from_bool(equal));
        }
        ("java/lang/String", "toCharArray") => {
            let text = string_of(heap, args.first()?)?;
            let id = heap.alloc(JavaObject::Array {
                elem_type: "char".into(),
                elements: text.chars().map(|c| Value::Int(c as i32)).collect(),
            });
            return handled(Value::Object(Some(id)));
        }
        ("java/lang/String", "<init>") if args.len() >= 2 => {
            // new String(byte[]|char[] [, ...])
            let values = array_values(heap, &args[1])?;
            let is_chars = matches!(
                heap.get(args[1].as_obj()?),
                Some(JavaObject::Array { elem_type, .. }) if elem_type == "char"
            );
            let (off, len) = match (args.get(2), args.get(3)) {
                (Some(Value::Int(o)), Some(Value::Int(l))) => (*o as usize, *l as usize),
                _ => (0, values.len()),
            };
            let slice = values.get(off..off + len)?;
            let text = if is_chars {
                slice
                    .iter()
                    .filter_map(|v| char::from_u32(v.as_int() as u32))
                    .collect::<String>()
            } else {
                let bytes: Vec<u8> = slice.iter().map(|v| v.as_int() as u8).collect();
                String::from_utf8_lossy(&bytes).to_string()
            };
            let id = args[0].as_obj()?;
            *heap.get_mut(id)? = JavaObject::String(text);
            return handled(Value::NULL);
        }
        ("java/io/InputStreamReader" | "java/io/FileReader", "<init>") => {
            let stream = args.get(1)?.as_obj()?;
            let data = heap.get_native::<ByteStream>(stream)?.data.clone();
            become_native(
                heap,
                &args[0],
                CharReader {
                    chars: decode(&data),
                    pos: 0,
                },
            );
            return handled(Value::NULL);
        }
        ("java/io/BufferedReader", "<init>") => {
            // Wrapping a reader (or the stream a path opened) shares its text.
            let inner = args.get(1)?.as_obj()?;
            let chars = match (
                heap.get_native::<CharReader>(inner),
                heap.get_native::<ByteStream>(inner),
            ) {
                (Some(reader), _) => reader.chars[reader.pos..].to_vec(),
                (None, Some(stream)) => decode(&stream.data),
                _ => return None,
            };
            become_native(heap, &args[0], CharReader { chars, pos: 0 });
            return handled(Value::NULL);
        }
        ("java/io/StringReader", "<init>") => {
            let text = string_of(heap, args.get(1)?)?;
            become_native(
                heap,
                &args[0],
                CharReader {
                    chars: text.chars().collect(),
                    pos: 0,
                },
            );
            return handled(Value::NULL);
        }
        ("java/io/StringWriter", "<init>") => {
            become_native(heap, &args[0], StringSink::default());
            return handled(Value::NULL);
        }
        (
            "java/io/OutputStreamWriter"
            | "java/io/BufferedWriter"
            | "java/io/PrintWriter"
            | "java/io/BufferedOutputStream"
            | "java/io/PrintStream",
            "<init>",
        ) => {
            let inner = args.get(1)?.as_obj()?;
            if let Some(sink) = heap.get_native::<FileSink>(inner) {
                let path = sink.path.clone();
                become_native(heap, &args[0], FileSink { path });
                return handled(Value::NULL);
            }
            if heap.get_native::<StringSink>(inner).is_some() {
                // Writing through to the same buffer would need sharing; the
                // wrapper writes into a fresh sink the caller reads back.
                return None;
            }
        }
        _ => {}
    }

    // Methods on the native text objects, whatever class the call names.
    let id = args.first()?.as_obj()?;
    if heap.get_native::<CharReader>(id).is_some() {
        let reader = heap.get_native_mut::<CharReader>(id)?;
        let remaining = reader.chars.len().saturating_sub(reader.pos);
        match method_name {
            "read" if args.len() == 1 => {
                let value = reader.chars.get(reader.pos).map_or(-1, |c| *c as i32);
                if value >= 0 {
                    reader.pos += 1;
                }
                return handled(Value::Int(value));
            }
            "read" if args.len() >= 2 => {
                let (off, len) = if args.len() >= 4 {
                    (
                        args[2].as_int().max(0) as usize,
                        args[3].as_int().max(0) as usize,
                    )
                } else {
                    (0, usize::MAX)
                };
                if remaining == 0 {
                    return handled(Value::Int(-1));
                }
                let target = args[1].as_obj()?;
                let take = remaining.min(len).min(match heap.get(target)? {
                    JavaObject::Array { elements, .. } => elements.len().saturating_sub(off),
                    _ => 0,
                });
                let reader = heap.get_native_mut::<CharReader>(id)?;
                let chunk: Vec<Value> = reader.chars[reader.pos..reader.pos + take]
                    .iter()
                    .map(|c| Value::Int(*c as i32))
                    .collect();
                reader.pos += take;
                if let Some(JavaObject::Array { elements, .. }) = heap.get_mut(target) {
                    for (i, v) in chunk.into_iter().enumerate() {
                        elements[off + i] = v;
                    }
                }
                return handled(Value::Int(take as i32));
            }
            "readLine" => {
                if remaining == 0 {
                    return handled(Value::NULL);
                }
                let rest = &reader.chars[reader.pos..];
                let end = rest.iter().position(|c| *c == '\n').unwrap_or(rest.len());
                let line: String = rest[..end]
                    .iter()
                    .collect::<String>()
                    .trim_end_matches('\r')
                    .to_string();
                reader.pos += (end + 1).min(rest.len());
                return handled(alloc_str(heap, line));
            }
            "ready" => return handled(Value::from_bool(remaining > 0)),
            "skip" => {
                let n = (args.get(1)?.as_long().max(0) as usize).min(remaining);
                reader.pos += n;
                return handled(Value::Long(n as i64));
            }
            "close" => return handled(Value::NULL),
            _ => {}
        }
    }
    if let Some(sink) = heap.get_native_mut::<StringSink>(id) {
        match method_name {
            "write" | "append" if args.len() >= 2 => {
                let piece = match &args[1] {
                    Value::Int(c) => char::from_u32(*c as u32)
                        .map(String::from)
                        .unwrap_or_default(),
                    other => {
                        let other = other.clone();
                        let text = match heap.get(other.as_obj()?)? {
                            JavaObject::String(s) => s.clone(),
                            JavaObject::Array { elements, .. } => elements
                                .iter()
                                .filter_map(|v| char::from_u32(v.as_int() as u32))
                                .collect(),
                            _ => return None,
                        };
                        let (off, len) = if args.len() >= 4 {
                            (
                                args[2].as_int().max(0) as usize,
                                args[3].as_int().max(0) as usize,
                            )
                        } else {
                            (0, usize::MAX)
                        };
                        text.chars().skip(off).take(len).collect()
                    }
                };
                heap.get_native_mut::<StringSink>(id)?.text.push_str(&piece);
                return handled(if method_name == "append" {
                    args[0].clone()
                } else {
                    Value::NULL
                });
            }
            "toString" => {
                let text = sink.text.clone();
                return handled(alloc_str(heap, text));
            }
            "flush" | "close" => return handled(Value::NULL),
            _ => {}
        }
    }
    None
}
