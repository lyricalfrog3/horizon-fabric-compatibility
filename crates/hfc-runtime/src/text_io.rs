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
            fn held_ids(&self) -> Vec<usize> {
                // The three text natives are `Console` (a `String` line),
                // `CharReader` (decoded `char`s) and `StringSink` (written
                // text): none holds a heap id.
                Vec::new()
            }
        }
    };
}
/// `System.out` / `System.err`: what a mod prints goes to the host process's
/// standard output or error, as it would from a JVM.
#[derive(Debug)]
pub struct Console {
    pub error: bool,
    /// Text since the last line break, written out a line at a time.
    pub line: String,
}

native!(Console, "java/io/PrintStream");
native!(CharReader, "java/io/Reader");
native!(StringSink, "java/io/StringWriter");

fn string_of(heap: &Heap, value: &Value) -> Option<String> {
    heap.get_string(value.as_obj()?).map(str::to_string)
}

/// Allocate a NEW string object. Every `String` method that returns a string
/// (a case conversion, a slice, `concat`, `replace`, `trim`, ...) allocates a
/// fresh object in Java: only literals and `String.intern()` are shared, so two
/// results with the same text are different objects. `Heap::alloc_string`
/// interns by text, so it must not be used here.
fn alloc_str(heap: &mut Heap, s: impl Into<String>) -> Value {
    Value::Object(Some(crate::builtins::new_string(heap, s)))
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
            // Indices are UTF-16 code units: `"a😀b".indexOf("b")` is 3, not 2,
            // and an `int` argument is ONE code unit (a surrogate is a legal
            // needle, and a value above 0xFFFF is not a character at all and
            // matches nothing).
            let text = crate::builtins::utf16_units(&string_of(heap, &args[0])?);
            let needle: Vec<u16> = match &args[1] {
                // `indexOf(int)`: a BMP value is one code unit, and a
                // supplementary code point is the pair of surrogates that
                // encodes it (Java searches for the code point, not the unit).
                Value::Int(c) if (0..=0xFFFF).contains(c) => vec![*c as u16],
                Value::Int(c) => match u32::try_from(*c).ok().and_then(char::from_u32) {
                    Some(c) => c.encode_utf16(&mut [0; 2]).to_vec(),
                    None => return handled(Value::Int(-1)),
                },
                other => crate::builtins::utf16_units(&string_of(heap, other)?),
            };
            let last = method_name == "lastIndexOf";
            let found = if last {
                // A negative start answers -1 in Java, and the default start is
                // the last place the needle still fits.
                let from = match args.get(2) {
                    None => text.len(),
                    Some(v) => usize::try_from(v.as_int()).ok().unwrap_or(0),
                };
                if args.get(2).is_some_and(|v| v.as_int() < 0) {
                    None
                } else {
                    crate::builtins::last_index_of_units_at(&text, &needle, from)
                }
            } else {
                let from = args.get(2).map(|v| v.as_int().max(0) as usize).unwrap_or(0);
                crate::builtins::index_of_units_from(&text, &needle, from)
            };
            return handled(Value::Int(found.map_or(-1, |i| i as i32)));
        }
        ("java/lang/String", "replace") if args.len() == 3 => {
            // `replace(char, char)` works on code units, so it can replace half
            // of a surrogate pair; `replace(CharSequence, CharSequence)` works
            // on text. A replacement that changes nothing answers this string.
            let text = string_of(heap, &args[0])?;
            let replaced = match (&args[1], &args[2]) {
                (Value::Int(from), Value::Int(to)) => {
                    let (from, to) = (*from as u16, *to as u16);
                    if from == to {
                        None
                    } else {
                        let units: Vec<u16> = text
                            .encode_utf16()
                            .map(|unit| if unit == from { to } else { unit })
                            .collect();
                        // Java answers this string when nothing matched.
                        (crate::builtins::text_of_units(&units) != text)
                            .then(|| crate::builtins::text_of_units(&units))
                    }
                }
                other => {
                    let (Value::Object(Some(_)) | Value::Object(None), _) = other else {
                        return None;
                    };
                    let from = string_of(heap, &args[1])?;
                    let to = string_of(heap, &args[2])?;
                    if from.is_empty() || !text.contains(&from) {
                        None
                    } else {
                        Some(text.replace(&from, &to))
                    }
                }
            };
            return handled(match replaced {
                Some(text) => alloc_str(heap, text),
                None => args[0].clone(),
            });
        }
        ("java/lang/String", "concat") if args.len() == 2 => {
            let other = string_of(heap, &args[1])?;
            // `concat` answers this string when the argument is empty, and
            // allocates a new one otherwise.
            if other.is_empty() {
                return handled(args[0].clone());
            }
            let text = string_of(heap, &args[0])? + &other;
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
        ("java/lang/String", "format" | "formatted") if !args.is_empty() => {
            // The three shapes are `format(String, Object...)`,
            // `format(Locale, String, Object...)` and `"fmt".formatted(Object...)`.
            // The receiver is implicit in a virtual call, so a static
            // `format`'s first argument is the format string, while a virtual
            // `formatted`'s first argument is the format string too; a static
            // `format` reached through a `Locale` has it second. Boxed numbers
            // are plain values here, so the argument is told apart by whether
            // it is text, not by the descriptor.
            let (fmt, rest) = if method_name == "formatted" {
                (string_of(heap, &args[0])?, args.get(1))
            } else if let Some(fmt) = string_of(heap, &args[0]) {
                (fmt, args.get(1))
            } else {
                // `format(Locale, String, Object...)`: the format string is
                // second. A null one is a NullPointerException on a JVM, and
                // the caller's `?` propagates "not handled" for that, which
                // would be wrong; report it as the JDK's null-format failure.
                let Some(fmt) = string_of(heap, args.get(1)?) else {
                    return Some(BuiltinResult::Threw(crate::num_builtins::exception(
                        heap,
                        "java/lang/NullPointerException",
                        "Cannot invoke \"String.length()\" because \"s\" is null",
                    )));
                };
                (fmt, args.get(2))
            };
            // A null varargs array is not "no arguments": on a JVM every
            // specifier then sees a null argument, and none is reported missing.
            let values = match rest {
                Some(Value::Object(None)) => None,
                Some(value) => array_values(heap, value),
                None => None,
            };
            let values = if rest.is_none() {
                // `formatted()` with no arguments is `format(this)` on an empty
                // array, not a null one.
                Some(Vec::new())
            } else {
                values
            };
            return match java_format(heap, &fmt, values.as_deref()) {
                Ok(text) => handled(alloc_str(heap, text)),
                // The JDK throws one of the `IllegalFormatException` subclasses
                // here; leave it pending so the calling frame catches it or
                // unwinds exactly as on a JVM.
                Err(error) => Some(BuiltinResult::Threw(error.into_exception(heap))),
            };
        }
        ("java/lang/Comparable" | "java/lang/String", "compareTo") if args.len() == 2 => {
            // Natural ordering of the plain values the runtime keeps unboxed.
            let ordering = if let (Some(a), Some(b)) =
                (string_of(heap, &args[0]), string_of(heap, &args[1]))
            {
                // Java compares UTF-16 code units and returns their difference.
                let (mut left, mut right) = (a.encode_utf16(), b.encode_utf16());
                let (mut left_len, mut right_len) = (0, 0);
                let mut diff = 0;
                loop {
                    match (left.next(), right.next()) {
                        (Some(x), Some(y)) => {
                            left_len += 1;
                            right_len += 1;
                            if x != y && diff == 0 {
                                diff = x as i32 - y as i32;
                            }
                        }
                        (Some(_), None) => left_len += 1,
                        (None, Some(_)) => right_len += 1,
                        (None, None) => break,
                    }
                    if diff != 0 {
                        break;
                    }
                }
                if diff != 0 {
                    diff
                } else {
                    left_len - right_len
                }
            } else {
                match (&args[0], &args[1]) {
                    (Value::Double(_) | Value::Float(_), _)
                    | (_, Value::Double(_) | Value::Float(_)) => {
                        let (a, b) = (args[0].as_double(), args[1].as_double());
                        a.partial_cmp(&b).map_or(0, |o| o as i32)
                    }
                    (Value::Int(_) | Value::Long(_), Value::Int(_) | Value::Long(_)) => {
                        args[0].as_long().cmp(&args[1].as_long()) as i32
                    }
                    _ => return None,
                }
            };
            return handled(Value::Int(ordering));
        }
        ("java/lang/String", "join") if args.len() == 2 => {
            // join(delimiter, elements...) or join(delimiter, Iterable)
            let delimiter = crate::builtins::text_of_value(heap, &args[0]);
            let items: Vec<Value> = match args[1].as_obj().and_then(|id| heap.get(id)) {
                Some(JavaObject::List(items)) | Some(JavaObject::Set(items)) => items.clone(),
                Some(JavaObject::Array { elements, .. }) => elements.clone(),
                _ => return None,
            };
            let parts: Vec<String> = items
                .iter()
                .map(|item| match item {
                    Value::Object(None) => "null".to_string(),
                    other => crate::builtins::text_of_value(heap, other),
                })
                .collect();
            return handled(alloc_str(heap, parts.join(&delimiter)));
        }
        // The JDK-internal `join(prefix, suffix, delimiter, String[], size)`
        // that `StringJoiner` (and so `Collectors.joining`) reaches through
        // `JavaLangAccess`. The runtime's strings are not byte arrays, so the
        // JDK's own body cannot run on them.
        ("java/lang/String", "join") if args.len() == 5 => {
            let prefix = string_of(heap, &args[0])?.to_string();
            let suffix = string_of(heap, &args[1])?.to_string();
            let delimiter = string_of(heap, &args[2])?.to_string();
            let size = args[4].as_int().max(0) as usize;
            let elements = match args[3].as_obj().and_then(|id| heap.get(id)) {
                Some(JavaObject::Array { elements, .. }) => elements.clone(),
                _ => Vec::new(),
            };
            let parts: Vec<String> = elements
                .iter()
                .take(size)
                .map(|item| match item {
                    Value::Object(None) => "null".to_string(),
                    other => crate::builtins::text_of_value(heap, other),
                })
                .collect();
            return handled(alloc_str(
                heap,
                format!("{prefix}{}{suffix}", parts.join(&delimiter)),
            ));
        }
        ("java/lang/String", "chars" | "codePoints") if args.len() == 1 => {
            let text = string_of(heap, &args[0])?;
            let items = if method_name == "chars" {
                text.encode_utf16().map(|c| Value::Int(c as i32)).collect()
            } else {
                text.chars().map(|c| Value::Int(c as i32)).collect()
            };
            let sid = heap.alloc_stream(items);
            return handled(Value::Object(Some(sid)));
        }
        ("java/lang/String", "split") if args.len() >= 2 => {
            let text = string_of(heap, &args[0])?;
            let pattern = java_pattern(&string_of(heap, &args[1])?);
            let limit = args.get(2).map(|v| v.as_int()).unwrap_or(0);
            let parts = java_split(&text, &pattern, limit);
            let elements = parts
                .into_iter()
                .map(|part| alloc_str(heap, part))
                .collect();
            let id = heap.alloc(JavaObject::Array {
                elem_type: "java/lang/String".into(),
                elements,
            });
            return handled(Value::Object(Some(id)));
        }
        ("java/lang/String", "replaceAll" | "replaceFirst") if args.len() >= 3 => {
            let text = string_of(heap, &args[0])?;
            let pattern = java_pattern(&string_of(heap, &args[1])?);
            let replacement = java_replacement(&string_of(heap, &args[2])?);
            // An invalid pattern would throw PatternSyntaxException; the text stays as it is.
            let out = match regex::Regex::new(&pattern) {
                Ok(re) if method_name == "replaceAll" => {
                    re.replace_all(&text, replacement.as_str()).into_owned()
                }
                Ok(re) => re.replace(&text, replacement.as_str()).into_owned(),
                Err(_) => text,
            };
            return handled(alloc_str(heap, out));
        }
        ("java/lang/String", "matches") if args.len() >= 2 => {
            // The whole text must match, as `Pattern.matches`.
            let text = string_of(heap, &args[0])?;
            let pattern = java_pattern(&string_of(heap, &args[1])?);
            let hit = regex::Regex::new(&format!("^(?:{pattern})$"))
                .map(|re| re.is_match(&text))
                .unwrap_or(false);
            return handled(Value::from_bool(hit));
        }
        ("java/lang/String", "toCharArray") => {
            let text = string_of(heap, args.first()?)?;
            let id = heap.alloc(JavaObject::Array {
                elem_type: "char".into(),
                elements: text.chars().map(|c| Value::Int(c as i32)).collect(),
            });
            return handled(Value::Object(Some(id)));
        }
        // new String(), new String(String), new String(StringBuilder|StringBuffer):
        // a new string object with that text (not the interned one).
        ("java/lang/String", "<init>")
            if args.len() == 1 || args.len() == 2 && string_of(heap, &args[1]).is_some() =>
        {
            let text = args
                .get(1)
                .and_then(|v| string_of(heap, v))
                .unwrap_or_default();
            let id = args[0].as_obj()?;
            *heap.get_mut(id)? = JavaObject::String(text);
            return handled(Value::NULL);
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

/// A Java `Matcher.replaceAll` replacement as a `regex` crate one: `$1` and
/// `${name}` keep working, `\x` is a literal `x`, and a literal `$` is `$$`.
fn java_replacement(java: &str) -> String {
    let mut out = String::with_capacity(java.len());
    let mut chars = java.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('$') => out.push_str("$$"),
                Some(other) => out.push(other),
                None => {}
            },
            '$' if matches!(chars.peek(), Some(n) if n.is_ascii_digit()) => {
                // `$12` in Java is group 1 then "2" when 12 groups do not exist; the
                // crate reads the longest name, so brace the number.
                let mut digits = String::new();
                while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                    digits.push(*d);
                    chars.next();
                }
                out.push_str(&format!("${{{digits}}}"));
            }
            other => out.push(other),
        }
    }
    out
}

