//! Block facts cached per classpath, so a later start does not read them again.
//!
//! `collect_block_facts` runs the mod's own block objects and Minecraft's real
//! `BlockState` methods for every state of every registered block, and reading
//! a block's shapes builds `VoxelShape`s with Minecraft's own bytecode. Both
//! are pure reads of objects whose existence is already paid for by the
//! entrypoints, so their result can be kept from one start to the next: mdm's
//! 344 blocks cost about 5 s of facts and 13 s of shapes, and none of it
//! changes while the classpath does not.
//!
//! The key is the SHA-256 of every JAR the interpreter loads classes from (the
//! client jar, every library, every mod jar — a library change can change a
//! mod's facts), the identity of the JDK image, the version of this crate and
//! [`BLOCK_FACTS_CACHE_VERSION`]. Anything that does not read back exactly
//! (missing file, other key, other version, a field that will not parse) is a
//! miss: the facts are then read from the mod again and the cache rewritten.
//!
//! What the cache does **not** replace is the block objects themselves: `C1`
//! placement and `C2`/`C3` use/container reads run the mod's real code at play
//! time, on the `Block` instances the mod made in its entrypoint. A cache hit
//! therefore still runs the entrypoints; it only skips reading the facts back
//! out of the objects they built.
//!
//! The file lives in the game folder's `.horizon-cache/`, beside the generated
//! content mods, and is written atomically (`hfc_core::atomic_write`).

use super::FabricBridge;
use super::RegisteredBlock;
use hfc_core::atomic_write;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Bumped when the facts a start reads out of a block change meaning: a field
/// added to [`RegisteredBlock`] or its states, or a different way of reading
/// them in `block_builtins.rs`. A cache from another version is a miss.
pub const BLOCK_FACTS_CACHE_VERSION: u32 = 1;

/// `fabric-blocks` under the game folder's `.horizon-cache/`.
pub const CACHE_DIR: &str = "fabric-blocks";

/// What a start knows about where the facts came from.
#[derive(Debug)]
pub(crate) struct BlockFactsCache {
    /// The directory the facts are cached in, `None` to keep them in memory.
    dir: Option<PathBuf>,
    /// The key of the classpath the facts belong to, once it was computed.
    key: Option<String>,
    /// The facts in the heap were read back from the cache, not from blocks.
    from_cache: bool,
}

impl BlockFactsCache {
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            key: None,
            from_cache: false,
        }
    }
}

/// The facts file for one key.
fn facts_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("facts-{key}.json"))
}

/// One cache file: the whole facts list of one classpath.
#[derive(Serialize, Deserialize)]
struct CachedFacts {
    /// [`BLOCK_FACTS_CACHE_VERSION`].
    version: u32,
    /// The classpath key the facts were read for.
    key: String,
    /// The version of this crate that read them.
    runtime: String,
    /// Whether the shapes were read before the file was written. Facts written
    /// without shapes are valid facts, but a start that finds them still has to
    /// read the shapes; it must not believe it has them.
    #[serde(default)]
    shapes: bool,
    blocks: Vec<RegisteredBlock>,
}

/// The `(len, mtime)` identity of a JDK image: hashing 66 MB every start would
/// cost more than the facts themselves (`java-runtime-epsilon/lib/modules`).
fn image_identity(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?;
    Some(format!(
        "{}:{}:{}",
        path.display(),
        meta.len(),
        mtime.as_nanos()
    ))
}

impl FabricBridge {
    /// Cache block facts under `dir` from now on. Called with the game
    /// folder's `.horizon-cache/fabric-blocks` before `init`, so that `init`
    /// can load them instead of reading every block again.
    pub fn set_block_facts_cache(&mut self, dir: Option<PathBuf>) {
        self.block_facts_cache = BlockFactsCache::new(dir);
    }

