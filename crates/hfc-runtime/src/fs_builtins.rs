//! `java.nio.file` / `java.io` file access, backed by [`crate::vfs::Vfs`].

use crate::builtins::BuiltinResult;
use crate::heap::Heap;
use crate::minecraft::ByteStream;
use crate::value::{JavaObject, NativeObject, Value};
use crate::vfs;
use std::any::Any;
use std::collections::HashMap;

/// An open output stream or writer: every write goes straight to the file.
#[derive(Debug)]
pub struct FileSink {
    pub path: String,
}

impl NativeObject for FileSink {
    fn class_name(&self) -> &str {
        "java/io/OutputStream"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

fn string_of(heap: &Heap, value: &Value) -> Option<String> {
    heap.get_string(value.as_obj()?).map(str::to_string)
}

/// The path string of a `Path` the runtime made (not a path inside a mod jar).
fn path_of(heap: &Heap, value: &Value) -> Option<String> {
    let id = value.as_obj()?;
    match heap.get(id)? {
        JavaObject::Instance { fields, .. } if !fields.contains_key("jar") => {
            string_of(heap, fields.get("path")?)
        }
        JavaObject::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn alloc_path(heap: &mut Heap, path: &str) -> Value {
    let sid = heap.alloc_string(path);
    let mut fields = HashMap::new();
    fields.insert("path".to_string(), Value::Object(Some(sid)));
    let id = heap.alloc(JavaObject::Instance {
        class_name: "java/nio/file/Path".into(),
        fields,
    });
    Value::Object(Some(id))
}

fn alloc_bytes(heap: &mut Heap, bytes: &[u8]) -> Value {
    let id = heap.alloc(JavaObject::Array {
        elem_type: "byte".into(),
        elements: bytes.iter().map(|b| Value::Int(*b as i8 as i32)).collect(),
    });
    Value::Object(Some(id))
}

fn bytes_of(heap: &Heap, value: &Value) -> Option<Vec<u8>> {
    match heap.get(value.as_obj()?)? {
        JavaObject::Array { elements, .. } => {
            Some(elements.iter().map(|v| v.as_int() as u8).collect())
        }
        _ => None,
    }
}

/// True when an `OpenOption...` argument names `APPEND`.
fn wants_append(heap: &Heap, args: &[Value]) -> bool {
    args.iter().any(|arg| {
        let Some(id) = arg.as_obj() else { return false };
        match heap.get(id) {
            Some(JavaObject::Array { elements, .. }) => elements.iter().any(|e| is_append(heap, e)),
            _ => is_append(heap, arg),
        }
    })
}

fn is_append(heap: &Heap, value: &Value) -> bool {
    let Some(id) = value.as_obj() else {
        return false;
    };
    match heap.get(id) {
        Some(JavaObject::Instance { fields, .. }) => fields
            .get("name")
            .and_then(|n| string_of(heap, n))
            .is_some_and(|n| n == "APPEND"),
        _ => false,
    }
}

pub(crate) fn game_dir(heap: &Heap) -> String {
    heap.fs
        .root()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/game".into())
}

/// The game and config directories `FabricLoader` reports.
pub fn loader_dir(heap: &mut Heap, method_name: &str) -> Option<Value> {
    let game = game_dir(heap);
    let path = match method_name {
        "getGameDir" => game,
        "getConfigDir" => vfs::join(&game, "config"),
        _ => return None,
    };
    Some(alloc_path(heap, &path))
}

/// The path of a `File` argument (its `path` field), or of a plain string.
fn file_path(heap: &Heap, value: &Value) -> Option<String> {
    let id = value.as_obj()?;
    if let Some(text) = heap.get_string(id) {
        return Some(text.to_string());
    }
    match heap.get(id)? {
        JavaObject::Instance { fields, .. } => string_of(heap, fields.get("path")?),
        _ => None,
    }
}

/// `java.io.FileSystem` / `java.io.UnixFileSystem` natives, backed by the VFS.
///
/// `java.io.File` is real JDK bytecode: it builds its paths, resolves relative
/// ones against `user.dir` and throws its own exceptions. These are the few
/// calls it makes that only a JVM native can answer, so what a mod sees is the
/// JDK's own behaviour over the runtime's file system boundary.
fn unix_file_system(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    if !matches!(class_name, "java/io/FileSystem" | "java/io/UnixFileSystem") {
        return None;
    }
    let handled = |v: Value| Some(BuiltinResult::Handled(v));
    // `java.io.File` hands the natives a path as the mod wrote it, and a JVM
    // resolves a relative one against the process's working directory: here
    // that is the game folder, which is also the sandbox's root.
    let path = |index: usize| {
        args.get(index)
            .and_then(|v| file_path(heap, v))
            .map(|path| {
                if path.starts_with('/') {
                    path
                } else {
                    vfs::join(&game_dir(heap), &path)
                }
            })
    };
    match method_name {
        // The constructor's `static { initIDs(); }`; nothing to register here.
        "initIDs" | "registerNatives" => handled(Value::NULL),
        "canonicalize0" | "canonicalize" => {
            let path = path(1)?;
            let sid = heap.alloc_string(heap.fs.canonicalize(&path));
            handled(Value::Object(Some(sid)))
        }
        "getBooleanAttributes0" | "getBooleanAttributes" => {
            let Some(meta) = path(1).and_then(|path| heap.fs.metadata(&path)) else {
                return handled(Value::Int(0));
            };
            let mut bits = 1; // BA_EXISTS
            if meta.is_file {
                bits |= 2; // BA_REGULAR
            }
            if meta.is_dir {
                bits |= 4; // BA_DIRECTORY
            }
            if meta.hidden {
                bits |= 8; // BA_HIDDEN
            }
            handled(Value::Int(bits))
        }
        "checkAccess0" | "checkAccess" => {
            let path = path(1)?;
            let access = args.get(2).map(Value::as_int).unwrap_or(4);
            handled(Value::from_bool(heap.fs.check_access(&path, access)))
        }
        "getLastModifiedTime0" | "getLastModifiedTime" => {
            let millis = path(1)
                .and_then(|path| heap.fs.metadata(&path))
                .map_or(0, |meta| meta.modified_ms);
            handled(Value::Long(millis))
        }
        "getLength0" | "getLength" => {
            let len = path(1)
                .and_then(|path| heap.fs.metadata(&path))
                .map_or(0, |meta| if meta.is_file { meta.len } else { 0 });
            handled(Value::Long(len as i64))
        }
        "setPermission0" | "setPermission" => {
            let path = path(1)?;
            let access = args.get(2).map(Value::as_int).unwrap_or(0);
            let enable = args.get(3).is_some_and(Value::as_bool);
            let owner_only = args.get(4).is_some_and(Value::as_bool);
            handled(Value::from_bool(
                heap.fs.set_permission(&path, access, enable, owner_only),
            ))
        }
        "createFileExclusively0" | "createFileExclusively" => {
            let path = path(1)?;
            handled(Value::from_bool(heap.fs.create_file_exclusive(&path)))
        }
        "delete0" | "delete" => {
            let path = path(1)?;
            handled(Value::from_bool(heap.fs.delete(&path)))
        }
        "createDirectory0" | "createDirectory" => {
            let path = path(1)?;
            handled(Value::from_bool(heap.fs.create_dir(&path)))
        }
        "rename0" | "rename" => {
            let (Some(from), Some(to)) = (path(1), path(2)) else {
                return handled(Value::from_bool(false));
            };
            handled(Value::from_bool(heap.fs.rename(&from, &to)))
        }
        "setLastModifiedTime0" | "setLastModifiedTime" => {
            let path = path(1)?;
            let millis = args.get(2).map(Value::as_long).unwrap_or(0);
            handled(Value::from_bool(heap.fs.set_last_modified(&path, millis)))
        }
        "setReadOnly0" | "setReadOnly" => {
            let path = path(1)?;
            handled(Value::from_bool(heap.fs.set_read_only(&path)))
        }
        // A directory entry cannot be longer than the file system allows.
        "getNameMax0" | "getNameMax" => handled(Value::Int(255)),
        "list0" | "list" => {
            let Some(path) = path(1) else {
                return handled(Value::NULL);
            };
            if !heap.fs.is_dir(&path) {
                return handled(Value::NULL);
            }
            let names: Vec<Value> = heap
                .fs
                .list(&path)
                .into_iter()
                .map(|name| Value::Object(Some(heap.alloc_string(name))))
                .collect();
            let id = heap.alloc(JavaObject::Array {
                elem_type: "java/lang/String".into(),
                elements: names,
            });
            handled(Value::Object(Some(id)))
        }
        _ => None,
    }
}

pub fn file_system_method(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    let handled = |v: Value| Some(BuiltinResult::Handled(v));

    if let Some(result) = unix_file_system(heap, class_name, method_name, args) {
        return Some(result);
    }

    // Open streams first: they are not Path or Files calls.
    if let Some(id) = args.first().and_then(|v| v.as_obj()) {
        if let Some(sink) = heap.get_native::<FileSink>(id) {
            let path = sink.path.clone();
            let bytes: Option<Vec<u8>> = match method_name {
                "write" if args.len() == 2 => match &args[1] {
                    Value::Int(b) => Some(vec![*b as u8]),
                    other => match heap.get(other.as_obj()?) {
                        Some(JavaObject::String(s)) => Some(s.clone().into_bytes()),
                        _ => bytes_of(heap, other),
                    },
                },
                "write" if args.len() == 4 => {
                    let all = bytes_of(heap, &args[1])
                        .or_else(|| string_of(heap, &args[1]).map(String::into_bytes))?;
                    let (off, len) = (args[2].as_int() as usize, args[3].as_int() as usize);
                    Some(all.get(off..off + len)?.to_vec())
                }
                "append" if args.len() == 2 => string_of(heap, &args[1]).map(String::into_bytes),
                "newLine" => Some(b"\n".to_vec()),
                "flush" | "close" => Some(Vec::new()),
                _ => None,
            };
            let bytes = bytes?;
            heap.fs.write(&path, &bytes, true);
            return handled(if method_name == "append" {
                args[0].clone()
            } else {
                Value::NULL
            });
        }
        // A reader over a file's bytes.
        if matches!(method_name, "readLine" | "lines") {
            if let Some(stream) = heap.get_native_mut::<ByteStream>(id) {
                if method_name == "readLine" {
                    if stream.pos >= stream.data.len() {
                        return handled(Value::NULL);
                    }
                    let rest = &stream.data[stream.pos..];
                    let end = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
                    let line = String::from_utf8_lossy(&rest[..end])
                        .trim_end_matches('\r')
                        .to_string();
                    stream.pos += (end + 1).min(rest.len());
                    let sid = heap.alloc_string(line);
                    return handled(Value::Object(Some(sid)));
                }
            }
        }
    }

    // `ClassLoader.getResourceAsStream` / `Class.getResourceAsStream`: a file
    // in any loaded mod jar, the way Fabric's single class loader finds it.
    if method_name == "getResourceAsStream"
        && matches!(class_name, "java/lang/ClassLoader" | "java/lang/Class")
    {
        let name = string_of(heap, args.get(1)?)?;
        let entry = name.trim_start_matches('/').to_string();
        let found = heap.loaded_mods.values().find_map(|meta| {
            let jar = meta.jar.as_deref()?;
            crate::builtins::read_mod_resource(jar, meta.nested_jar.as_deref(), &entry)
        });
        return match found {
            Some(data) => {
                let id = heap.alloc_native(ByteStream { data, pos: 0 });
                handled(Value::Object(Some(id)))
            }
            None => handled(Value::NULL),
        };
    }

    match class_name {
        "java/nio/file/Paths" | "java/nio/file/Path" if matches!(method_name, "get" | "of") => {
            let first = string_of(heap, args.first()?)?;
            let mut path = first;
            if let Some(id) = args.get(1).and_then(|v| v.as_obj()) {
                let more: Vec<Value> = match heap.get(id) {
                    Some(JavaObject::Array { elements, .. }) => elements.clone(),
                    _ => Vec::new(),
                };
                for part in more {
                    path = vfs::join(&path, &string_of(heap, &part)?);
                }
            }
            let path = vfs::join("", &path);
            handled(alloc_path(heap, &path))
        }
        "java/nio/file/FileSystem" if method_name == "newWatchService" => {
            handled(inert(heap, "java/nio/file/WatchService"))
        }
        "java/nio/file/WatchService" | "java/nio/file/WatchKey"
            if matches!(method_name, "close" | "cancel" | "reset") =>
        {
            handled(Value::from_bool(true))
        }
        "java/nio/file/Path" => {
            let this = path_of(heap, args.first()?)?;
            match method_name {
                "resolve" => {
                    let child = path_of(heap, args.get(1)?)?;
                    let joined = vfs::join(&this, &child);
                    handled(alloc_path(heap, &joined))
                }
                "resolveSibling" => {
                    let sibling = path_of(heap, args.get(1)?)?;
                    let base = vfs::parent(&this).unwrap_or_default();
                    let joined = vfs::join(&base, &sibling);
                    handled(alloc_path(heap, &joined))
                }
                "getParent" => match vfs::parent(&this) {
                    Some(parent) => handled(alloc_path(heap, &parent)),
                    None => handled(Value::NULL),
                },
                "getFileName" => {
                    let name = vfs::file_name(&this).to_string();
                    handled(alloc_path(heap, &name))
                }
                "toString" => {
                    let sid = heap.alloc_string(this);
                    handled(Value::Object(Some(sid)))
                }
                // Watching the game directory for edits is a host boundary: the
                // runtime has no watcher thread, so a watch registers and never
                // fires (a config edited on disk is read at the next start).
                "getFileSystem" => handled(inert(heap, "java/nio/file/FileSystem")),
                "register" => handled(inert(heap, "java/nio/file/WatchKey")),
                "toAbsolutePath" | "normalize" | "toRealPath" => handled(args[0].clone()),
                "toFile" => {
                    let mut fields = HashMap::new();
                    let sid = heap.alloc_string(this);
                    fields.insert("path".to_string(), Value::Object(Some(sid)));
                    let id = heap.alloc(JavaObject::Instance {
                        class_name: "java/io/File".into(),
                        fields,
                    });
                    handled(Value::Object(Some(id)))
                }
                _ => None,
            }
        }
        "java/nio/file/Files" => {
            let path = path_of(heap, args.first()?)?;
            match method_name {
                "exists" => handled(Value::from_bool(heap.fs.exists(&path))),
                "notExists" => handled(Value::from_bool(!heap.fs.exists(&path))),
                "isDirectory" => handled(Value::from_bool(heap.fs.is_dir(&path))),
                "isRegularFile" => handled(Value::from_bool(heap.fs.is_file(&path))),
                "createDirectories" | "createDirectory" => {
                    heap.fs.create_dirs(&path);
                    handled(args[0].clone())
                }
                "createFile" => {
                    if !heap.fs.exists(&path) {
                        heap.fs.write(&path, b"", false);
                    }
                    handled(args[0].clone())
                }
                "delete" | "deleteIfExists" => {
                    handled(Value::from_bool(heap.fs.delete(&path)).clone())
                }
                "size" => handled(Value::Long(
                    heap.fs.read(&path).map_or(0, |b| b.len() as i64),
                )),
                "readAllBytes" => {
                    let bytes = heap.fs.read(&path)?;
                    handled(alloc_bytes(heap, &bytes))
                }
                "readString" => {
                    let text = String::from_utf8_lossy(&heap.fs.read(&path)?).to_string();
                    let sid = heap.alloc_string(text);
                    handled(Value::Object(Some(sid)))
                }
                "readAllLines" => {
                    let text = String::from_utf8_lossy(&heap.fs.read(&path)?).to_string();
                    let lines: Vec<Value> = text
                        .lines()
                        .map(|l| Value::Object(Some(heap.alloc_string(l))))
                        .collect();
                    let lid = heap.alloc_list(lines);
                    handled(Value::Object(Some(lid)))
                }
                "writeString" | "write" => {
                    let data = args.get(1)?;
                    let bytes = match string_of(heap, data) {
                        Some(text) => text.into_bytes(),
                        None => bytes_of(heap, data)?,
                    };
                    let append = wants_append(heap, &args[2..]);
                    heap.fs.write(&path, &bytes, append);
                    handled(args[0].clone())
                }
                "newInputStream" => {
                    // A path inside a mod jar is handled by the caller.
                    let data = heap.fs.read(&path)?;
                    let id = heap.alloc_native(ByteStream { data, pos: 0 });
                    handled(Value::Object(Some(id)))
                }
                "newBufferedReader" => {
                    let data = heap.fs.read(&path)?;
                    let id = heap.alloc_native(ByteStream { data, pos: 0 });
                    handled(Value::Object(Some(id)))
                }
                "newOutputStream" | "newBufferedWriter" => {
                    let append = wants_append(heap, &args[1..]);
                    if !append {
                        heap.fs.write(&path, b"", false);
                    }
                    let id = heap.alloc_native(FileSink { path });
                    handled(Value::Object(Some(id)))
                }
                "list" => {
                    let names = heap.fs.list(&path);
                    let items: Vec<Value> = names
                        .iter()
                        .map(|name| alloc_path(heap, &vfs::join(&path, name)))
                        .collect();
                    let sid = heap.alloc_stream(items);
                    handled(Value::Object(Some(sid)))
                }
                _ => None,
            }
        }
        // `java.io.File` is real JDK bytecode; its file-system calls come back
        // here through `java/io/FileSystem` (see `unix_file_system`). The one
        // exception is `toPath`, which builds a `Path` the runtime models.
        "java/io/File" if method_name == "toPath" => {
            let path = file_path(heap, args.first()?)?;
            handled(alloc_path(heap, &path))
        }
        _ => None,
    }
}

/// An object with no state, for host services the runtime does not provide.
fn inert(heap: &mut Heap, class_name: &str) -> Value {
    Value::Object(Some(heap.alloc(JavaObject::Instance {
        class_name: class_name.into(),
        fields: HashMap::new(),
    })))
}