/// A Java regex as a `regex` crate one. `\R` (any line break) is Java-only.
fn java_pattern(java: &str) -> String {
    java.replace(
        "\\R",
        "(?:\\r\\n|[\\n\\x0B\\x0C\\r\\x{85}\\x{2028}\\x{2029}])",
    )
}

/// `String.split(regex, limit)`: a leading empty piece from a zero-width match
/// is dropped, and at limit 0 trailing empty pieces are too.
fn java_split(text: &str, pattern: &str, limit: i32) -> Vec<String> {
    let Ok(re) = regex::Regex::new(pattern) else {
        return vec![text.to_string()];
    };
    let mut parts = Vec::new();
    let mut last = 0;
    for m in re.find_iter(text) {
        if limit > 0 && parts.len() as i32 == limit - 1 {
            break;
        }
        if m.end() == 0 {
            continue;
        }
        if m.start() == m.end() && m.start() >= text.len() {
            continue;
        }
        parts.push(text[last..m.start()].to_string());
        last = m.end();
    }
    if parts.is_empty() {
        return vec![text.to_string()];
    }
    parts.push(text[last..].to_string());
    if limit == 0 {
        while parts.len() > 1 && parts.last().is_some_and(|p| p.is_empty()) {
            parts.pop();
        }
    }
    parts
}

// ===========================================================================
// `String.format` / `String.formatted`: java.util.Formatter, transcribed
// ===========================================================================
//
// A mod's visible text routinely comes out of `java.util.Formatter`: a
// centring mixin pads a title with `String.format("%" + n + "s", "")`, a
// tooltip formats a count with `%,d`, a config screen a hex colour with
// `%06X`. An approximation is not good enough for any of them — a wrong width
// leaves a title as a run of spaces, and a format string returned unchanged
// prints `%s` where a name belongs.
//
// The syntax follows the JDK exactly: the `%[argument_index$][flags][width]
// [.precision]conversion` grammar (parsed the way the JDK's regex backtracks,
// so a malformed specifier is diagnosed with the same exception), the
// per-conversion flag compatibility checks, the `-`/`0` padding rules and the
// `n$`/`<` argument bookkeeping. The conversions are `%s %S %b %B %c %C %d %o
// %x %X %e %E %f %g %G %a %A %h %H %n %%`.
//
// Two JDK internals are reproduced rather than approximated:
//
//  * `DoubleToDecimal`'s digit selection — the shortest decimal that
//    `Double.toString` prints, which `e`/`f`/`g` round from.
//  * `jdk.internal.math.FormattedFPDecimal`'s rounding, including its rule that
//    ties round toward positive infinity: `%.0f` of `2.5` is `3` and `%.0f` of
//    `0.5` is `1`.
//
// `%t`/`%T` (date and time) is the one conversion that is not implemented: its
// output depends on the calendar and the default time zone, which the runtime
// does not carry, and answering it from a guess would be fabricated text. It
// throws `UnknownFormatConversionException` (an `IllegalFormatException`)
// rather than being silently dropped, so a mod that asks for it fails loudly.
//
// Localisation: the runtime carries no CLDR bundle, so digits, grouping and the
// decimal point are the root locale's (`0`-`9`, `,`, `.`) whatever Locale
// argument a caller passes. That is the same host limitation `NumberFormat` has
// here, and it is stated rather than guessed: the runtime does not pretend to
// produce a German or Arabic rendering.

/// The JDK exception a format error corresponds to.
///
/// The payload matters, not just the text: the JDK's exception classes derive
/// `getMessage()` from their own private fields (`UnknownFormatConversion`
/// stores the conversion in `s` and formats `Conversion = '%s'`), so the object
/// is built with those fields set and the JDK's own `getMessage` bytecode
/// produces the message. Storing only a pre-rendered `detailMessage` would make
/// `e.getMessage()` answer `Conversion = 'null'` on the real class.
struct FormatError {
    class: &'static str,
    payload: FormatErrorPayload,
}

enum FormatErrorPayload {
    /// `UnknownFormatConversionException.s`
    Conversion(String),
    /// `MissingFormatArgumentException.s` / `MissingFormatWidthException.s`
    Specifier(String),
    /// `IllegalFormatFlagsException.flags`
    Flags(String),
    /// `FormatFlagsConversionMismatchException.f` and `.c`
    Mismatch(String, char),
    /// `DuplicateFormatFlagsException.flags`
    DuplicateFlag(String),
    /// `IllegalFormatWidthException.w`
    Width(i32),
    /// `IllegalFormatPrecisionException.p`
    Precision(i32),
    /// `IllegalFormatArgumentIndexException.illegalIndex`
    ArgumentIndex(i32),
    /// `IllegalFormatCodePointException.c`
    CodePoint(i32),
    /// `IllegalFormatConversionException.c` and `.arg`
    ConversionMismatch(char, String),
}

impl FormatError {
    fn new(class: &'static str, payload: FormatErrorPayload) -> Self {
        FormatError { class, payload }
    }

    /// The exception object, with the fields the JDK class reads and a
    /// `detailMessage` matching what its `getMessage` derives.
    fn into_exception(self, heap: &mut Heap) -> Value {
        use crate::value::JavaObject;
        let mut fields: Vec<(&str, Value)> = Vec::new();
        let message = match self.payload {
            FormatErrorPayload::Conversion(c) => {
                fields.push(("s", Value::Object(Some(heap.alloc_string(c.clone())))));
                format!("Conversion = '{c}'")
            }
            FormatErrorPayload::Specifier(s) => {
                fields.push(("s", Value::Object(Some(heap.alloc_string(s.clone())))));
                format!("Format specifier '{s}'")
            }
            FormatErrorPayload::Flags(flags) => {
                fields.push((
                    "flags",
                    Value::Object(Some(heap.alloc_string(flags.clone()))),
                ));
                format!("Flags = '{flags}'")
            }
            FormatErrorPayload::Mismatch(flags, c) => {
                fields.push(("f", Value::Object(Some(heap.alloc_string(flags.clone())))));
                fields.push(("c", Value::Int(c as i32)));
                format!("Conversion = {c}, Flags = {flags}")
            }
            FormatErrorPayload::DuplicateFlag(c) => {
                let text = c.to_string();
                fields.push((
                    "flags",
                    Value::Object(Some(heap.alloc_string(text.clone()))),
                ));
                format!("Flags = '{text}'")
            }
            FormatErrorPayload::Width(w) => {
                fields.push(("w", Value::Int(w)));
                w.to_string()
            }
            FormatErrorPayload::Precision(p) => {
                fields.push(("p", Value::Int(p)));
                p.to_string()
            }
            FormatErrorPayload::ArgumentIndex(index) => {
                fields.push(("illegalIndex", Value::Int(index)));
                if index == i32::MIN {
                    "Format argument index: (not representable as int)".to_string()
                } else {
                    format!("Illegal format argument index = {index}")
                }
            }
            FormatErrorPayload::CodePoint(c) => {
                fields.push(("c", Value::Int(c)));
                // `String.format("Code point = %#x", c)`: an int prints unsigned.
                format!("Code point = 0x{:x}", c as u32)
            }
            FormatErrorPayload::ConversionMismatch(c, class) => {
                fields.push(("c", Value::Int(c as i32)));
                let class_id = heap.alloc_class(class.clone());
                fields.push(("arg", Value::Object(Some(class_id))));
                format!("{c} != {class}")
            }
        };
        let detail = Value::Object(Some(heap.alloc_string(message)));
        let mut values: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
        values.insert("detailMessage".to_string(), detail);
        for (name, value) in fields {
            values.insert(name.to_string(), value);
        }
        Value::Object(Some(heap.alloc(JavaObject::Instance {
            class_name: self.class.to_string(),
            fields: values,
        })))
    }
}

