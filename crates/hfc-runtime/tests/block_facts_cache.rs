//! Block facts cached per classpath: the same facts come back, and a different
//! jar set is never served another set's facts.
//!
//! The cache is keyed on the SHA-256 of every JAR the interpreter loads, so a
//! changed library or mod jar is a miss by construction; these tests prove it
//! on the real classpath, and prove that a cache hit produces exactly the facts
//! a fresh read produces (the whole list, shapes included, compared field by
//! field through `RegisteredBlock`'s equality).
//!
//! Needs a Minecraft install and `test-mods/` jars; otherwise it skips. mdm's
//! init costs about 46M interpreter instructions, so run it with `--release`.

mod common;

use hfc_runtime::bridge::{FabricBridge, RegisteredBlock};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf()
}

fn test_mod(name: &str) -> Option<PathBuf> {
    let jar = root().join("test-mods").join(name);
    if !jar.exists() {
        eprintln!("skipped: test-mods/{name} is not present");
        return None;
    }
    Some(jar)
}

/// A temp cache directory of this test, removed when `path` is dropped.
struct Cache {
    path: PathBuf,
}

impl Cache {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("hfc-block-facts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        Self { path }
    }

    fn dir(&self) -> &Path {
        &self.path
    }

    fn files(&self) -> Vec<PathBuf> {
        std::fs::read_dir(&self.path)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A bridge over the installed client, its libraries and the JDK image with
/// `jars` loaded as mods, caching facts under `cache`.
fn bridge(jars: &[&Path], cache: &Cache) -> Option<FabricBridge> {
    let mut bridge = common::bridge_with_vanilla()?;
    for jar in jars {
        bridge.load_mod_jar(jar).unwrap();
    }
    bridge.set_block_facts_cache(Some(cache.dir().to_path_buf()));
    bridge.init().unwrap();
    Some(bridge)
}

/// The facts a cache hit serves are the facts a cold read produces: every
/// block, every state, every shape, every note.
#[test]
fn cached_facts_are_the_facts_a_fresh_read_produces() {
    let (Some(api), Some(mdm)) = (
        test_mod("fabric-api-0.165.0+26.3.jar"),
        test_mod("mdm-3.1-fabric-26.3.jar"),
    ) else {
        return;
    };
    let jars = [api.as_path(), mdm.as_path()];
    let cache = Cache::new("roundtrip");

    // Cold: the facts are read out of mdm's own block objects.
    let Some(mut cold) = bridge(&jars, &cache) else {
        return;
    };
    assert!(
        !cold.block_facts_from_cache(),
        "an empty cache directory cannot hit"
    );
    // Two blocks' shapes, so the round trip carries real boxes (a cabinet's
    // four single-box states and a fridge's many-box ones) instead of `None`.
    cold.read_block_shapes(&["mdm:set_1_cabinet", "mdm:black_modern_fridge_1"]);
    let fresh: Vec<RegisteredBlock> = cold.registered_blocks();
    assert_eq!(fresh.len(), 344, "mdm registers 344 blocks");
    let shape_boxes = |blocks: &[RegisteredBlock], id: &str| -> usize {
        let block = blocks.iter().find(|b| b.id == id).expect(id);
        block
            .states
            .values()
            .map(|state| state.outline.as_ref().map_or(0, Vec::len))
            .sum()
    };
    assert!(shape_boxes(&fresh, "mdm:black_modern_fridge_1") > 4);
    let written = cold
        .store_block_facts_cache()
        .expect("the facts are written for the next start");
    assert!(written.is_file(), "{}", written.display());
    assert_eq!(cache.files(), [written.clone()]);

    // Warm: another start over the same classpath reads them back.
    let Some(warm) = bridge(&jars, &cache) else {
        return;
    };
    assert!(
        warm.block_facts_from_cache(),
        "the same jar set is a cache hit"
    );
    let cached = warm.registered_blocks();
    assert_eq!(
        cached, fresh,
        "a cache hit serves the facts of a fresh read"
    );
    assert_eq!(
        shape_boxes(&cached, "mdm:black_modern_fridge_1"),
        shape_boxes(&fresh, "mdm:black_modern_fridge_1")
    );

    // And a start that never sees the cache produces the same facts again: the
    // comparison above is not two reads of one mutated heap.
    let other = Cache::new("roundtrip-fresh");
    let Some(mut fresh_again) = bridge(&jars, &other) else {
        return;
    };
    assert!(!fresh_again.block_facts_from_cache());
    fresh_again.read_block_shapes(&["mdm:set_1_cabinet", "mdm:black_modern_fridge_1"]);
    assert_eq!(fresh_again.registered_blocks(), fresh);
}

/// A changed jar set is a miss: never another classpath's facts.
#[test]
fn another_jar_set_is_a_miss() {
    let (Some(api), Some(mdm), Some(tools)) = (
        test_mod("fabric-api-0.165.0+26.3.jar"),
        test_mod("mdm-3.1-fabric-26.3.jar"),
        test_mod("emerald-tools-justfatlard-2.2.1.jar"),
    ) else {
        return;
    };
    let cache = Cache::new("jars");
    let jars = [api.as_path(), mdm.as_path()];

    let Some(cold) = bridge(&jars, &cache) else {
        return;
    };
    assert!(!cold.block_facts_from_cache());
    let first = cold.registered_blocks();
    let written = cold.store_block_facts_cache().expect("facts written");

    // The same jar set hits.
    let Some(same) = bridge(&jars, &cache) else {
        return;
    };
    assert!(same.block_facts_from_cache(), "the same jar set hits");
    assert_eq!(same.registered_blocks(), first);

    // One more jar — a mod that registers no block at all — changes the
    // classpath, so the cached facts are not used.
    let three = [api.as_path(), mdm.as_path(), tools.as_path()];
    let two = {
        let Some(extra) = bridge(&three, &cache) else {
            return;
        };
        assert!(
            !extra.block_facts_from_cache(),
            "a jar set with one more jar is a miss"
        );
        extra.registered_blocks()
    };
    assert_eq!(two, first, "the extra jar registers no block");

    // One jar fewer is a miss too, and so is a file that does not read back:
    // the facts are read from the blocks again rather than trusted.
    let one = {
        let Some(fewer) = bridge(&[api.as_path()], &cache) else {
            return;
        };
        assert!(!fewer.block_facts_from_cache(), "one jar fewer is a miss");
        assert!(
            fewer.registered_blocks().is_empty(),
            "fabric-api has no block"
        );
        fewer.registered_blocks()
    };
    assert!(one.is_empty());
    std::fs::write(&written, b"{\"version\": 1, \"key\": \"nonsense\"}").unwrap();
    let Some(damaged) = bridge(&jars, &cache) else {
        return;
    };
    assert!(
        !damaged.block_facts_from_cache(),
        "a cache file that does not match is a miss"
    );
    assert_eq!(damaged.registered_blocks(), first);
}

/// A cache written before a config change (same jars, same key) is not served
/// when the entrypoints register a different list of blocks this start.
#[test]
fn a_changed_block_list_is_a_miss_even_with_the_same_key() {
    let (Some(api), Some(mdm)) = (
        test_mod("fabric-api-0.165.0+26.3.jar"),
        test_mod("mdm-3.1-fabric-26.3.jar"),
    ) else {
        return;
    };
    let jars = [api.as_path(), mdm.as_path()];
    let cache = Cache::new("ids");
    let Some(cold) = bridge(&jars, &cache) else {
        return;
    };
    assert!(!cold.block_facts_from_cache());
    let fresh = cold.registered_blocks();
    let written = cold.store_block_facts_cache().expect("facts written");

    // The same file and key, but one block id the entrypoints do not register:
    // the cache cannot describe this start.
    let mut cached: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&written).unwrap()).unwrap();
    cached["blocks"][0]["id"] = serde_json::json!("example:not_registered");
    std::fs::write(&written, serde_json::to_vec(&cached).unwrap()).unwrap();

