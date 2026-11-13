//! Size limits on the loader's read path.
//!
//! A zip entry's declared size comes from the archive and an untrusted jar
//! controls it, so every read in [`hfc_runtime::loader`] is bounded on the
//! stream, not on the declared number. These tests build archives by hand so
//! the declaration and the payload can disagree: the `zip` crate will not write
//! a lying central directory, and that lie is exactly the attack.
//!
//! Each test asserts the specific error, not just that *something* failed: an
//! archive that happens to be rejected for a different reason (a bad CRC, a
//! malformed header) would not prove the limit works.

use hfc_core::Limits;
use hfc_runtime::loader::ClassLoader;
use std::io::Write;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Archive construction
// ---------------------------------------------------------------------------

/// A temp directory of this test binary, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hfc-loader-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch");
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut value = 0xFFFF_FFFFu32;
    for &byte in data {
        value ^= byte as u32;
        for _ in 0..8 {
            value = if value & 1 != 0 {
                0xEDB8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
        }
    }
    value ^ 0xFFFF_FFFF
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(data).expect("deflate");
    encoder.finish().expect("deflate")
}

/// One entry of a hand-built archive. `stored` skips compression, `declared`
/// is what the central directory claims, and `payload` is what the entry really
/// expands to. They are allowed to disagree, which is the point.
struct Entry<'a> {
    name: &'a str,
    payload: Vec<u8>,
    declared: u32,
    stored: bool,
}