fn unknown_conversion(c: char) -> FormatError {
    FormatError::new(
        "java/util/UnknownFormatConversionException",
        FormatErrorPayload::Conversion(c.to_string()),
    )
}

fn missing_argument(spec: &str) -> FormatError {
    FormatError::new(
        "java/util/MissingFormatArgumentException",
        FormatErrorPayload::Specifier(spec.to_string()),
    )
}

fn missing_width(spec: &str) -> FormatError {
    FormatError::new(
        "java/util/MissingFormatWidthException",
        FormatErrorPayload::Specifier(spec.to_string()),
    )
}

fn illegal_flags(flags: Flags) -> FormatError {
    FormatError::new(
        "java/util/IllegalFormatFlagsException",
        FormatErrorPayload::Flags(flags.text()),
    )
}

fn illegal_width(width: i32) -> FormatError {
    FormatError::new(
        "java/util/IllegalFormatWidthException",
        FormatErrorPayload::Width(width),
    )
}

fn illegal_precision(precision: i32) -> FormatError {
    FormatError::new(
        "java/util/IllegalFormatPrecisionException",
        FormatErrorPayload::Precision(precision),
    )
}

fn illegal_code_point(c: i32) -> FormatError {
    FormatError::new(
        "java/util/IllegalFormatCodePointException",
        FormatErrorPayload::CodePoint(c),
    )
}

fn conversion_mismatch(conversion: char, argument_class: &str) -> FormatError {
    FormatError::new(
        "java/util/IllegalFormatConversionException",
        FormatErrorPayload::ConversionMismatch(conversion, argument_class.to_string()),
    )
}

fn illegal_argument_index(index: i32) -> FormatError {
    FormatError::new(
        "java/util/IllegalFormatArgumentIndexException",
        FormatErrorPayload::ArgumentIndex(index),
    )
}

// --- flags ----------------------------------------------------------------

/// One bit per flag, in the JDK's own order.
const FLAG_LEFT_JUSTIFY: u32 = 1 << 0;
/// The internal bit an upper-case conversion sets; `Flags.toString` prints it
/// (`%+ E` reports `Flags = '^+ '`).
const FLAG_UPPERCASE: u32 = 1 << 1;
const FLAG_ALTERNATE: u32 = 1 << 2;
const FLAG_PLUS: u32 = 1 << 3;
const FLAG_LEADING_SPACE: u32 = 1 << 4;
const FLAG_ZERO_PAD: u32 = 1 << 5;
const FLAG_GROUP: u32 = 1 << 6;
const FLAG_PARENTHESES: u32 = 1 << 7;
const FLAG_PREVIOUS: u32 = 1 << 8;

/// `Formatter.Flags`.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Flags(u32);

impl Flags {
    const NONE: Flags = Flags(0);

    fn contains(self, flag: u32) -> bool {
        (self.0 & flag) == flag
    }

    fn contains_any(self, flags: u32) -> bool {
        (self.0 & flags) != 0
    }

    fn add(&mut self, flag: u32) {
        self.0 |= flag;
    }

    fn without(self, flag: u32) -> Flags {
        Flags(self.0 & !flag)
    }

    /// `Flags.toString`: the flags in the JDK's fixed order, which the exception
    /// messages embed.
    fn text(self) -> String {
        let mut out = String::new();
        for (flag, c) in [
            (FLAG_LEFT_JUSTIFY, '-'),
            (FLAG_UPPERCASE, '^'),
            (FLAG_ALTERNATE, '#'),
            (FLAG_PLUS, '+'),
            (FLAG_LEADING_SPACE, ' '),
            (FLAG_ZERO_PAD, '0'),
            (FLAG_GROUP, ','),
            (FLAG_PARENTHESES, '('),
            (FLAG_PREVIOUS, '<'),
        ] {
            if self.contains(flag) {
                out.push(c);
            }
        }
        out
    }

    /// `Flags.parse`: one bit per flag character; a repeat is a
    /// `DuplicateFormatFlagsException` naming the character itself.
    fn parse(text: &str) -> Result<Flags, FormatError> {
        let mut flags = Flags::NONE;
        for c in text.chars() {
            let flag = match c {
                '-' => FLAG_LEFT_JUSTIFY,
                '#' => FLAG_ALTERNATE,
                '+' => FLAG_PLUS,
                ' ' => FLAG_LEADING_SPACE,
                '0' => FLAG_ZERO_PAD,
                ',' => FLAG_GROUP,
                '(' => FLAG_PARENTHESES,
                '<' => FLAG_PREVIOUS,
                // The grammar's flag class is exactly the characters above, so
                // this is unreachable; the JDK's own exception keeps it honest
                // if the grammar ever widens.
                _ => {
                    return Err(FormatError::new(
                        "java/util/UnknownFormatFlagsException",
                        FormatErrorPayload::Flags(c.to_string()),
                    ));
                }
            };
            if flags.contains(flag) {
                return Err(FormatError::new(
                    "java/util/DuplicateFormatFlagsException",
                    FormatErrorPayload::DuplicateFlag(c.to_string()),
                ));
            }
            flags.add(flag);
        }
        Ok(flags)
    }
}

// --- conversions ----------------------------------------------------------

/// `Formatter.Conversion`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Conversion {
    Boolean,
    String,
    HashCode,
    Character,
    DecimalInteger,
    OctalInteger,
    HexadecimalInteger,
    Scientific,
    General,
    DecimalFloat,
    HexadecimalFloat,
    LineSeparator,
    PercentSign,
    /// `t`/`T`: the date/time family. The grammar is supported; the calendar
    /// itself is not (see `print_spec`).
    DateTime,
}

impl Conversion {
    /// `Conversion.isValid`, before the upper-case fold. Note that `t`/`T` is
    /// *not* here: the JDK matches it in its own regex group, ahead of the
    /// conversion character.
    fn is_valid(c: char) -> bool {
        matches!(
            c,
            'b' | 'B'
                | 's'
                | 'S'
                | 'h'
                | 'H'
                | 'c'
                | 'C'
                | 'd'
                | 'o'
                | 'x'
                | 'X'
                | 'e'
                | 'E'
                | 'g'
                | 'G'
                | 'f'
                | 'a'
                | 'A'
                | 'n'
                | '%'
        )
    }

    /// The conversion a character names (case-insensitively). `None` for a
    /// letter the JDK does not treat as a conversion.
    fn from_char(c: char) -> Option<Conversion> {
        Some(match c.to_ascii_lowercase() {
            'b' => Conversion::Boolean,
            's' => Conversion::String,
            'h' => Conversion::HashCode,
            'c' => Conversion::Character,
            'd' => Conversion::DecimalInteger,
            'o' => Conversion::OctalInteger,
            'x' => Conversion::HexadecimalInteger,
            'e' => Conversion::Scientific,
            'g' => Conversion::General,
            'f' => Conversion::DecimalFloat,
            'a' => Conversion::HexadecimalFloat,
            'n' => Conversion::LineSeparator,
            '%' => Conversion::PercentSign,
            _ => return None,
        })
    }

    fn is_general(self) -> bool {
        matches!(
            self,
            Conversion::Boolean | Conversion::String | Conversion::HashCode
        )
    }

    fn is_character(self) -> bool {
        self == Conversion::Character
    }

    fn is_integer(self) -> bool {
        matches!(
            self,
            Conversion::DecimalInteger | Conversion::OctalInteger | Conversion::HexadecimalInteger
        )
    }

    fn is_float(self) -> bool {
        matches!(
            self,
            Conversion::Scientific
                | Conversion::General
                | Conversion::DecimalFloat
                | Conversion::HexadecimalFloat
        )
    }

    fn is_text(self) -> bool {
        matches!(self, Conversion::LineSeparator | Conversion::PercentSign)
    }

    fn is_date_time(self) -> bool {
        self == Conversion::DateTime
    }
}

// --- specifiers -----------------------------------------------------------

/// `Formatter.FormatSpecifier`'s fields. `index` uses the JDK's sentinels:
/// `0` is "the next argument", `-1` is "the previous one" (`%<`), and `-2` is a
/// conversion that consumes no argument (`%n`, `%%`).
#[derive(Clone)]
struct Spec {
    index: i32,
    flags: Flags,
    width: i32,
    precision: i32,
    conversion: Conversion,
    /// The conversion as the format string spelled it, which the
    /// `MissingFormat*Exception` messages echo back.
    conversion_char: char,
}

impl Spec {
    /// `FormatSpecifier.toString`. The internal `UPPERCASE` bit is stripped:
    /// the case of the conversion character already carries it.
    fn text(&self) -> String {
        let mut out = String::from("%");
        out.push_str(&self.flags.without(FLAG_UPPERCASE).text());
        if self.index > 0 {
            out.push_str(&self.index.to_string());
            out.push('$');
        }
        if self.width != -1 {
            out.push_str(&self.width.to_string());
        }
        if self.precision != -1 {
            out.push('.');
            out.push_str(&self.precision.to_string());
        }
        out.push(self.conversion_char);
        out
    }
}

fn is_flag_char(c: char) -> bool {
    matches!(c, '-' | '#' | '+' | ' ' | '0' | ',' | '(' | '<')
}

/// One successful match of the JDK's specifier regex, holding the captured
/// groups verbatim plus the index just past the conversion.
struct RawSpec {
    index: String,
    flags: String,
    width: String,
    /// The precision text including its leading `.`, or empty.
    precision: String,
    /// `t`/`T`, the date/time prefix.
    date_time: Option<char>,
    conversion: char,
    end: usize,
}

