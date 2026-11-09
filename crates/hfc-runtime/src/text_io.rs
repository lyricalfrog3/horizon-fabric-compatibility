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
        ("java/lang/String", "format" | "formatted") if !args.is_empty() => {
            // format(fmt, args[]), format(Locale, fmt, args[]), "fmt".formatted(args[])
            let (fmt, rest) = if method_name == "formatted" {
                (string_of(heap, &args[0])?, args.get(1))
            } else if let Some(fmt) = string_of(heap, &args[0]) {
                (fmt, args.get(1))
            } else {
                (string_of(heap, args.get(1)?)?, args.get(2))
            };
            let values = rest.and_then(|v| array_values(heap, v)).unwrap_or_default();
            let text = java_format(heap, &fmt, &values);
            return handled(alloc_str(heap, text));
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

/// Java's `Float.toString`/`Double.toString` for the plain cases (`1.0`, `0.5`).
fn java_float_string(value: f64) -> String {
    if value.is_nan() {
        "NaN".into()
    } else if value.is_infinite() {
        if value > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else if value == value.trunc() && value.abs() < 1e7 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

/// `String.format`: `%s %S %d %x %X %o %c %b %f %e %n %%` with the `-`, `+`,
/// ` `, `0`, `,` and `#` flags, width, precision and `n$` argument indexes.
fn java_format(heap: &Heap, fmt: &str, args: &[Value]) -> String {
    let as_text = |value: &Value| -> String {
        match value {
            Value::Int(v) => v.to_string(),
            Value::Long(v) => v.to_string(),
            Value::Float(v) => java_float_string(*v as f64),
            Value::Double(v) => java_float_string(*v),
            Value::Object(None) => "null".into(),
            other => crate::builtins::text_of_value(heap, other),
        }
    };
    let as_i64 = |value: &Value| match value {
        Value::Long(v) => *v,
        other => other.as_int() as i64,
    };
    let as_f64 = |value: &Value| match value {
        Value::Float(v) => *v as f64,
        Value::Double(v) => *v,
        Value::Long(v) => *v as f64,
        other => other.as_int() as f64,
    };
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    let mut next_arg = 0;
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut spec = String::new();
        let conversion = loop {
            match chars.next() {
                Some(ch) if ch.is_ascii_alphabetic() || ch == '%' => break Some(ch),
                Some(ch) => spec.push(ch),
                None => break None,
            }
        };
        let Some(conversion) = conversion else {
            out.push('%');
            out.push_str(&spec);
            break;
        };
        match conversion {
            '%' => {
                out.push('%');
                continue;
            }
            'n' => {
                out.push('\n');
                continue;
            }
            _ => {}
        }
        let mut index = None;
        if let Some((num, rest)) = spec.split_once('$') {
            index = num.parse::<usize>().ok().map(|n| n.saturating_sub(1));
            spec = rest.to_string();
        }
        let flags: String = spec
            .chars()
            .take_while(|c| "-+ 0,#(".contains(*c))
            .collect();
        let rest = &spec[flags.len()..];
        let (width, precision) = match rest.split_once('.') {
            Some((w, p)) => (w.parse::<usize>().unwrap_or(0), p.parse::<usize>().ok()),
            None => (rest.parse::<usize>().unwrap_or(0), None),
        };
        let arg = match index {
            Some(i) => args.get(i),
            None => {
                next_arg += 1;
                args.get(next_arg - 1)
            }
        };
        let Some(arg) = arg else {
            out.push_str("null");
            continue;
        };
        let mut body = match conversion {
            's' | 'S' => {
                let mut text = as_text(arg);
                if let Some(p) = precision {
                    text = text.chars().take(p).collect();
                }
                if conversion == 'S' {
                    text = text.to_uppercase();
                }
                text
            }
            'd' => {
                let value = as_i64(arg);
                let digits = value.unsigned_abs().to_string();
                let digits = if flags.contains(',') {
                    let mut grouped = String::new();
                    for (i, d) in digits.chars().enumerate() {
                        if i > 0 && (digits.len() - i) % 3 == 0 {
                            grouped.push(',');
                        }
                        grouped.push(d);
                    }
                    grouped
                } else {
                    digits
                };
                let sign = if value < 0 {
                    "-"
                } else if flags.contains('+') {
                    "+"
                } else if flags.contains(' ') {
                    " "
                } else {
                    ""
                };
                format!("{sign}{digits}")
            }
            'x' | 'X' | 'o' => {
                let value = match arg {
                    Value::Long(v) => *v as u64,
                    other => other.as_int() as u32 as u64,
                };
                let text = match conversion {
                    'x' => format!("{value:x}"),
                    'X' => format!("{value:X}"),
                    _ => format!("{value:o}"),
                };
                if flags.contains('#') && conversion != 'o' {
                    format!("0{}{text}", conversion)
                } else {
                    text
                }
            }
            'c' | 'C' => char::from_u32(arg.as_int() as u32)
                .map(|c| c.to_string())
                .unwrap_or_default(),
            'b' | 'B' => match arg {
                Value::Object(None) => "false".into(),
                other => {
                    let text = crate::builtins::text_of_value(heap, other);
                    if text == "false" { "false" } else { "true" }.into()
                }
            },
            'f' => {
                let value = as_f64(arg);
                let mut text = format!("{:.*}", precision.unwrap_or(6), value.abs());
                if flags.contains(',') {
                    let (int, frac) = text.split_once('.').unwrap_or((&text, ""));
                    let mut grouped = String::new();
                    for (i, d) in int.chars().enumerate() {
                        if i > 0 && (int.len() - i) % 3 == 0 {
                            grouped.push(',');
                        }
                        grouped.push(d);
                    }
                    text = if frac.is_empty() {
                        grouped
                    } else {
                        format!("{grouped}.{frac}")
                    };
                }
                let sign = if value.is_sign_negative() && value != 0.0 {
                    "-"
                } else if flags.contains('+') {
                    "+"
                } else {
                    ""
                };
                format!("{sign}{text}")
            }
            'e' | 'E' => {
                let text = format!("{:.*e}", precision.unwrap_or(6), as_f64(arg));
                let text = match text.split_once('e') {
                    Some((m, e)) => {
                        let exp: i32 = e.parse().unwrap_or(0);
                        format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
                    }
                    None => text,
                };
                if conversion == 'E' {
                    text.to_uppercase()
                } else {
                    text
                }
            }
            _ => as_text(arg),
        };
        let len = body.chars().count();
        if len < width {
            let pad = width - len;
            if flags.contains('-') {
                body.push_str(&" ".repeat(pad));
            } else if flags.contains('0') && matches!(conversion, 'd' | 'x' | 'X' | 'o' | 'f') {
                let sign_len = usize::from(body.starts_with(['-', '+', ' ']));
                body.insert_str(sign_len, &"0".repeat(pad));
            } else {
                body.insert_str(0, &" ".repeat(pad));
            }
        }
        out.push_str(&body);
    }
    out
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