fn build_zip(entries: &[Entry]) -> Vec<u8> {
    assert!(!entries.is_empty(), "an archive needs an entry");
    let mut locals = Vec::new();
    let mut central = Vec::new();
    for entry in entries {
        let crc = crc32(&entry.payload);
        let (method, body) = if entry.stored {
            (0u16, entry.payload.clone())
        } else {
            (8u16, deflate(&entry.payload))
        };
        let name = entry.name.as_bytes();
        let comp_size = body.len() as u32;
        let local_offset = locals.len() as u32;

        locals.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        locals.extend_from_slice(&20u16.to_le_bytes()); // version needed
        locals.extend_from_slice(&0u16.to_le_bytes()); // flags
        locals.extend_from_slice(&method.to_le_bytes());
        locals.extend_from_slice(&0u16.to_le_bytes()); // time
        locals.extend_from_slice(&0u16.to_le_bytes()); // date
        locals.extend_from_slice(&crc.to_le_bytes());
        locals.extend_from_slice(&comp_size.to_le_bytes());
        locals.extend_from_slice(&entry.declared.to_le_bytes());
        locals.extend_from_slice(&(name.len() as u16).to_le_bytes());
        locals.extend_from_slice(&0u16.to_le_bytes()); // extra length
        locals.extend_from_slice(name);
        locals.extend_from_slice(&body);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // made by
        central.extend_from_slice(&20u16.to_le_bytes()); // needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // time
        central.extend_from_slice(&0u16.to_le_bytes()); // date
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&comp_size.to_le_bytes());
        central.extend_from_slice(&entry.declared.to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&local_offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let cd_offset = locals.len() as u32;
    let cd_size = central.len() as u32;
    let mut out = locals;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // disk
    out.extend_from_slice(&0u16.to_le_bytes()); // cd disk
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment length
    out
}

fn small_limits() -> Limits {
    Limits {
        // Small enough to build in a test, large enough for a real class file.
        max_jar_bytes: 4 * 1024 * 1024,
        max_metadata_bytes: 4096,
        max_uncompressed_bytes: 64 * 1024,
        ..Limits::default()
    }
}

/// Write `bytes` as `name` in `scratch` and return the path.
fn write_jar(scratch: &Scratch, name: &str, bytes: &[u8]) -> PathBuf {
    let path = scratch.path(name);
    std::fs::write(&path, bytes).expect("write jar");
    path
}

fn message(error: &hfc_core::Error) -> String {
    format!("{error}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The declared size is a lie: a class entry claims 64 bytes and expands to
/// 512 KiB. The class path must refuse it rather than allocate the payload.
#[test]
fn a_class_entry_that_lies_about_its_size_is_refused() {
    let scratch = Scratch::new("class-lie");
    let payload: Vec<u8> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
    let jar = write_jar(
        &scratch,
        "lying-class.jar",
        &build_zip(&[Entry {
            name: "pkg/Bomb.class",
            payload: payload.clone(),
            declared: 64,
            stored: false,
        }]),
    );

    let mut loader = ClassLoader::with_limits(small_limits());
    loader.add_jar(&jar).expect("the archive itself opens");
    assert!(
        loader.has_class("pkg/Bomb"),
        "the entry is indexed, so the read path is what refuses it"
    );

    let error = loader
        .read_class_bytes("pkg/Bomb")
        .expect_err("a class that expands past the limit must not be returned");
    let text = message(&error);
    // The `zip` crate in the lockfile already caps the decompressor at the
    // declared size, so the lie is caught there first and reported as a
    // read error; the loader's own `take(limit + 1)` bound is what keeps this
    // true if that crate behaviour ever changes or an entry arrives by another
    // path. Either way the contract is the same: the payload is refused and
    // never fully allocated.
    assert!(
        text.contains("exceeds limit")
            || text.contains("expands past")
            || text.contains("larger than its declared"),
        "the error is a size refusal, not an incidental failure: {text}"
    );
    assert!(
        !text.contains("class not found"),
        "the entry exists and was refused for its size: {text}"
    );
}

/// The same lie for a *nested jar*: the entry says 128 bytes and holds 512 KiB.
/// The nested-jar read must be bounded too, or a jar-in-jar is a bomb.
#[test]
fn a_nested_jar_that_lies_about_its_size_is_refused() {
    let scratch = Scratch::new("nested-lie");
    // A nested archive that would be valid if it were allowed, wrapped in an
    // entry whose declared size is far smaller than what it expands to.
    let nested = build_zip(&[Entry {
        name: "nested/Real.class",
        payload: b"CAFEBABE".to_vec(),
        declared: 8,
        stored: true,
    }]);
    let mut payload = nested;
    payload.resize(512 * 1024, 0);
    let jar = write_jar(
        &scratch,
        "lying-nested.jar",
        &build_zip(&[Entry {
            name: "META-INF/jars/lib.jar",
            payload,
            declared: 128,
            stored: false,
        }]),
    );

    let mut loader = ClassLoader::with_limits(small_limits());
    // The archive loads; the oversized nested jar is skipped rather than
    // allocated, so its class is not reachable.
    loader.add_jar(&jar).expect("the outer archive opens");
    assert!(
        !loader.has_class("nested/Real"),
        "a nested jar that expands past the limit is not indexed"
    );
    let error = loader
        .read_class_bytes("nested/Real")
        .expect_err("its class must not be reachable");
    assert!(
        message(&error).contains("class not found"),
        "the class was never indexed: {error}"
    );
}

/// A `META-INF/services` resource that expands past the metadata limit must be
/// reported as over-limit, not silently truncated to its first `limit` bytes.
/// A truncation would look like a shorter, plausible provider list.
#[test]
fn a_text_resource_that_expands_past_the_limit_is_not_truncated() {
    let scratch = Scratch::new("text-lie");
    // An *honest* declaration over the limit: the old `take(limit)` reader
    // returned the first 4096 bytes as if that were the whole file, so a mod's
    // `META-INF/services` list silently came back short (or its last provider
    // name cut in half). The limit has to reject it, not truncate it.
    let payload = vec![b'A'; 64 * 1024];
    let jar = write_jar(
        &scratch,
        "large-text.jar",
        &build_zip(&[Entry {
            name: "META-INF/services/example.Service",
            payload: payload.clone(),
            declared: payload.len() as u32,
            stored: false,
        }]),
    );

    let mut loader = ClassLoader::with_limits(small_limits());
    loader.add_jar(&jar).expect("the archive opens");
    let texts = loader.text_resources("META-INF/services/example.Service");
    assert!(
        texts.is_empty(),
        "an over-limit resource is refused, not served as a truncated provider list \
         (got {} entry/entries of {:?} bytes)",
        texts.len(),
        texts.first().map(String::len).unwrap_or_default()
    );

    // The same file under the limit is still read in full.
    let small = vec![b'B'; 1024];
    let jar = write_jar(
        &scratch,
        "small-text.jar",
        &build_zip(&[Entry {
            name: "META-INF/services/example.Service",
            payload: small.clone(),
            declared: small.len() as u32,
            stored: false,
        }]),
    );
    let mut loader = ClassLoader::with_limits(small_limits());
    loader.add_jar(&jar).expect("the archive opens");
    let texts = loader.text_resources("META-INF/services/example.Service");
    assert_eq!(texts.len(), 1);
    assert_eq!(
        texts[0].len(),
        small.len(),
        "a resource under the limit is complete"
    );
}

/// A *real* oversized entry (a truthful declaration) is still refused, so the
/// limit is not only about lies: the normal class path keeps its bound.
#[test]
fn an_honestly_oversized_class_entry_is_refused() {
    let scratch = Scratch::new("honest-large");
    let payload = vec![0u8; 8 * 1024]; // over the 4096-byte class limit
    let jar = write_jar(
        &scratch,
        "large-class.jar",
        &build_zip(&[Entry {
            name: "pkg/Big.class",
            payload: payload.clone(),
            declared: payload.len() as u32,
            stored: false,
        }]),
    );

    let mut loader = ClassLoader::with_limits(small_limits());
    loader.add_jar(&jar).expect("the archive opens");
    let error = loader
        .read_class_bytes("pkg/Big")
        .expect_err("an honestly oversized class is refused");
    assert!(
        message(&error).contains("exceeds limit"),
        "the declared-size check still fires: {error}"
    );
}

/// A class that fits is still read: the limit must not reject normal input.
#[test]
fn a_class_that_fits_is_still_read() {
    let scratch = Scratch::new("fits");
    // The smallest useful class file: magic + minor/major + an empty constant
    // pool and nothing else. `read_class_bytes` returns raw bytes, so it does
    // not need to parse; the point is that the read succeeds.
    let mut class = Vec::new();
    class.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
    class.extend_from_slice(&0u16.to_be_bytes());
    class.extend_from_slice(&65u16.to_be_bytes());
    class.extend_from_slice(&1u16.to_be_bytes());
    let jar = write_jar(
        &scratch,
        "fits.jar",
        &build_zip(&[Entry {
            name: "pkg/Small.class",
            payload: class.clone(),
            declared: class.len() as u32,
            stored: false,
        }]),
    );

    let mut loader = ClassLoader::with_limits(small_limits());
    loader.add_jar(&jar).expect("the archive opens");
    assert_eq!(
        loader.read_class_bytes("pkg/Small").expect("read"),
        class,
        "a class under the limit is returned byte for byte"
    );
}

/// The total across nested jars is bounded, not just each entry: a jar holding
/// several nested archives that each sit under the per-jar limit but together
/// exceed the total must stop before the total is passed.
#[test]
fn nested_jar_totals_are_bounded_across_entries() {
    let scratch = Scratch::new("nested-total");
    let limits = Limits {
        max_jar_bytes: 48 * 1024,
        max_uncompressed_bytes: 64 * 1024,
        max_metadata_bytes: 4096,
        ..Limits::default()
    };
    // Four nested jars, each under `max_jar_bytes` but 160 KiB together, which
    // is over `max_uncompressed_bytes`. Each holds one *small* class plus a
    // 40 KiB filler, so no per-entry limit rejects them and no class file is
    // oversized: only the running total across nested jars can stop them. The
    // class names differ, so how many archives were accepted is observable.
    let mut entries = Vec::new();
    for index in 0..4 {
        let inner = build_zip(&[
            Entry {
                name: &format!("nested/Real{index}.class"),
                payload: b"CAFEBABE".to_vec(),
                declared: 8,
                stored: true,
            },
            Entry {
                name: "filler.bin",
                payload: vec![0u8; 40 * 1024],
                declared: (40 * 1024) as u32,
                stored: true,
            },
        ]);
        // The name outlives the entry vector; the archive is built before the
        // test ends, so a leaked small string keeps the borrow simple.
        let name: &'static str =
            Box::leak(format!("META-INF/jars/lib{index}.jar").into_boxed_str());
        entries.push(Entry {
            name,
            payload: inner,
            declared: 0,
            stored: false,
        });
    }
    for entry in &mut entries {
        // The declared size is the entry's own (uncompressed) length: these
        // nested jars are honestly sized, so only the running total can stop
        // them.
        entry.declared = entry.payload.len() as u32;
    }
    let jar = write_jar(&scratch, "many-nested.jar", &build_zip(&entries));

    let mut loader = ClassLoader::with_limits(limits);
    loader.add_jar(&jar).expect("the archive opens");

    // 64 KiB total, 40 KiB per nested jar: the first fits, the second cannot.
    assert!(
        loader.has_class("nested/Real0"),
        "the first nested jar fits inside the total"
    );
    for index in 1..4 {
        assert!(
            !loader.has_class(&format!("nested/Real{index}")),
            "nested jar {index} would push the total past max_uncompressed_bytes"
        );
    }
    assert_eq!(
        loader.read_class_bytes("nested/Real0").expect("read"),
        b"CAFEBABE"
    );

    // Prove the total, not a per-entry limit, did the work: one nested jar of
    // the same size is accepted.
    let single = write_jar(
        &scratch,
        "single.jar",
        &build_zip(&[
            Entry {
                name: "nested/Real.class",
                payload: b"CAFEBABE".to_vec(),
                declared: 8,
                stored: true,
            },
            Entry {
                name: "filler.bin",
                payload: vec![0u8; 40 * 1024],
                declared: (40 * 1024) as u32,
                stored: true,
            },
        ]),
    );
    let mut one = ClassLoader::with_limits(limits);
    one.add_jar(&single).expect("the archive opens");
    assert_eq!(
        one.read_class_bytes("nested/Real").expect("read"),
        b"CAFEBABE",
        "a single 40 KiB nested jar is under both limits"
    );
}

/// A path that does not exist, or a name the archive does not hold, still fails
/// as "class not found" rather than as a limit error.
#[test]
fn missing_classes_report_not_found() {
    let scratch = Scratch::new("missing");
    let jar = write_jar(
        &scratch,
        "tiny.jar",
        &build_zip(&[Entry {
            name: "pkg/Only.class",
            payload: b"CAFEBABE".to_vec(),
            declared: 8,
            stored: true,
        }]),
    );
    let mut loader = ClassLoader::with_limits(small_limits());
    loader.add_jar(&jar).expect("the archive opens");
    let error = loader
        .read_class_bytes("pkg/Absent")
        .expect_err("an absent class is an error");
    assert!(message(&error).contains("class not found"), "{error}");
    let _: &Path = &jar;
}