/// Match the JDK's `%(\d+\$)?([-#+ 0,(\<]*)?(\d+)?(\.\d+)?([tT])?([a-zA-Z%])`
/// at `at`, with a regex engine's greedy-then-backtracking priority.
///
/// Whether the specifier is well formed at all matters, because the JDK only
/// raises its detailed exceptions (a bad index, a duplicate flag, a huge width)
/// for a specifier that *matches*. One that does not match is reported as an
/// unknown conversion naming the character right after `%`, which is why
/// `%012,d` says `Conversion = '0'`: the greedy flag run takes `0`, then no
/// conversion follows `12`, and every shorter split fails too.
fn match_spec(format: &str, at: usize) -> Option<RawSpec> {
    let bytes = format.as_bytes();
    let digits_end = |from: usize| {
        let mut i = from;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        i
    };

    // group 1: (\d+\$)? — the longest digit run followed by '$'.
    let first_digit_end = digits_end(at);
    let (index_text, after_index) = if first_digit_end > at
        && first_digit_end < bytes.len()
        && bytes[first_digit_end] == b'$'
    {
        (&format[at..first_digit_end], first_digit_end + 1)
    } else {
        ("", at)
    };

    // group 2: ([-#+ 0,(<]*)? and group 3: (\d+)? both accept digits, so the
    // regex prefers the longest flag run and only shortens it when the rest
    // cannot match.
    let greedy_flags_end = {
        let mut i = after_index;
        while i < bytes.len() && is_flag_char(bytes[i] as char) {
            i += 1;
        }
        i
    };
    for flags_at in (after_index..=greedy_flags_end).rev() {
        let flags_text = &format[after_index..flags_at];
        let greedy_width_end = digits_end(flags_at);
        for width_at in (flags_at..=greedy_width_end).rev() {
            let width_text = &format[flags_at..width_at];
            let mut j = width_at;
            // group 4: (\.\d+) — at least one digit is required.
            let precision_text = if j < bytes.len() && bytes[j] == b'.' {
                let end = digits_end(j + 1);
                if end == j + 1 {
                    // `%.` with no digits: this split fails; try a shorter
                    // width or flag run.
                    continue;
                }
                let text = &format[j..end];
                j = end;
                text
            } else {
                ""
            };
            // group 5: ([tT])? — it must be followed by group 6.
            let mut date_time = None;
            if j < bytes.len() && matches!(bytes[j], b't' | b'T') {
                if j + 1 < bytes.len()
                    && (bytes[j + 1].is_ascii_alphabetic() || bytes[j + 1] == b'%')
                {
                    date_time = Some(bytes[j] as char);
                    j += 1;
                }
            }
            // group 6: ([a-zA-Z%]).
            if j >= bytes.len() {
                continue;
            }
            let c = bytes[j] as char;
            if !c.is_ascii_alphabetic() && c != '%' {
                continue;
            }
            return Some(RawSpec {
                index: index_text.to_string(),
                flags: flags_text.to_string(),
                width: width_text.to_string(),
                precision: precision_text.to_string(),
                date_time,
                conversion: c,
                end: j + 1,
            });
        }
    }
    None
}

/// Build a `Spec` from a raw match: the JDK's numeric parses, then its `check*`
/// validation.
fn build_spec(raw: RawSpec) -> Result<Spec, FormatError> {
    // `index(s, start, end)`: the digits before the `$`.
    let mut index = 0;
    if !raw.index.is_empty() {
        match raw.index.parse::<i32>() {
            Ok(value) if value > 0 => index = value,
            // The JDK rejects a non-positive index outright.
            Ok(value) => return Err(illegal_argument_index(value)),
            Err(_) => return Err(illegal_argument_index(i32::MIN)),
        }
    }

    let mut flags = Flags::parse(&raw.flags)?;
    if flags.contains(FLAG_PREVIOUS) {
        // `%<s` reuses the previous argument whatever an explicit `n$` said:
        // the JDK applies `flags()` after `index()`.
        index = -1;
    }

    let mut width = -1;
    if !raw.width.is_empty() {
        match raw.width.parse::<i32>() {
            Ok(value) if value >= 0 => width = value,
            _ => return Err(illegal_width(i32::MIN)),
        }
    }

    let mut precision = -1;
    if !raw.precision.is_empty() {
        // The captured text includes the leading '.'.
        match raw.precision[1..].parse::<i32>() {
            Ok(value) if value >= 0 => precision = value,
            _ => return Err(illegal_precision(i32::MIN)),
        }
    }

    if let Some(prefix) = raw.date_time {
        // `%tY` and friends are parsed by the JDK, and `checkDateTime` runs
        // against the conversion *before* the argument is looked at. Only a date
        // and time conversion this runtime cannot produce is reported, at print
        // time, where the JDK's own `IllegalFormatConversionException` lands for
        // a non-date argument (`%tY` of an `Integer`). A well-typed date
        // argument reaches a conversion with no host calendar, and that throws
        // there instead of inventing a date.
        let mut flags = Flags::parse(&raw.flags)?;
        if flags.contains(FLAG_PREVIOUS) {
            index = -1;
        }
        if prefix.is_ascii_uppercase() {
            flags.add(FLAG_UPPERCASE);
        }
        let spec = Spec {
            index,
            flags,
            width,
            precision,
            conversion: Conversion::DateTime,
            conversion_char: raw.conversion,
        };
        check(&spec)?;
        return Ok(spec);
    }

    let conversion_char = raw.conversion;
    if !Conversion::is_valid(conversion_char) {
        return Err(unknown_conversion(conversion_char));
    }
    let Some(conversion) = Conversion::from_char(conversion_char) else {
        return Err(unknown_conversion(conversion_char));
    };
    if conversion_char.is_ascii_uppercase() {
        flags.add(FLAG_UPPERCASE);
    }
    let spec = Spec {
        index: if conversion.is_text() { -2 } else { index },
        flags,
        width,
        precision,
        conversion,
        conversion_char,
    };
    check(&spec)?;
    Ok(spec)
}

/// The JDK's `checkGeneral`/`checkCharacter`/`checkInteger`/`checkFloat`/
/// `checkText`, in the JDK's order, so a specifier with more than one problem
/// reports the same exception the JVM does.
fn check(spec: &Spec) -> Result<(), FormatError> {
    let flags = spec.flags;
    if spec.conversion.is_general() {
        if matches!(spec.conversion, Conversion::Boolean | Conversion::HashCode)
            && flags.contains(FLAG_ALTERNATE)
        {
            return Err(mismatch(flags, FLAG_ALTERNATE, spec.conversion_char));
        }
        // '-' requires a width.
        if spec.width == -1 && flags.contains(FLAG_LEFT_JUSTIFY) {
            return Err(missing_width(&spec.text()));
        }
        return check_bad_flags(
            flags,
            FLAG_PLUS | FLAG_LEADING_SPACE | FLAG_ZERO_PAD | FLAG_GROUP | FLAG_PARENTHESES,
            spec.conversion_char,
        );
    }
    if spec.conversion.is_character() {
        if spec.precision != -1 {
            return Err(illegal_precision(spec.precision));
        }
        check_bad_flags(
            flags,
            FLAG_ALTERNATE
                | FLAG_PLUS
                | FLAG_LEADING_SPACE
                | FLAG_ZERO_PAD
                | FLAG_GROUP
                | FLAG_PARENTHESES,
            spec.conversion_char,
        )?;
        if spec.width == -1 && flags.contains(FLAG_LEFT_JUSTIFY) {
            return Err(missing_width(&spec.text()));
        }
        return Ok(());
    }
    if spec.conversion.is_integer() {
        check_numeric(spec)?;
        if spec.precision != -1 {
            return Err(illegal_precision(spec.precision));
        }
        return match spec.conversion {
            Conversion::DecimalInteger => {
                check_bad_flags(flags, FLAG_ALTERNATE, spec.conversion_char)
            }
            // `o`, `x` and `X` all reject grouping here; the radix forms'
            // sign-flag check belongs to `print(long)`, after the argument type
            // has been established, so it lives in `print_integer`.
            _ => check_bad_flags(flags, FLAG_GROUP, spec.conversion_char),
        };
    }
    if spec.conversion.is_float() {
        check_numeric(spec)?;
        return match spec.conversion {
            Conversion::DecimalFloat => Ok(()),
            Conversion::HexadecimalFloat => {
                check_bad_flags(flags, FLAG_PARENTHESES | FLAG_GROUP, spec.conversion_char)
            }
            Conversion::Scientific => check_bad_flags(flags, FLAG_GROUP, spec.conversion_char),
            Conversion::General => check_bad_flags(flags, FLAG_ALTERNATE, spec.conversion_char),
            _ => Ok(()),
        };
    }
    if spec.conversion.is_date_time() {
        // `checkDateTime`: the precision is rejected, the date/time conversion
        // character must be one the JDK knows, and most flags are bad. Only the
        // syntax is validated here; the argument is checked at print time, as
        // the JDK does.
        if spec.precision != -1 {
            return Err(illegal_precision(spec.precision));
        }
        check_bad_flags(
            flags,
            FLAG_ALTERNATE
                | FLAG_PLUS
                | FLAG_LEADING_SPACE
                | FLAG_ZERO_PAD
                | FLAG_GROUP
                | FLAG_PARENTHESES,
            spec.conversion_char,
        )?;
        if spec.width == -1 && flags.contains(FLAG_LEFT_JUSTIFY) {
            return Err(missing_width(&spec.text()));
        }
        return Ok(());
    }
    if spec.conversion.is_text() {
        if spec.precision != -1 {
            return Err(illegal_precision(spec.precision));
        }
        return match spec.conversion {
            Conversion::PercentSign => {
                if flags != Flags(FLAG_LEFT_JUSTIFY) && flags != Flags::NONE {
                    return Err(illegal_flags(flags));
                }
                if spec.width == -1 && flags.contains(FLAG_LEFT_JUSTIFY) {
                    return Err(missing_width(&spec.text()));
                }
                Ok(())
            }
            Conversion::LineSeparator => {
                if spec.width != -1 {
                    return Err(illegal_width(spec.width));
                }
                if flags != Flags::NONE {
                    return Err(illegal_flags(flags));
                }
                Ok(())
            }
            _ => Ok(()),
        };
    }
    Err(unknown_conversion(spec.conversion_char))
}

/// `failMismatch(f, c)`: the message names only the offending flags, in the
/// JDK's own order.
fn mismatch(flags: Flags, bad: u32, conversion: char) -> FormatError {
    FormatError::new(
        "java/util/FormatFlagsConversionMismatchException",
        FormatErrorPayload::Mismatch(Flags(flags.0 & bad).text(), conversion),
    )
}

fn check_bad_flags(flags: Flags, bad: u32, conversion: char) -> Result<(), FormatError> {
    if flags.contains_any(bad) {
        return Err(mismatch(flags, bad, conversion));
    }
    Ok(())
}

