//! Reader for the JDK's `lib/modules` image.
//!
//! The image is the JDK's own class library: `java.lang.Object`,
//! `java.util.AbstractMap`, `java.lang.Class`, and every other class the
//! runtime would otherwise have to reimplement by hand. Reading it means the
//! interpreter runs the same bytecode a JVM would, instead of a hand-written
//! table of shims.
//!
//! The format is the OpenJDK `.jimage` container: a 28-byte little-endian
//! header, a redirect table (the perfect hash), an offsets table, a variable
//! length location table, and a string table. Resources after the index may be
//! stored uncompressed, deflated ("zip"), or constant-pool-shared
//! ("compact-cp").

use hfc_core::{Error, Result};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAGIC: i32 = 0xCAFE_DADA_u32 as i32;
const HEADER_SLOTS: usize = 7;
const HEADER_SIZE: usize = HEADER_SLOTS * 4;

/// A class file inside a JDK image.
#[derive(Debug, Clone, Copy)]
struct Resource {
    content_offset: u64,
    compressed_size: u64,
    uncompressed_size: u64,
    from_java_base: bool,
}

/// Maps class names (`java/lang/Object`) to their bytes in a JDK image.
pub struct JImage {
    path: PathBuf,
    index_size: u64,
    resources: HashMap<String, Resource>,
    locations: Vec<u8>,
    strings: Vec<u8>,
}

impl JImage {
    /// Open a `lib/modules` image and index every class it contains.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;

        let mut header = [0u8; HEADER_SIZE];
        file.read_exact(&mut header)
            .map_err(|e| Error::io(path, e))?;
        let magic = i32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        if magic != MAGIC {
            return Err(Error::new(
                path.display().to_string(),
                "not a jimage file (bad magic)",
            ));
        }
        let version = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        if version >> 16 != 1 {
            return Err(Error::new(
                path.display().to_string(),
                format!("unsupported jimage major version {}", version >> 16),
            ));
        }
        let resource_count = read_u32(&header, 12) as usize;
        let table_length = read_u32(&header, 16) as usize;
        let locations_size = read_u32(&header, 20) as usize;
        let strings_size = read_u32(&header, 24) as usize;

        let redirect_size = table_length * 4;
        let offsets_size = table_length * 4;
        let index_size = HEADER_SIZE + redirect_size + offsets_size + locations_size + strings_size;

        let mut index = vec![0u8; index_size - HEADER_SIZE];
        file.read_exact(&mut index)
            .map_err(|e| Error::io(path, e))?;

        let offsets_start = redirect_size;
        let locations_start = offsets_start + offsets_size;
        let strings_start = locations_start + locations_size;
        let locations = index[locations_start..strings_start].to_vec();
        let strings = index[strings_start..].to_vec();

        let mut resources: HashMap<String, Resource> = HashMap::new();
        for i in 0..resource_count {
            let slot = offsets_start + i * 4;
            let Some(offset) = index.get(slot..slot + 4) else {
                break;
            };
            let offset = u32::from_le_bytes([offset[0], offset[1], offset[2], offset[3]]) as usize;
            if offset == 0 {
                continue;
            }
            let Some(attrs) = read_attributes(&locations, offset) else {
                continue;
            };
            let extension = string_at(&strings, attrs[4] as usize);
            if extension != "class" {
                continue;
            }
            let parent = string_at(&strings, attrs[2] as usize);
            let base = string_at(&strings, attrs[3] as usize);
            let name = if parent.is_empty() {
                base
            } else {
                format!("{parent}/{base}")
            };
            // Modules can contain the same class path (split packages). Prefer
            // java.base, which owns the classes every mod relies on.
            let module = string_at(&strings, attrs[1] as usize);
            let from_java_base = module == "java.base";
            let resource = Resource {
                content_offset: attrs[5],
                compressed_size: attrs[6],
                uncompressed_size: attrs[7],
                from_java_base,
            };
            match resources.get_mut(&name) {
                Some(existing) if existing.from_java_base || !from_java_base => {}
                _ => {
                    resources.insert(name, resource);
                }
            }
        }

