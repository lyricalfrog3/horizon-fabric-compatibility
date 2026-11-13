#![forbid(unsafe_code)]
//! Shared types for Horizon Fabric Compatibility (HFC).
//!
//! This crate holds no mod logic: it defines the compatibility API version,
//! the resource limits applied to untrusted archives, and small filesystem and
//! hashing helpers shared by the scanner, the cache and the bootstrap.

use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::{
    fmt,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// The compatibility layer's API line. Generated ports record it so a later
/// HFC release can tell which generation of a port it is looking at.
pub const COMPAT_API: &str = "0.1";

/// The version of this build of HFC.
pub const HFC_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The pipeline version: bump when the metadata, carry or install formats
/// change in a way that invalidates a cached conversion. It is part of every
/// cache key, so old entries simply stop matching.
pub const PIPELINE_VERSION: u32 = 1;

/// The directory the compatibility installation lives in, beside the ordinary
/// mods of the game folder.
pub const COMPAT_DIR: &str = "HorizonFabricCompat";

/// The marker file that makes a folder a compatibility installation.
pub const MARKER: &str = "hfc.toml";

/// The state file HFC owns inside [`COMPAT_DIR`].
pub const STATE: &str = "state.json";

/// The machine-readable report inside [`COMPAT_DIR`].
pub const REPORT_JSON: &str = "report.json";

/// The human-readable report inside [`COMPAT_DIR`].
pub const REPORT_MD: &str = "report.md";

/// The hidden marker written into every generated folder mod. HFC only ever
/// removes or replaces a mod directory carrying it.
pub const GENERATED_MARKER: &str = ".hfc-generated.json";

/// A conservative cap on a single Fabric jar. Real mods are far smaller; the
/// cap exists so that an untrusted file cannot make the scanner allocate
/// without bound.
pub const MAX_JAR_BYTES: u64 = 512 * 1024 * 1024;

/// The most entries an archive may hold.
pub const MAX_ENTRIES: usize = 65_536;

/// The most bytes one metadata file (`fabric.mod.json`, a mixin config) may
/// expand to. Metadata is small; anything larger is not metadata.
pub const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;

/// The most bytes all entries of one archive may expand to.
pub const MAX_UNCOMPRESSED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// How many archive levels are read, including the top-level jar. One means
/// declared nested jars are read; their own nested declarations are reported
/// but not followed.
pub const MAX_NESTED_DEPTH: u8 = 1;

/// How many nested jars one archive may declare.
pub const MAX_NESTED_JARS: usize = 256;

/// The longest archive path accepted, in bytes.
pub const MAX_PATH_BYTES: usize = 1024;

/// The limits applied to an untrusted archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub max_jar_bytes: u64,
    pub max_entries: usize,
    pub max_metadata_bytes: u64,
    pub max_uncompressed_bytes: u64,
    pub max_nested_depth: u8,
    pub max_nested_jars: usize,
    pub max_path_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_jar_bytes: MAX_JAR_BYTES,
            max_entries: MAX_ENTRIES,
            max_metadata_bytes: MAX_METADATA_BYTES,
            max_uncompressed_bytes: MAX_UNCOMPRESSED_BYTES,
            max_nested_depth: MAX_NESTED_DEPTH,
            max_nested_jars: MAX_NESTED_JARS,
            max_path_bytes: MAX_PATH_BYTES,
        }
    }
}

/// A structured error with a human-readable context. The compatibility layer
/// deals with untrusted files, so every error names the file or path it came
/// from and never panics on malformed input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub context: String,
    pub message: String,
}

impl Error {
    pub fn new(context: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            context: context.into(),
            message: message.into(),
        }
    }

    pub fn io(path: &Path, error: io::Error) -> Self {
        Self::new(path.display().to_string(), error.to_string())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.context.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.context, self.message)
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// The SHA-256 of a file, read in bounded chunks.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|e| Error::io(path, e))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).map_err(|e| Error::io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex(hasher.finalize().as_slice()))
}

/// The SHA-256 of bytes.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hex(hasher.finalize().as_slice())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The target platform as it appears in cache keys.
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The prefix every temporary file of [`atomic_write`] carries, so a stale one
/// from a killed process can be recognised and swept.
pub const PARTIAL_PREFIX: &str = ".hfc-part-";

/// Distinguishes concurrent writers inside one process. The process id alone is
/// not enough: two threads writing the same destination would otherwise pick
/// the same name.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Deletes its temporary file unless the write completed, so a failed or
/// unwinding write leaves nothing behind.
struct TempGuard(Option<PathBuf>);