/// `checkNumeric`, shared by the integer and float families.
fn check_numeric(spec: &Spec) -> Result<(), FormatError> {
    let flags = spec.flags;
    if spec.width != -1 && spec.width < 0 {
        return Err(illegal_width(spec.width));
    }
    if spec.precision != -1 && spec.precision < 0 {
        return Err(illegal_precision(spec.precision));
    }
    // '-' and '0' require a width.
    if spec.width == -1 && flags.contains_any(FLAG_LEFT_JUSTIFY | FLAG_ZERO_PAD) {
        return Err(missing_width(&spec.text()));
    }
    // A '+' and a leading space contradict; so do '-' and '0'.
    if flags.contains(FLAG_PLUS | FLAG_LEADING_SPACE)
        || flags.contains(FLAG_LEFT_JUSTIFY | FLAG_ZERO_PAD)
    {
        return Err(illegal_flags(flags));
    }
    Ok(())
}

// --- parsing a whole format string ----------------------------------------

/// One element of a parsed format string.
enum FormatString<'a> {
    Fixed(&'a str),
    Spec(Spec),
}

/// `Formatter.parse`.
fn parse_format(format: &str) -> Result<Vec<FormatString<'_>>, FormatError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < format.len() {
        let Some(offset) = format[i..].find('%') else {
            out.push(FormatString::Fixed(&format[i..]));
            return Ok(out);
        };
        let percent = i + offset;
        if percent != i {
            out.push(FormatString::Fixed(&format[i..percent]));
        }
        let after = percent + 1;
        let Some(&next) = format.as_bytes().get(after) else {
            // A trailing '%' has no conversion to name: the JDK reports '%'.
            return Err(unknown_conversion('%'));
        };
        let c = next as char;
        if Conversion::is_valid(c) {
            // The bare form (`%s`, `%n`, `%%`), which the JDK bypasses its
            // regex for: a specifier with default flags, width and precision.
            let Some(conversion) = Conversion::from_char(c) else {
                return Err(unknown_conversion(c));
            };
            let mut flags = Flags::NONE;
            if c.is_ascii_uppercase() {
                flags.add(FLAG_UPPERCASE);
            }
            let spec = Spec {
                index: if conversion.is_text() { -2 } else { 0 },
                flags,
                width: -1,
                precision: -1,
                conversion,
                conversion_char: c,
            };
            check(&spec)?;
            out.push(FormatString::Spec(spec));
            i = after + 1;
        } else {
            let raw = match_spec(format, after).ok_or_else(|| {
                // No specifier matches here, so the JDK names the character
                // right after the '%' (`%012,d` reports '0', not ',').
                unknown_conversion(format[after..].chars().next().unwrap_or('%'))
            })?;
            i = raw.end;
            out.push(FormatString::Spec(build_spec(raw)?));
        }
    }
    Ok(out)
}

// --- arguments ------------------------------------------------------------

/// What a format argument is, as far as the conversions distinguish them.
///
/// The runtime keeps a boxed `char`, `byte` and `short` as a plain `int` (it
/// does not record which wrapper made it), so a format string cannot tell a
/// `Character` from an `Integer`. `%c` and `%d` accept either, as a JVM does
/// for `Integer`/`Byte`/`Short`; the only visible consequence is the class name
/// in a conversion-mismatch message.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FmtKind {
    Int,
    Long,
    Float,
    Double,
    Text,
    Bool,
    Object,
}

struct FmtArg {
    kind: FmtKind,
    value: Value,
    is_null: bool,
}

impl FmtArg {
    fn null() -> FmtArg {
        FmtArg {
            kind: FmtKind::Object,
            value: Value::NULL,
            is_null: true,
        }
    }
}

/// Classify a value the way the converter sees it.
fn fmt_arg(heap: &Heap, value: &Value) -> FmtArg {
    if value.is_null() {
        return FmtArg::null();
    }
    let kind = match value {
        Value::Int(_) => FmtKind::Int,
        Value::Long(_) => FmtKind::Long,
        Value::Float(_) => FmtKind::Float,
        Value::Double(_) => FmtKind::Double,
        Value::Object(Some(id)) => {
            if heap.get_string(*id).is_some() {
                FmtKind::Text
            } else if matches!(
                heap.get(*id),
                Some(JavaObject::Instance { class_name, .. }) if class_name == "java/lang/Boolean"
            ) {
                FmtKind::Bool
            } else {
                FmtKind::Object
            }
        }
        Value::Object(None) => FmtKind::Object,
    };
    FmtArg {
        kind,
        value: value.clone(),
        is_null: false,
    }
}

/// The `java.lang.X` name `IllegalFormatConversionException` prints.
fn fmt_class_name(heap: &Heap, arg: &FmtArg) -> String {
    match arg.kind {
        FmtKind::Int => "java.lang.Integer".into(),
        FmtKind::Long => "java.lang.Long".into(),
        FmtKind::Float => "java.lang.Float".into(),
        FmtKind::Double => "java.lang.Double".into(),
        FmtKind::Text => "java.lang.String".into(),
        FmtKind::Bool => "java.lang.Boolean".into(),
        FmtKind::Object => match arg.value.as_obj().and_then(|id| heap.get(id)) {
            Some(JavaObject::Instance { class_name, .. }) => class_name.replace('/', "."),
            Some(other) => other.class_name().replace('/', "."),
            None => "java.lang.Object".into(),
        },
    }
}

/// `String.format`/`String.formatted`.
///
/// `args` is `None` when the varargs array itself was null: a JVM then treats
/// every argument as null (and never reports a missing one).
///
/// The error names the exception the JDK would throw; the caller builds it and
/// leaves it pending, so the calling frame catches it exactly as on a JVM.
fn java_format(heap: &Heap, fmt: &str, args: Option<&[Value]>) -> Result<String, FormatError> {
    let parts = parse_format(fmt)?;
    let mut out = String::new();
    // `last` is the index of the last argument a specifier used (-1 before any);
    // `lasto` counts the ordinary specifiers, which is the numbering `%s`
    // follows even after an explicit `%2$s`.
    let mut last: i32 = -1;
    let mut lasto: i32 = -1;
    for part in &parts {
        let FormatString::Spec(spec) = part else {
            if let FormatString::Fixed(text) = part {
                out.push_str(text);
            }
            continue;
        };
        let arg = match spec.index {
            -2 => FmtArg::null(),
            -1 => {
                if last < 0 || args.is_some_and(|a| last as usize >= a.len()) {
                    return Err(missing_argument(&spec.text()));
                }
                arg_at(heap, args, last as usize)
            }
            0 => {
                lasto += 1;
                last = lasto;
                if args.is_some_and(|a| lasto as usize >= a.len()) {
                    return Err(missing_argument(&spec.text()));
                }
                arg_at(heap, args, lasto as usize)
            }
            index => {
                last = index - 1;
                if args.is_some_and(|a| last as usize >= a.len()) {
                    return Err(missing_argument(&spec.text()));
                }
                arg_at(heap, args, last as usize)
            }
        };
        print_spec(heap, spec, &arg, &mut out)?;
    }
    Ok(out)
}

/// The argument at `index`, or a null argument (a null varargs array, or an
/// index past its end once the caller has ruled out a missing one).
fn arg_at(heap: &Heap, args: Option<&[Value]>, index: usize) -> FmtArg {
    match args.and_then(|a| a.get(index)) {
        Some(value) => fmt_arg(heap, value),
        None => FmtArg::null(),
    }
}

// --- printing -------------------------------------------------------------

fn print_spec(heap: &Heap, spec: &Spec, arg: &FmtArg, out: &mut String) -> Result<(), FormatError> {
    match spec.conversion {
        Conversion::LineSeparator => {
            // `System.lineSeparator()`. A width or a flag on `%n` is rejected
            // while checking, so nothing else applies.
            out.push('\n');
        }
        Conversion::PercentSign => justify(out, "%", spec),
        Conversion::Boolean => {
            // `%b` is false only for `null` and `Boolean.FALSE`; every other
            // object is true whatever its `toString` says (a `"false"` string is
            // true). A boxed boolean is the only value that varies.
            let text = if arg.is_null {
                "false"
            } else if arg.kind == FmtKind::Bool {
                if boxed_bool(heap, &arg.value) {
                    "true"
                } else {
                    "false"
                }
            } else {
                "true"
            };
            print_text(spec, text, out);
        }
        Conversion::HashCode => {
            let text = if arg.is_null {
                "null".to_string()
            } else {
                // `Integer.toHexString(arg.hashCode())`, which is unsigned.
                format!("{:x}", java_hash_code(heap, arg) as u32)
            };
            print_text(spec, &text, out);
        }
        Conversion::String => {
            let text = if arg.is_null {
                "null".to_string()
            } else {
                // `%s` calls `toString`, and `Double.toString`/`Float.toString`
                // have their own shape (`1.0`, `1.0E7`), not Rust's `3`/`1e7`.
                match arg.kind {
                    FmtKind::Double => java_double_to_string(arg.value.as_double()),
                    FmtKind::Float => java_float_to_string(arg.value.as_float()),
                    _ => crate::builtins::text_of_value(heap, &arg.value),
                }
            };
            print_text(spec, &text, out);
        }
        Conversion::Character => {
            if arg.is_null {
                print_text(spec, "null", out);
            } else {
                // `printCharacter` takes Character, Byte, Short or Integer. The
                // runtime cannot tell `char` from `int`, and `%c` of either is
                // the same code point, so both are accepted.
                let text = match arg.kind {
                    FmtKind::Int => code_point_text(arg.value.as_int()),
                    _ => Err(conversion_mismatch(
                        spec.conversion_char,
                        &fmt_class_name(heap, arg),
                    )),
                }?;
                print_text(spec, &text, out);
            }
        }
        Conversion::DateTime => {
            // A date/time conversion needs the calendar and the default time
            // zone. With a null argument the JDK prints "null" (its
            // `printDateTime` checks that first); with a date argument there is
            // no host calendar to read, so this throws rather than inventing a
            // date. Everything else is the JDK's own conversion mismatch.
            if arg.is_null {
                print_text(spec, "null", out);
            } else if is_date_argument(heap, arg) {
                return Err(FormatError::new(
                    "java/util/UnknownFormatConversionException",
                    FormatErrorPayload::Conversion(format!(
                        "{}{}",
                        if spec.flags.contains(FLAG_UPPERCASE) {
                            'T'
                        } else {
                            't'
                        },
                        spec.conversion_char
                    )),
                ));
            } else {
                return Err(conversion_mismatch(
                    spec.conversion_char,
                    &fmt_class_name(heap, arg),
                ));
            }
        }
        Conversion::DecimalInteger | Conversion::OctalInteger | Conversion::HexadecimalInteger => {
            if arg.is_null {
                print_text(spec, "null", out);
            } else {
                print_integer(heap, spec, arg, out)?;
            }
        }
        Conversion::Scientific
        | Conversion::General
        | Conversion::DecimalFloat
        | Conversion::HexadecimalFloat => {
            if arg.is_null {
                print_text(spec, "null", out);
            } else {
                print_float(heap, spec, arg, out)?;
            }
        }
    }
    Ok(())
}