        Ok(Self {
            path: path.to_path_buf(),
            index_size: index_size as u64,
            resources,
            locations,
            strings,
        })
    }

    /// Whether the image contains a class.
    pub fn contains_class(&self, name: &str) -> bool {
        self.resources.contains_key(name)
    }

    /// Read and decompress a class file.
    pub fn class_bytes(&self, name: &str) -> Result<Vec<u8>> {
        let resource = self
            .resources
            .get(name)
            .ok_or_else(|| Error::new("jimage", format!("class not found: {name}")))?;
        let mut file = std::fs::File::open(&self.path).map_err(|e| Error::io(&self.path, e))?;

        let size = if resource.compressed_size == 0 {
            resource.uncompressed_size
        } else {
            resource.compressed_size
        };
        if size > 64 * 1024 * 1024 {
            return Err(Error::new("jimage", format!("class too large: {name}")));
        }
        let mut bytes = vec![0u8; size as usize];
        read_at(
            &mut file,
            &mut bytes,
            self.index_size + resource.content_offset,
        )
        .map_err(|e| Error::io(&self.path, e))?;

        let mut bytes = if resource.compressed_size == 0 {
            bytes
        } else {
            self.decompress(&mut bytes)?
        };

        // A compressed resource may nest another compression layer.
        while bytes.len() >= 29 {
            let magic = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            if magic != 0xCAFE_FAFA_u32 as i32 {
                break;
            }
            bytes = self.decompress(&mut bytes)?;
        }

        Ok(bytes)
    }

    fn decompress(&self, bytes: &mut [u8]) -> Result<Vec<u8>> {
        if bytes.len() < 29 {
            return Err(Error::new("jimage", "truncated compressed resource"));
        }
        let compressed_size = u64::from_le_bytes(bytes[4..12].try_into().unwrap());
        let uncompressed_size = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
        let plugin_offset = i32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
        let _content_offset = i32::from_le_bytes(bytes[24..28].try_into().unwrap());
        let plugin = string_at(&self.strings, plugin_offset);
        let body = &bytes[29..];

        if compressed_size < 29 {
            return Err(Error::new("jimage", "invalid compressed resource size"));
        }

        match plugin.as_str() {
            "zip" => inflate(body, uncompressed_size as usize),
            "compact-cp" => Ok(normalize_constant_pool(&self.strings, body)),
            other => Err(Error::new(
                "jimage",
                format!("unsupported jimage compressor: {other:?}"),
            )),
        }
    }

    /// Expose the location table for diagnostics and tests.
    pub fn location_size(&self) -> usize {
        self.locations.len()
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_at(file: &mut std::fs::File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(buf)
}

/// Decode a variable length location entry into its eight attributes.
fn read_attributes(locations: &[u8], mut offset: usize) -> Option<[u64; 8]> {
    let mut attributes = [0u64; 8];
    loop {
        let data = *locations.get(offset)?;
        offset += 1;
        if data <= 0x7 {
            break;
        }
        let kind = (data >> 3) as usize;
        if kind >= attributes.len() {
            return None;
        }
        let length = (data & 0x7) as usize + 1;
        let mut value = 0u64;
        for _ in 0..length {
            value = (value << 8) | *locations.get(offset)? as u64;
            offset += 1;
        }
        attributes[kind] = value;
    }
    Some(attributes)
}

/// Read a NUL-terminated string from the image's string table.
fn string_at(strings: &[u8], offset: usize) -> String {
    if offset >= strings.len() {
        return String::new();
    }
    let end = strings[offset..]
        .iter()
        .position(|&b| b == 0)
        .map(|p| offset + p)
        .unwrap_or(strings.len());
    String::from_utf8_lossy(&strings[offset..end]).into_owned()
}

fn inflate(body: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    let mut decoder = flate2::read::ZlibDecoder::new(body);
    // jimage's "zip" plugin uses raw DEFLATE through `Inflater`; accept either
    // framing by retrying raw input when the zlib header is not present.
    match decoder.read_to_end(&mut out) {
        Ok(_) => Ok(out),
        Err(_) => {
            let mut out = Vec::with_capacity(expected);
            let mut decoder = flate2::read::DeflateDecoder::new(body);
            decoder
                .read_to_end(&mut out)
                .map_err(|e| Error::new("jimage", format!("inflate failed: {e}")))?;
            Ok(out)
        }
    }
}

// ---------------------------------------------------------------------------
// compact-cp: the JDK rewrites constant-pool UTF-8 entries into references to
// the image's string table. Reconstruct a normal class file before parsing.
// ---------------------------------------------------------------------------

const CONSTANT_UTF8: u8 = 1;
const CONSTANT_INTEGER: u8 = 3;
const CONSTANT_FLOAT: u8 = 4;
const CONSTANT_LONG: u8 = 5;
const CONSTANT_DOUBLE: u8 = 6;
const CONSTANT_CLASS: u8 = 7;
const CONSTANT_STRING: u8 = 8;
const CONSTANT_FIELDREF: u8 = 9;
const CONSTANT_METHODREF: u8 = 10;
const CONSTANT_INTERFACE_METHODREF: u8 = 11;
const CONSTANT_NAME_AND_TYPE: u8 = 12;
const CONSTANT_METHOD_HANDLE: u8 = 15;
const CONSTANT_METHOD_TYPE: u8 = 16;
const CONSTANT_DYNAMIC: u8 = 17;
const CONSTANT_INVOKE_DYNAMIC: u8 = 18;
const CONSTANT_MODULE: u8 = 19;
const CONSTANT_PACKAGE: u8 = 20;
const EXTERNALIZED_STRING: u8 = 23;
const EXTERNALIZED_STRING_DESCRIPTOR: u8 = 25;

fn constant_size(tag: u8) -> Option<usize> {
    Some(match tag {
        CONSTANT_INTEGER | CONSTANT_FLOAT => 4,
        CONSTANT_LONG | CONSTANT_DOUBLE => 8,
        CONSTANT_CLASS | CONSTANT_STRING | CONSTANT_METHOD_TYPE | CONSTANT_MODULE
        | CONSTANT_PACKAGE => 2,
        CONSTANT_FIELDREF
        | CONSTANT_METHODREF
        | CONSTANT_INTERFACE_METHODREF
        | CONSTANT_NAME_AND_TYPE
        | CONSTANT_DYNAMIC
        | CONSTANT_INVOKE_DYNAMIC => 4,
        CONSTANT_METHOD_HANDLE => 3,
        _ => return None,
    })
}

fn normalize_constant_pool(strings: &[u8], transformed: &[u8]) -> Vec<u8> {
    let mut input = transformed;
    let mut out = Vec::with_capacity(transformed.len());
    if input.len() < 10 {
        return transformed.to_vec();
    }
    out.extend_from_slice(&input[..8]);
    let count = u16::from_be_bytes([input[8], input[9]]) as usize;
    out.extend_from_slice(&input[8..10]);
    input = &input[10..];

    let mut i = 1;
    while i < count {
        let Some((&tag, rest)) = input.split_first() else {
            break;
        };
        input = rest;
        i += 1;
        match tag {
            CONSTANT_UTF8 => {
                let Some(len) = read_u16_be(input) else {
                    break;
                };
                let total = 2 + len;
                if input.len() < total {
                    break;
                }
                out.push(tag);
                out.extend_from_slice(&input[..total]);
                input = &input[total..];
            }
            EXTERNALIZED_STRING => {
                let Some((index, used)) = read_compressed_index(input) else {
                    break;
                };
                input = &input[used..];
                let text = string_at(strings, index);
                out.push(CONSTANT_UTF8);
                write_utf8(&mut out, &text);
            }
            EXTERNALIZED_STRING_DESCRIPTOR => {
                let Some((desc_index, used)) = read_compressed_index(input) else {
                    break;
                };
                input = &input[used..];
                let Some(indexes_length) = read_compressed_index(input) else {
                    break;
                };
                input = &input[indexes_length.1..];
                if input.len() < indexes_length.0 {
                    break;
                }
                let indexes = decompress_flow(&input[..indexes_length.0]);
                input = &input[indexes_length.0..];
                let descriptor = string_at(strings, desc_index);
                let text = reconstruct_descriptor(strings, &descriptor, &indexes);
                out.push(CONSTANT_UTF8);
                write_utf8(&mut out, &text);
            }
            _ => {
                let Some(size) = constant_size(tag) else {
                    break;
                };
                out.push(tag);
                if input.len() < size {
                    break;
                }
                out.extend_from_slice(&input[..size]);
                input = &input[size..];
                if tag == CONSTANT_LONG || tag == CONSTANT_DOUBLE {
                    // Eight-byte constants occupy two constant pool slots.
                    i += 1;
                }
            }
        }
    }
    out.extend_from_slice(input);
    out
}

fn read_u16_be(bytes: &[u8]) -> Option<usize> {
    Some(u16::from_be_bytes([*bytes.first()?, *bytes.get(1)?]) as usize)
}

fn write_utf8(out: &mut Vec<u8>, text: &str) {
    let bytes = text.as_bytes();
    let len = bytes.len().min(u16::MAX as usize);
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.extend_from_slice(&bytes[..len]);
}

/// `CompressIndexes.readInt`: either a 4-byte big-endian value (high bit
/// clear) or a 1..4 byte value whose first byte stores the length.
fn read_compressed_index(bytes: &[u8]) -> Option<(usize, usize)> {
    let header = *bytes.first()?;
    let compressed = header & 0x80 != 0;
    let length = if compressed {
        ((header >> 5) & 0x3) as usize
    } else {
        4
    };
    let mut value = if compressed {
        (header & 0x1F) as usize
    } else {
        header as usize
    };
    for i in 1..length {
        value = (value << 8) | *bytes.get(i)? as usize;
    }
    Some((value, length))
}

fn decompress_flow(bytes: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let Some((value, used)) = read_compressed_index(&bytes[offset..]) else {
            break;
        };
        out.push(value);
        offset += used;
    }
    out
}

/// `StringSharingDecompressor.reconstruct`: substitute `Lpkg/Class;` template
/// parameters with the shared strings.
fn reconstruct_descriptor(strings: &[u8], descriptor: &str, indexes: &[usize]) -> String {
    let mut out = Vec::new();
    let mut arg = 0;
    for &c in descriptor.as_bytes() {
        if c == b'L' {
            out.push(c);
            if arg + 1 < indexes.len() {
                let package = string_at(strings, indexes[arg]);
                arg += 1;
                if !package.is_empty() {
                    out.extend_from_slice(package.as_bytes());
                    out.push(b'/');
                }
                let class = string_at(strings, indexes[arg]);
                arg += 1;
                out.extend_from_slice(class.as_bytes());
            }
        } else {
            out.push(c);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
