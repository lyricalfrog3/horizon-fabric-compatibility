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

/// The most one class resource may decompress to. A class file is never this
/// large; the cap bounds what a hostile local image can ask for.
const MAX_RESOURCE_BYTES: u64 = 64 * 1024 * 1024;

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
    /// The `lib/modules` file this image was read from.
    pub fn path(&self) -> &Path {
        &self.path
    }

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

        let redirect_size = (table_length as u64) * 4;
        let offsets_size = (table_length as u64) * 4;
        // Every length below comes from the file's own header, so it is
        // attacker-controlled when the image is not the JDK's. Bound the index
        // against what the file actually holds before allocating: a header
        // claiming gigabytes must fail here, not by asking for gigabytes.
        let index_size = (HEADER_SIZE as u64)
            .checked_add(redirect_size)
            .and_then(|n| n.checked_add(offsets_size))
            .and_then(|n| n.checked_add(locations_size as u64))
            .and_then(|n| n.checked_add(strings_size as u64))
            .ok_or_else(|| {
                Error::new(
                    path.display().to_string(),
                    "index size overflows: not a valid jimage",
                )
            })?;
        let file_len = file.metadata().map_err(|e| Error::io(path, e))?.len();
        if index_size > file_len {
            return Err(Error::new(
                path.display().to_string(),
                format!(
                    "index size {index_size} exceeds the {file_len} byte file; refusing to allocate"
                ),
            ));
        }
        let index_size = index_size as usize;

        let mut index = vec![0u8; index_size - HEADER_SIZE];
        file.read_exact(&mut index)
            .map_err(|e| Error::io(path, e))?;

        let redirect_size = redirect_size as usize;
        let offsets_size = offsets_size as usize;

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
        if size > MAX_RESOURCE_BYTES {
            return Err(Error::new("jimage", format!("class too large: {name}")));
        }
        // The offset and size come from the location table, so they are only as
        // trustworthy as the file. Require the whole resource to be inside it
        // before allocating for it.
        let file_len = file.metadata().map_err(|e| Error::io(&self.path, e))?.len();
        let start = self
            .index_size
            .checked_add(resource.content_offset)
            .ok_or_else(|| Error::new("jimage", format!("resource offset overflows: {name}")))?;
        let end = start
            .checked_add(size)
            .ok_or_else(|| Error::new("jimage", format!("resource size overflows: {name}")))?;
        if end > file_len {
            return Err(Error::new(
                "jimage",
                format!("resource {name} extends past the end of the image"),
            ));
        }
        let mut bytes = vec![0u8; size as usize];
        read_at(&mut file, &mut bytes, start).map_err(|e| Error::io(&self.path, e))?;

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
            "zip" => {
                if uncompressed_size > MAX_RESOURCE_BYTES {
                    return Err(Error::new(
                        "jimage",
                        format!("compressed resource expands past {MAX_RESOURCE_BYTES} bytes"),
                    ));
                }
                inflate(body, uncompressed_size as usize)
            }
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
    // `expected` is the declared uncompressed size, which the stream does not
    // have to honour: read one byte past it and fail if the stream really is
    // larger, so a lying declaration cannot expand without bound.
    let limit = expected as u64;
    let too_large = || {
        Error::new(
            "jimage",
            format!("compressed resource expands past its declared {limit} bytes"),
        )
    };
    let mut out = Vec::with_capacity(expected);
    let mut decoder = flate2::read::ZlibDecoder::new(body);
    // jimage's "zip" plugin uses raw DEFLATE through `Inflater`; accept either
    // framing by retrying raw input when the zlib header is not present.
    match decoder
        .by_ref()
        .take(limit.saturating_add(1))
        .read_to_end(&mut out)
    {
        Ok(_) if out.len() as u64 > limit => Err(too_large()),
        Ok(_) => Ok(out),
        Err(_) => {
            let mut out = Vec::with_capacity(expected);
            let mut decoder = flate2::read::DeflateDecoder::new(body);
            decoder
                .take(limit.saturating_add(1))
                .read_to_end(&mut out)
                .map_err(|e| Error::new("jimage", format!("inflate failed: {e}")))?;
            if out.len() as u64 > limit {
                return Err(too_large());
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A header claiming a huge index must fail against the file length instead
    /// of asking the allocator for it.
    #[test]
    fn a_header_that_claims_a_huge_index_is_refused() {
        let dir = std::env::temp_dir().join(format!("hfc-jimage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("modules");
        // The 28-byte header: magic, version, a reserved word, the resource
        // count, and the three index lengths. The table length is the lie.
        let mut header = Vec::new();
        header.extend_from_slice(&(MAGIC as u32).to_le_bytes());
        header.extend_from_slice(&(1u32 << 16).to_le_bytes()); // version 1
        header.extend_from_slice(&0u32.to_le_bytes()); // reserved
        header.extend_from_slice(&0u32.to_le_bytes()); // resource count
        header.extend_from_slice(&u32::MAX.to_le_bytes()); // table length: huge
        header.extend_from_slice(&u32::MAX.to_le_bytes()); // locations
        header.extend_from_slice(&u32::MAX.to_le_bytes()); // strings
        assert_eq!(header.len(), HEADER_SIZE);
        std::fs::write(&path, &header).unwrap();

        let error = match JImage::open(&path) {
            Ok(_) => panic!("a bogus index size is refused"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("index size"),
            "the error names the index: {error}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `inflate` stops at the declared size, so a stream that expands past it
    /// fails rather than running to its own end.
    #[test]
    fn inflate_refuses_a_stream_past_its_declared_size() {
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut encoder, &vec![0u8; 4096]).unwrap();
        let compressed = encoder.finish().unwrap();
        let error = inflate(&compressed, 16).expect_err("the stream is larger than declared");
        assert!(error.to_string().contains("expands past"), "{error}");
        // The same stream with an honest size inflates.
        let ok = inflate(&compressed, 4096).expect("an honest size works");
        assert_eq!(ok.len(), 4096);
    }
}