/// Whether an argument is one a date/time conversion accepts: a `long` (an
/// epoch millisecond count), a `java.util.Date`, a `Calendar` or a
/// `TemporalAccessor`.
fn is_date_argument(heap: &Heap, arg: &FmtArg) -> bool {
    if arg.kind == FmtKind::Long {
        return true;
    }
    match arg.value.as_obj().and_then(|id| heap.get(id)) {
        Some(JavaObject::Instance { class_name, .. }) => {
            matches!(
                class_name.as_str(),
                "java/util/Date" | "java/util/Calendar" | "java/util/GregorianCalendar"
            ) || class_name.starts_with("java/time/")
        }
        _ => false,
    }
}

/// A `java.lang.Boolean` instance's value. The runtime keeps a boxed boolean in
/// a `value` field (a plain `int` would be indistinguishable from a number).
fn boxed_bool(heap: &Heap, value: &Value) -> bool {
    match value.as_obj().and_then(|id| heap.get(id)) {
        Some(JavaObject::Instance { fields, .. }) => {
            fields.get("value").map(|v| v.as_bool()).unwrap_or(false)
        }
        _ => value.as_bool(),
    }
}

/// `arg.hashCode()`, over the value the runtime keeps.
fn java_hash_code(heap: &Heap, arg: &FmtArg) -> i32 {
    match arg.kind {
        FmtKind::Int => arg.value.as_int(),
        FmtKind::Long => {
            let v = arg.value.as_long();
            (v ^ (v >> 32)) as i32
        }
        FmtKind::Float => {
            let v = arg.value.as_float();
            // `Float.hashCode` is `floatToIntBits`, which collapses every NaN.
            let bits = if v.is_nan() {
                0x7fc0_0000u32
            } else {
                v.to_bits()
            };
            bits as i32
        }
        FmtKind::Double => {
            let v = arg.value.as_double();
            let bits = if v.is_nan() {
                0x7ff8_0000_0000_0000u64
            } else {
                v.to_bits()
            };
            (bits ^ (bits >> 32)) as i32
        }
        FmtKind::Text => arg
            .value
            .as_obj()
            .and_then(|id| heap.get_string(id))
            .map(crate::builtins::java_string_hash)
            .unwrap_or(0),
        FmtKind::Bool => {
            if boxed_bool(heap, &arg.value) {
                1231
            } else {
                1237
            }
        }
        // `Object.hashCode` is identity-based on a JVM; the heap id is the
        // runtime's stable identity for the object.
        FmtKind::Object => arg.value.as_obj().unwrap_or(0) as i32,
    }
}

/// The `%c` text of an `int`/`Character` argument.
fn code_point_text(cp: i32) -> Result<String, FormatError> {
    match char::from_u32(cp as u32) {
        Some(c) => Ok(c.to_string()),
        // A surrogate is a valid code point to `Character.isValidCodePoint`,
        // and `%c` of it is that single `char`; a Rust `String` cannot hold a
        // lone surrogate, so it renders as the replacement character a JDK
        // encoder would substitute for one.
        None if (0xD800..=0xDFFF).contains(&cp) => Ok("\u{FFFD}".to_string()),
        None => Err(illegal_code_point(cp)),
    }
}

/// `print(fmt, s, l)`: precision truncates (in UTF-16 code units, as
/// `String.substring` counts), an upper-case conversion upper-cases, then the
/// width pads.
fn print_text(spec: &Spec, text: &str, out: &mut String) {
    let mut text = text.to_string();
    if spec.precision != -1 {
        let units: Vec<u16> = text.encode_utf16().collect();
        let precision = spec.precision as usize;
        if precision < units.len() {
            // A precision that cuts a surrogate pair in half leaves a lone
            // surrogate, which a Java `String` can hold and a Rust `String`
            // cannot; it becomes U+FFFD as elsewhere in the runtime.
            text = crate::builtins::text_of_units(&units[..precision]);
        }
    }
    if spec.flags.contains(FLAG_UPPERCASE) {
        text = text.to_uppercase();
    }
    justify(out, &text, spec);
}

/// `appendJustified`: space padding, on the left unless `-` asks for the right.
fn justify(out: &mut String, text: &str, spec: &Spec) {
    if spec.width == -1 {
        out.push_str(text);
        return;
    }
    let pad = spec.width - text.encode_utf16().count() as i32;
    let pad = pad.max(0) as usize;
    if spec.flags.contains(FLAG_LEFT_JUSTIFY) {
        out.push_str(text);
        out.push_str(&" ".repeat(pad));
    } else {
        out.push_str(&" ".repeat(pad));
        out.push_str(text);
    }
}

/// The integer conversions.
fn print_integer(
    heap: &Heap,
    spec: &Spec,
    arg: &FmtArg,
    out: &mut String,
) -> Result<(), FormatError> {
    // `printInteger` accepts Byte, Short, Integer, Long and BigInteger only.
    if !matches!(arg.kind, FmtKind::Int | FmtKind::Long) {
        return Err(conversion_mismatch(
            spec.conversion_char,
            &fmt_class_name(heap, arg),
        ));
    }
    let mut body = String::new();
    match spec.conversion {
        Conversion::DecimalInteger => {
            let number = match arg.kind {
                FmtKind::Long => arg.value.as_long(),
                _ => arg.value.as_int() as i64,
            };
            let negative = number < 0;
            let sign_len = leading_sign(spec, negative, &mut body);
            // `Long.toString(value, 10)` keeps the minus; the digits after it
            // are grouped and zero-padded. `adjustWidth` spends one column of a
            // parenthesised negative on its closing `)`, and the padding budget
            // counts the sign already written into the builder.
            let digits = number.unsigned_abs().to_string();
            let width = if negative && spec.flags.contains(FLAG_PARENTHESES) {
                spec.width - 1
            } else {
                spec.width
            };
            body.push_str(&magnitude(&digits, spec.flags, width, sign_len));
            trailing_sign(spec, negative, &mut body);
        }
        Conversion::OctalInteger | Conversion::HexadecimalInteger => {
            // `print(long)` re-checks these flags for the radix forms, after the
            // argument's type has been established: a sign would be meaningless
            // on an unsigned rendering.
            check_bad_flags(
                spec.flags,
                FLAG_PARENTHESES | FLAG_LEADING_SPACE | FLAG_PLUS,
                spec.conversion_char,
            )?;
            // `print(int)`/`print(long)` widen a negative value to its unsigned
            // width first, so `%x` of -1 is `ffffffff` and of -1L is
            // `ffffffffffffffff` — never `-1`.
            let unsigned = match arg.kind {
                FmtKind::Long => arg.value.as_long() as u64,
                _ => arg.value.as_int() as u32 as u64,
            };
            let digits = if spec.conversion == Conversion::OctalInteger {
                format!("{unsigned:o}")
            } else if spec.flags.contains(FLAG_UPPERCASE) {
                format!("{unsigned:X}")
            } else {
                format!("{unsigned:x}")
            };
            let prefix = if !spec.flags.contains(FLAG_ALTERNATE) {
                ""
            } else if spec.conversion == Conversion::OctalInteger {
                "0"
            } else if spec.flags.contains(FLAG_UPPERCASE) {
                "0X"
            } else {
                "0x"
            };
            // The JDK's length for the zero padding counts the radix prefix but
            // not a sign (the radix conversions reject sign flags).
            let pad = if spec.flags.contains(FLAG_ZERO_PAD) && spec.width != -1 {
                (spec.width as usize).saturating_sub(prefix.len() + digits.len())
            } else {
                0
            };
            // The JDK appends the radix indicator first, then the zero
            // padding, then the digits, so `%#08x` of 255 is `0x0000ff`.
            body.push_str(prefix);
            body.push_str(&"0".repeat(pad));
            body.push_str(&digits);
        }
        _ => unreachable!("print_integer serves only the integer conversions"),
    }
    justify(out, &body, spec);
    Ok(())
}

/// `leadingSign`: the sign of a decimal integer, before its digits. Answers the
/// length in UTF-16 units of what it wrote, which the zero padding measures its
/// width budget against (the JDK compares the builder's full length).
fn leading_sign(spec: &Spec, negative: bool, out: &mut String) -> usize {
    if !negative {
        if spec.flags.contains(FLAG_PLUS) {
            out.push('+');
            1
        } else if spec.flags.contains(FLAG_LEADING_SPACE) {
            out.push(' ');
            1
        } else {
            0
        }
    } else if spec.flags.contains(FLAG_PARENTHESES) {
        out.push('(');
        1
    } else {
        out.push('-');
        1
    }
}

fn trailing_sign(spec: &Spec, negative: bool, out: &mut String) {
    if negative && spec.flags.contains(FLAG_PARENTHESES) {
        out.push(')');
    }
}

/// `localizedMagnitude` for the root locale: group the digits in threes, then
/// insert the zero padding in front of them (the caller has written any sign).
fn magnitude(digits: &str, flags: Flags, width: i32, sign_len: usize) -> String {
    let mut text = String::new();
    if flags.contains(FLAG_GROUP) {
        for (i, d) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                text.push(',');
            }
            text.push(d);
        }
    } else {
        text.push_str(digits);
    }
    // The width is the JDK's `adjustWidth` value and the comparison is against
    // the builder's total length, sign included.
    let used = sign_len + text.len();
    if flags.contains(FLAG_ZERO_PAD) && width != -1 && (width as usize) > used {
        let zeros = "0".repeat(width as usize - used);
        return format!("{zeros}{text}");
    }
    text
}