    let Some(reloaded) = bridge(&jars, &cache) else {
        return;
    };
    assert!(
        !reloaded.block_facts_from_cache(),
        "a changed block list must not be served from the cache"
    );
    assert_eq!(reloaded.registered_blocks(), fresh);
}

/// The key covers every loaded jar and not their order.
#[test]
fn the_key_is_the_set_of_jars_in_any_order() {
    let (Some(api), Some(tools)) = (
        test_mod("fabric-api-0.165.0+26.3.jar"),
        test_mod("emerald-tools-justfatlard-2.2.1.jar"),
    ) else {
        return;
    };
    let Some(mut first) = common::bridge_with_vanilla() else {
        return;
    };
    let Some(mut second) = common::bridge_with_vanilla() else {
        return;
    };
    first.add_jar(&api).unwrap();
    first.add_jar(&tools).unwrap();
    second.add_jar(&tools).unwrap();
    second.add_jar(&api).unwrap();
    let a = first.block_facts_key().expect("a key");
    let b = second.block_facts_key().expect("a key");
    assert_eq!(
        a, b,
        "the same jars in another order are the same classpath"
    );
    assert_eq!(a.len(), 64, "a SHA-256 in hex");

    let Some(mut third) = common::bridge_with_vanilla() else {
        return;
    };
    third.add_jar(&api).unwrap();
    assert_ne!(
        third.block_facts_key().expect("a key"),
        a,
        "one jar fewer is another classpath"
    );
}