impl TempGuard {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    fn keep(&mut self) {
        self.0 = None;
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

/// Write a file atomically: a uniquely named sibling temporary file, flushed,
/// then renamed over the destination, then the parent directory synced so the
/// rename survives a crash. An interrupted write leaves the old file intact and
/// (via [`TempGuard`]) no partial file behind.
///
/// Two writers to the same path in the same process do not collide: each gets
/// its own temporary name, and the final content is whichever rename ran last,
/// never a mixture.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = parent_dir(path);
    fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(
        ".{name}{PARTIAL_PREFIX}{}-{sequence}",
        std::process::id()
    ));
    let mut guard = TempGuard::new(tmp.clone());
    {
        let mut file = File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
        file.write_all(bytes).map_err(|e| Error::io(&tmp, e))?;
        file.sync_all().map_err(|e| Error::io(&tmp, e))?;
    }
    fs::rename(&tmp, path).map_err(|e| Error::io(path, e))?;
    guard.keep();
    sync_parent(parent)?;
    Ok(())
}

/// The directory holding `path`. `Path::new("a.json").parent()` is `Some("")`,
/// not `None`, and an empty path cannot be opened, so both cases mean the
/// current directory.
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Make the rename durable. Without this an unflushed directory entry can be
/// lost even though the file contents were synced.
#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<()> {
    let dir = File::open(parent).map_err(|e| Error::io(parent, e))?;
    dir.sync_all().map_err(|e| Error::io(parent, e))
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<()> {
    Ok(())
}

/// Write JSON atomically.
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| Error::new(path.display().to_string(), e.to_string()))?;
    atomic_write(path, &bytes)
}

/// Whether a path component is a safe, relative archive path.
pub fn safe_archive_path(name: &str, max_bytes: usize) -> bool {
    if name.is_empty() || name.len() > max_bytes {
        return false;
    }
    if name.starts_with('/') || name.contains(['\\', ':', '\0']) {
        return false;
    }
    name.split('/')
        .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Whether `name` is a plausible plain file name HFC may install or remove,
/// not a path escaping the mods folder.
pub fn safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.contains(['/', '\\', ':', '\0'])
        && name != "."
        && name != ".."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_paths_cannot_escape() {
        for bad in [
            "",
            "/etc/passwd",
            "a/../../b",
            "../x",
            "a//b",
            "a\\b",
            "C:/x",
            "a\0b",
        ] {
            assert!(!safe_archive_path(bad, 100), "{bad:?}");
        }
        assert!(safe_archive_path("META-INF/jars/lib.jar", 100));
        assert!(safe_archive_path("fabric.mod.json", 100));
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_part() {
        let dir = std::env::temp_dir().join(format!("hfc-atomic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("value.json");
        atomic_write_json(&path, &serde_json::json!({"a": 1})).unwrap();
        atomic_write_json(&path, &serde_json::json!({"a": 2})).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"a\": 2"));
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(PARTIAL_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_write_removes_its_temporary_file() {
        let dir = std::env::temp_dir().join(format!(
            "hfc-atomic-fail-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // A directory at the destination makes the final rename fail after the
        // temporary file was written.
        let path = dir.join("occupied");
        fs::create_dir_all(&path).unwrap();
        assert!(atomic_write(&path, b"data").is_err());
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(PARTIAL_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bare_file_name_resolves_to_the_current_directory() {
        // `Path::new("x").parent()` is `Some("")`, not `None`; an empty parent
        // must mean ".", never an unopenable path.
        assert_eq!(parent_dir(Path::new("bare.json")), Path::new("."));
        assert_eq!(parent_dir(Path::new("dir/file.json")), Path::new("dir"));
        assert_eq!(parent_dir(Path::new("/abs/file.json")), Path::new("/abs"));
    }

    #[test]
    fn a_bare_file_name_in_the_current_directory_works() {
        // The same case end to end: the write and its parent fsync must
        // succeed with only a file name. The guard restores the process's
        // directory even if the write panics, so other tests are unaffected
        // (they all use absolute temp paths).
        struct RestoreDir(PathBuf);
        impl Drop for RestoreDir {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.0);
            }
        }
        let dir = std::env::temp_dir().join(format!("hfc-atomic-cwd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let _guard = RestoreDir(std::env::current_dir().unwrap());
        std::env::set_current_dir(&dir).unwrap();
        atomic_write(Path::new("bare.json"), b"ok").unwrap();
        assert_eq!(fs::read(dir.join("bare.json")).unwrap(), b"ok");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_writers_do_not_collide() {
        let dir = std::env::temp_dir().join(format!("hfc-atomic-race-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shared.json");

        // Two distinct contents, one per writer. Each thread writes its own
        // payload many times; every write must succeed and the surviving file
        // must be exactly one of the two payloads, never a mixture.
        let payload_a =
            serde_json::to_vec_pretty(&serde_json::json!({"writer": "a", "n": 1})).unwrap();
        let payload_b =
            serde_json::to_vec_pretty(&serde_json::json!({"writer": "b", "n": 2})).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for payload in [payload_a.clone(), payload_b.clone()] {
            let path = path.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..50 {
                    atomic_write(&path, &payload).expect("concurrent atomic_write failed");
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let final_bytes = fs::read(&path).unwrap();
        assert!(
            final_bytes == payload_a || final_bytes == payload_b,
            "final file is not one of the two payloads: {:?}",
            String::from_utf8_lossy(&final_bytes)
        );
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(PARTIAL_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hashes_are_stable() {
        assert_eq!(
            sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