/// The float conversions (`e`, `f`, `g`, `a`).
fn print_float(
    heap: &Heap,
    spec: &Spec,
    arg: &FmtArg,
    out: &mut String,
) -> Result<(), FormatError> {
    // `printFloat` accepts Float, Double and BigDecimal only.
    if !matches!(arg.kind, FmtKind::Float | FmtKind::Double) {
        return Err(conversion_mismatch(
            spec.conversion_char,
            &fmt_class_name(heap, arg),
        ));
    }
    // `print(float)` widens to double, and the conversion runs on the double.
    let number = match arg.kind {
        FmtKind::Float => arg.value.as_float() as f64,
        _ => arg.value.as_double(),
    };
    if spec.conversion == Conversion::HexadecimalFloat {
        return print_hex_float(spec, number, out);
    }

    let upper = spec.flags.contains(FLAG_UPPERCASE);
    let mut body = String::new();
    if number.is_nan() {
        // `NaN` takes no sign flag and no zero padding; the width still pads.
        body.push_str(if upper { "NAN" } else { "NaN" });
        justify(out, &body, spec);
        return Ok(());
    }
    let negative = number.is_sign_negative();
    let magnitude = number.abs();
    if magnitude.is_infinite() {
        leading_sign(spec, negative, &mut body);
        body.push_str(if upper { "INFINITY" } else { "Infinity" });
        trailing_sign(spec, negative, &mut body);
        // An infinity never zero-pads: the JDK goes straight to
        // `appendJustified` for it.
        justify(out, &body, spec);
        return Ok(());
    }

    match spec.conversion {
        Conversion::Scientific => {
            let prec = if spec.precision == -1 {
                6
            } else {
                spec.precision
            };
            let rounded = FormattedFp::value_of(magnitude, prec, FpForm::Scientific);
            let mut mantissa = rounded.mantissa();
            add_zeros(&mut mantissa, prec as usize);
            if spec.flags.contains(FLAG_ALTERNATE) && prec == 0 {
                mantissa.push('.');
            }
            let exponent = if magnitude == 0.0 {
                "+00".to_string()
            } else {
                rounded.exponent()
            };
            let sign_len = leading_sign(spec, negative, &mut body);
            let width = adjusted_width(spec, negative, Some(exponent.len() as i32 + 1));
            body.push_str(&zero_pad(sign_len, &mantissa, spec.flags, width));
            body.push(if upper { 'E' } else { 'e' });
            let (sign, digits) = exponent.split_at(1);
            body.push_str(sign);
            body.push_str(digits);
            trailing_sign(spec, negative, &mut body);
        }
        Conversion::DecimalFloat => {
            let prec = if spec.precision == -1 {
                6
            } else {
                spec.precision
            };
            let rounded = FormattedFp::value_of(magnitude, prec, FpForm::Plain);
            let mut mantissa = rounded.mantissa();
            add_zeros(&mut mantissa, prec as usize);
            if spec.flags.contains(FLAG_ALTERNATE) && prec == 0 {
                mantissa.push('.');
            }
            let sign_len = leading_sign(spec, negative, &mut body);
            let width = adjusted_width(spec, negative, None);
            body.push_str(&zero_pad(sign_len, &mantissa, spec.flags, width));
            trailing_sign(spec, negative, &mut body);
        }
        Conversion::General => {
            // `%g` picks plain or scientific by the *rounded* exponent, so the
            // digits are rounded before the form is chosen.
            let mut prec = spec.precision;
            if prec == -1 {
                prec = 6;
            } else if prec == 0 {
                prec = 1;
            }
            let rounded = FormattedFp::value_of(magnitude, prec, FpForm::General);
            let rounded_exponent = rounded.exponent_rounded;
            let exponent = rounded.exponent();
            let has_exponent = !exponent.is_empty();
            let mut mantissa = rounded.mantissa();
            if has_exponent {
                prec -= 1;
            } else {
                prec -= rounded_exponent + 1;
            }
            add_zeros(&mut mantissa, prec.max(0) as usize);
            if spec.flags.contains(FLAG_ALTERNATE) && prec == 0 {
                mantissa.push('.');
            }
            let sign_len = leading_sign(spec, negative, &mut body);
            let width = adjusted_width(
                spec,
                negative,
                if has_exponent {
                    Some(exponent.len() as i32 + 1)
                } else {
                    None
                },
            );
            body.push_str(&zero_pad(sign_len, &mantissa, spec.flags, width));
            if has_exponent {
                body.push(if upper { 'E' } else { 'e' });
                let (sign, digits) = exponent.split_at(1);
                body.push_str(sign);
                body.push_str(digits);
            }
            trailing_sign(spec, negative, &mut body);
        }
        _ => unreachable!("print_float serves only the non-hex float conversions"),
    }
    justify(out, &body, spec);
    Ok(())
}

/// `adjustWidth(width, flags, neg)`, with the exponent's own columns removed
/// first for the conversions that carry one.
fn adjusted_width(spec: &Spec, negative: bool, exponent: Option<i32>) -> i32 {
    let mut width = spec.width;
    if negative && spec.flags.contains(FLAG_PARENTHESES) {
        width -= 1;
    }
    if let Some(extra) = exponent {
        width -= extra;
    }
    width
}

/// `localizedMagnitude` for a mantissa: group the integer digits, then insert
/// the zero padding ahead of them.
///
/// `sign` is what has already been written into the builder; the JDK measures
/// the width against the builder's length, so those characters count as used
/// space rather than padding.
fn zero_pad(sign_len: usize, mantissa: &str, flags: Flags, width: i32) -> String {
    let mut text = String::new();
    let (integer, fraction) = match mantissa.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (mantissa, None),
    };
    if flags.contains(FLAG_GROUP) {
        for (i, d) in integer.chars().enumerate() {
            if i > 0 && (integer.len() - i).is_multiple_of(3) {
                text.push(',');
            }
            text.push(d);
        }
    } else {
        text.push_str(integer);
    }
    if let Some(fraction) = fraction {
        text.push('.');
        text.push_str(fraction);
    }
    if flags.contains(FLAG_ZERO_PAD) && width != -1 {
        let used = sign_len + text.encode_utf16().count();
        let target = width.max(0) as usize;
        if target > used {
            return format!("{}{text}", "0".repeat(target - used));
        }
    }
    text
}

/// `addZeros`: pad a mantissa to `prec` digits after the point, adding the
/// point itself when there is none.
fn add_zeros(mantissa: &mut String, prec: usize) {
    let existing = match mantissa.find('.') {
        Some(dot) => mantissa.len() - dot - 1,
        None => 0,
    };
    if existing == prec {
        return;
    }
    if !mantissa.contains('.') {
        mantissa.push('.');
    }
    if prec > existing {
        mantissa.push_str(&"0".repeat(prec - existing));
    }
}

/// The `a` conversion.
fn print_hex_float(spec: &Spec, value: f64, out: &mut String) -> Result<(), FormatError> {
    let upper = spec.flags.contains(FLAG_UPPERCASE);
    let mut body = String::new();
    if value.is_nan() {
        body.push_str(if upper { "NAN" } else { "NaN" });
        justify(out, &body, spec);
        return Ok(());
    }
    let negative = value.is_sign_negative();
    let sign_len = leading_sign(spec, negative, &mut body);
    if value.is_infinite() {
        body.push_str(if upper { "INFINITY" } else { "Infinity" });
        trailing_sign(spec, negative, &mut body);
        justify(out, &body, spec);
        return Ok(());
    }

    let mut prec = spec.precision;
    if prec == -1 {
        prec = 0;
    } else if prec == 0 {
        prec = 1;
    }
    let digits = hex_double(value.abs(), prec);
    let mut hex = String::from(if upper { "0X" } else { "0x" });
    // `leadingCharacters` is the two-character radix prefix plus whatever sign
    // was actually written (the JDK tests the flags itself, which amounts to the
    // same thing).
    let leading = 2 + sign_len as i32;
    if spec.flags.contains(FLAG_ZERO_PAD) && spec.width != -1 {
        let zeros = spec.width - digits.len() as i32 - leading;
        if zeros > 0 {
            hex.push_str(&"0".repeat(zeros as usize));
        }
    }
    let (mantissa, exponent) = match digits.split_once('p') {
        Some((m, e)) => (m, e),
        None => (digits.as_str(), "0"),
    };
    let mut fraction = mantissa.to_string();
    if prec != 0 {
        add_zeros(&mut fraction, prec as usize);
    }
    if upper {
        hex.push_str(&fraction.to_uppercase());
    } else {
        hex.push_str(&fraction);
    }
    hex.push(if upper { 'P' } else { 'p' });
    hex.push_str(exponent);
    body.push_str(&hex);
    trailing_sign(spec, negative, &mut body);
    justify(out, &body, spec);
    Ok(())
}

/// `Double.toHexString` for a non-negative value, minus the `0x`: the value's
/// own significand digits with trailing zeros trimmed, and its exponent.
fn java_hex_double(value: f64) -> String {
    if value == 0.0 {
        return "0.0p0".to_string();
    }
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i64;
    let fraction = bits & 0x000f_ffff_ffff_ffff;
    let (lead, fraction, exp) = if exponent == 0 {
        // A subnormal: the JDK writes it as `0.<fraction>p-1022`.
        (0, fraction, -1022)
    } else {
        (1, fraction, exponent - 1023)
    };
    let hex = format!("{fraction:013x}");
    let trimmed = hex.trim_end_matches('0');
    let digits = if trimmed.is_empty() { "0" } else { trimmed };
    format!("{lead}.{digits}p{exp}")
}