    /// The key of the classpath block facts are cached for, or `None` when a
    /// JAR of the classpath cannot be read (then nothing is cached: a key that
    /// does not cover every class could serve another jar set's facts).
    pub fn block_facts_key(&mut self) -> Option<String> {
        if let Some(key) = &self.block_facts_cache.key {
            return Some(key.clone());
        }
        let mut jars: Vec<(String, String)> = Vec::new();
        for path in self.interp.loader.jar_paths() {
            let hash = hfc_core::sha256_file(path).ok()?;
            jars.push((path.display().to_string(), hash));
        }
        // Two classpaths with the same jars are the same, in whatever order the
        // host added them.
        jars.sort();
        let mut images: Vec<String> = Vec::new();
        for path in self.interp.loader.jimage_paths() {
            images.push(image_identity(path)?);
        }
        images.sort();
        let mut text = format!(
            "hfc-block-facts\nversion={BLOCK_FACTS_CACHE_VERSION}\nruntime={}\napi={}\n",
            env!("CARGO_PKG_VERSION"),
            hfc_core::COMPAT_API,
        );
        for (path, hash) in &jars {
            text.push_str(&format!("jar={path}\nsha256={hash}\n"));
        }
        for image in &images {
            text.push_str(&format!("image={image}\n"));
        }
        let key = hfc_core::sha256_bytes(text.as_bytes());
        self.block_facts_cache.key = Some(key.clone());
        Some(key)
    }

    /// Whether the facts in the heap came back from the cache instead of being
    /// read from the mod's blocks.
    pub fn block_facts_from_cache(&self) -> bool {
        self.block_facts_cache.from_cache
    }

    /// Whether the facts in the heap carry the blocks' outline and collision
    /// shapes. `read_block_shapes` without an id list sets it, and only facts
    /// written after that are cached with it: whoever reads the facts must ask
    /// for the shapes again when it is false.
    pub fn block_facts_shapes_known(&self) -> bool {
        self.interp.heap.block_facts_shapes
    }

    /// Load the cached facts of this classpath into the heap, if there are any.
    /// Returns whether they were loaded; a miss leaves the heap untouched.
    pub(crate) fn load_block_facts_cache(&mut self) -> bool {
        self.block_facts_cache.from_cache = false;
        let Some(dir) = self.block_facts_cache.dir.clone() else {
            return false;
        };
        let started = std::time::Instant::now();
        let Some(key) = self.block_facts_key() else {
            log::debug!("block facts cache: a classpath jar cannot be hashed; not cached");
            return false;
        };
        let path = facts_path(&dir, &key);
        let Ok(bytes) = std::fs::read(&path) else {
            return false;
        };
        let cached: CachedFacts = match serde_json::from_slice(&bytes) {
            Ok(cached) => cached,
            Err(error) => {
                log::info!(
                    "block facts cache {}: {error}; reading the blocks again",
                    path.display()
                );
                return false;
            }
        };
        if cached.version != BLOCK_FACTS_CACHE_VERSION
            || cached.key != key
            || cached.runtime != env!("CARGO_PKG_VERSION")
        {
            return false;
        }
        self.interp.heap.block_facts = cached.blocks;
        self.interp.heap.block_facts_shapes = cached.shapes;
        self.block_facts_cache.from_cache = true;
        log::info!(
            "block facts: {} block(s) read from the cache in {:.1?} ({})",
            self.interp.heap.block_facts.len(),
            started.elapsed(),
            path.display()
        );
        true
    }

    /// Write the facts in the heap for this classpath. Nothing is written when
    /// there is no cache directory or no block was registered.
    pub fn store_block_facts_cache(&self) -> Option<PathBuf> {
        let dir = self.block_facts_cache.dir.as_ref()?;
        if self.interp.heap.block_facts.is_empty() {
            return None;
        }
        let key = self.block_facts_cache.key.clone()?;
        if std::fs::create_dir_all(dir).is_err() {
            return None;
        }
        let cached = CachedFacts {
            version: BLOCK_FACTS_CACHE_VERSION,
            key: key.clone(),
            runtime: env!("CARGO_PKG_VERSION").to_string(),
            shapes: self.interp.heap.block_facts_shapes,
            blocks: self.interp.heap.block_facts.clone(),
        };
        let bytes = serde_json::to_vec(&cached).ok()?;
        let path = facts_path(dir, &key);
        if let Err(error) = atomic_write(&path, &bytes) {
            log::warn!("block facts not cached: {}: {error}", path.display());
            return None;
        }
        // One file per classpath: the older ones are dead weight (a jar set
        // nobody launches again). They are removed, never the current one.
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("facts-")
                    && name.ends_with(".json")
                    && entry.path() != path
                    && std::fs::remove_file(entry.path()).is_ok()
                {
                    log::debug!("block facts cache: removed stale {name}");
                }
            }
        }
        Some(path)
    }
}