/// `Formatter.FormatSpecifier.hexDouble`: `Double.toHexString`'s digits rounded
/// to `prec` hexadecimal fraction digits, for a non-negative value.
fn hex_double(value: f64, prec: i32) -> String {
    if !value.is_finite() || value == 0.0 || prec == 0 || prec >= 13 {
        return java_hex_double(value);
    }
    // A subnormal is scaled into the normal range and its exponent adjusted
    // back afterwards, exactly as the JDK does.
    let mut d = value;
    let subnormal = ((d.to_bits() >> 52) & 0x7ff) as i64 - 1023 == -1023;
    if subnormal {
        d *= f64::from_bits(0x4350_0000_0000_0000); // 2^54
    }
    let precision = 1 + prec * 4;
    let shift = 64 - precision as u32;
    let bits = d.to_bits();
    let mut significand = bits >> shift;
    let rounding = bits & ((1u64 << shift) - 1);
    // Round to nearest, ties to even.
    let least_zero = significand & 1 == 0;
    let round_bit = (rounding & (1u64 << (shift - 1))) != 0;
    let sticky = shift > 1 && (rounding & !(1u64 << (shift - 1))) != 0;
    if (!least_zero && round_bit) || (least_zero && round_bit && sticky) {
        significand += 1;
    }
    let rounded = f64::from_bits(significand << shift);
    if rounded.is_infinite() {
        // Rounding up overflowed the significand.
        return "1.0p1024".to_string();
    }
    let text = java_hex_double(rounded);
    if !subnormal {
        return text;
    }
    match text.split_once('p') {
        Some((mantissa, exp)) => {
            let adjusted = exp.parse::<i64>().unwrap_or(0) - 54;
            format!("{mantissa}p{adjusted}")
        }
        None => text,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FpForm {
    Scientific,
    Plain,
    General,
}

/// `jdk.internal.math.FormattedFPDecimal`: the decimal `Double.toString` selects
/// (`d = f * 10^e`), rounded to a number of significant digits and split into a
/// mantissa and an exponent.
struct FormattedFp {
    /// The significand as an integer, normalised so `10^(n-1) <= f < 10^n` (and
    /// `n = 0` exactly when `f = 0`).
    f: u64,
    /// `d = f * 10^e`.
    e: i32,
    /// The number of decimal digits of `f` (`0` when `f = 0`).
    n: i32,
    digits: String,
    /// `[+-]ee` with at least two exponent digits, or empty for the plain form.
    exponent: String,
    /// `n + e - 1`: the exponent of the leading digit of the rounded value.
    exponent_rounded: i32,
}

impl FormattedFp {
    fn value_of(value: f64, prec: i32, form: FpForm) -> FormattedFp {
        let (f, e, n) = shortest_digits(value);
        let mut this = FormattedFp {
            f,
            e,
            n,
            digits: String::new(),
            exponent: String::new(),
            exponent_rounded: n + e - 1,
        };
        match form {
            FpForm::Scientific => this.scientific(prec),
            FpForm::Plain => this.plain(prec),
            FpForm::General => this.general(prec),
        }
        this
    }

    fn mantissa(&self) -> String {
        self.digits.clone()
    }

    fn exponent(&self) -> String {
        self.exponent.clone()
    }

    /// `plain`: round to `n + e + prec` significant digits.
    fn plain(&mut self, prec: i32) {
        self.round(self.n as i64 + self.e as i64 + prec as i64);
        self.plain_chars();
    }

    fn plain_chars(&mut self) {
        if self.e >= 0 {
            // A pure integer: the digits, then `e` trailing zeros.
            let zeros = self.e as usize;
            self.digits = format!("{}{}", self.f, "0".repeat(zeros));
        } else if self.n + self.e > 0 {
            // Mixed: the integer part, a point, then the fraction.
            let integer_len = (self.n + self.e) as usize;
            let text = format!("{:0width$}", self.f, width = self.n as usize);
            let (integer, fraction) = text.split_at(integer_len);
            self.digits = format!("{integer}.{fraction}");
        } else {
            // A pure fraction: `0.`, then `-e - n` leading zeros, then the
            // digits. The JDK's buffer is `2 - e` long and the digits sit at its
            // end, so the zero run is `-e - n`.
            let zeros = (-self.e - self.n).max(0) as usize;
            let text = format!("{:0width$}", self.f, width = self.n as usize);
            self.digits = format!("0.{}{text}", "0".repeat(zeros));
        }
    }

    fn scientific(&mut self, prec: i32) {
        self.round(prec as i64 + 1);
        self.scientific_chars(prec);
    }

    fn scientific_chars(&mut self, prec: i32) {
        if prec != 0 {
            let text = format!("{:0width$}", self.f, width = self.n as usize);
            let mut chars: Vec<char> = text.chars().collect();
            chars.insert(1, '.');
            self.digits = chars.into_iter().collect();
        } else {
            self.digits = self.f.to_string();
        }
        self.exponent = self.exponent_chars();
    }

    fn general(&mut self, prec: i32) {
        self.round(prec as i64);
        let er = self.exponent_rounded;
        if (-4..prec).contains(&er) {
            self.plain_chars();
        } else {
            self.scientific_chars(prec - 1);
        }
    }

    fn exponent_chars(&self) -> String {
        let er = self.exponent_rounded;
        format!("{}{:02}", if er >= 0 { '+' } else { '-' }, er.abs())
    }

    /// `round`: round `d = f * 10^e` to `pp` significant digits.
    ///
    /// Ties round toward positive infinity, which is what makes `%.0f` of
    /// `2.5` be `3` and of `0.5` be `1`.
    fn round(&mut self, pp: i64) {
        if self.n == 0 || pp < 0 {
            self.f = 0;
            self.e = 0;
            self.n = 1;
            self.exponent_rounded = self.n + self.e - 1;
            return;
        }
        if pp >= self.n as i64 {
            // Fewer digits than requested: nothing to round away.
            self.exponent_rounded = self.n + self.e - 1;
            return;
        }
        let p = pp as i32;
        self.e += self.n - p;
        let pow10 = 10u64.pow((self.n - p) as u32);
        self.f = (self.f + (pow10 >> 1)) / pow10;
        if p == 0 {
            self.n = 1;
            if self.f == 0 {
                self.e = 0;
            }
            self.exponent_rounded = self.n + self.e - 1;
            return;
        }
        self.n = p;
        if self.f == 10u64.pow(p as u32) {
            // `f` grew an extra digit: absorb one zero into the exponent.
            self.f /= 10;
            self.e += 1;
        }
        self.exponent_rounded = self.n + self.e - 1;
    }
}

/// Java's `Double.toString`.
///
/// Its shape differs from Rust's `{}`: an integral value keeps a `.0`, and
/// outside `1e-3 <= |v| < 1e7` the value is written in scientific notation with
/// one digit before the point (`1.0E7`, `1.0E-4`) rather than expanded.
pub(crate) fn java_double_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .into();
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let magnitude = value.abs();
    // `Double.toString` selects the shortest digit run that reads back as the
    // same value; `shortest_digits` is that selection.
    let (f, e, n) = shortest_digits(magnitude);
    let digits = f.to_string();
    let exponent = n + e - 1;
    if (-3..7).contains(&exponent) {
        format!("{sign}{}", plain_digits(&digits, exponent))
    } else {
        format!("{sign}{}", scientific_digits(&digits, exponent))
    }
}

/// Java's `Float.toString`, the `f32` form of [`java_double_to_string`].
pub(crate) fn java_float_to_string(value: f32) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .into();
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let magnitude = value.abs();
    // The shortest digit run for the *float* (not its widened double).
    let text = format!("{magnitude:e}");
    let (mantissa, exp) = text.split_once('e').unwrap_or((&text, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let exponent = exp;
    if (-3..7).contains(&exponent) {
        format!("{sign}{}", plain_digits(digits, exponent))
    } else {
        format!("{sign}{}", scientific_digits(digits, exponent))
    }
}

/// The plain form of a decimal: the digits with a point placed after
/// `exponent + 1` of them, padded with zeros on whichever side is short, and
/// always with at least one digit on each side of the point.
fn plain_digits(digits: &str, exponent: i32) -> String {
    let integer_len = exponent + 1;
    if integer_len <= 0 {
        // `0.000123`: the leading zeros are `-integer_len` long.
        let zeros = (-integer_len) as usize;
        return format!("0.{}{digits}", "0".repeat(zeros));
    }
    let integer_len = integer_len as usize;
    if integer_len >= digits.len() {
        // The digits are exhausted before the point: pad with zeros and add the
        // mandatory `.0`.
        format!("{}{}.0", digits, "0".repeat(integer_len - digits.len()))
    } else {
        format!("{}.{}", &digits[..integer_len], &digits[integer_len..])
    }
}

/// The scientific form of a decimal: one digit, a point (Java always writes
/// one, so `1.0E7`), the rest of the digits, then `E` and the exponent without
/// a sign character.
fn scientific_digits(digits: &str, exponent: i32) -> String {
    let (first, rest) = digits.split_at(1);
    let rest = if rest.is_empty() { "0" } else { rest };
    format!("{first}.{rest}E{exponent}")
}

/// `DoubleToDecimal.split` for a non-negative finite value: the shortest decimal
/// that `Double.toString` selects, as `(f, e, n)` with `d = f * 10^e` and `n` the
/// digit count of `f`.
///
/// Rust's `{:e}` produces the same shortest round-tripping digit run Java does,
/// so the split is exact and avoids materialising a 300-digit integer.
fn shortest_digits(value: f64) -> (u64, i32, i32) {
    if value == 0.0 {
        return (0, 0, 0);
    }
    let text = format!("{value:e}");
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let n = digits.len() as i32;
    let f: u64 = digits.parse().unwrap_or(0);
    // `{:e}` puts the point after the first digit, so moving it past all `n`
    // digits scales the value by `10^(exponent - n + 1)`.
    (f, exponent - n + 1, n)
}

/// The text a `print`/`println`/`append` argument of descriptor `descriptor`
/// prints.
fn printed(heap: &Heap, descriptor: &str, value: &Value) -> String {
    match (descriptor.as_bytes().get(1), value) {
        (Some(b'Z'), Value::Int(i)) => (*i != 0).to_string(),
        (Some(b'C'), Value::Int(i)) => char::from_u32(*i as u32)
            .map(String::from)
            .unwrap_or_default(),
        (_, Value::Object(None)) => "null".to_string(),
        (_, other) => crate::builtins::value_to_string(heap, other),
    }
}

/// `PrintStream` methods on the runtime's `System.out`/`System.err`.
pub fn console_method(
    heap: &mut Heap,
    method_name: &str,
    descriptor: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    let id = args.first()?.as_obj()?;
    heap.get_native::<Console>(id)?;
    let text = match (method_name, args.len()) {
        ("println", 1) => Some("\n".to_string()),
        ("println", 2) => Some(format!("{}\n", printed(heap, descriptor, &args[1]))),
        ("print" | "append", 2) => Some(printed(heap, descriptor, &args[1])),
        ("write", 2) if descriptor == "(I)V" => {
            Some(char::from(args[1].as_int() as u8).to_string())
        }
        ("flush" | "close", 1) => Some(String::new()),
        ("checkError", 1) => return Some(BuiltinResult::Handled(Value::from_bool(false))),
        _ => None,
    }?;
    let console = heap.get_native_mut::<Console>(id)?;
    console.line.push_str(&text);
    let flush = matches!(method_name, "flush" | "close");
    while let Some(end) = console.line.find('\n') {
        let line: String = console.line.drain(..=end).collect();
        if console.error {
            eprint!("{line}");
        } else {
            print!("{line}");
        }
    }
    if flush && !console.line.is_empty() {
        let rest = std::mem::take(&mut console.line);
        if console.error {
            eprint!("{rest}");
        } else {
            print!("{rest}");
        }
    }
    // `append` returns the stream; the print methods return nothing.
    let result = if method_name == "append" {
        Value::Object(Some(id))
    } else {
        Value::NULL
    };
    Some(BuiltinResult::Handled(result))
}
