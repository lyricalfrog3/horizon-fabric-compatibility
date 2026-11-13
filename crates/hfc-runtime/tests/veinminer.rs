//! Veinminer (a Kotlin server mod) driven through `PlayerBlockBreakEvents.BEFORE`.
//!
//! The mod's own bytecode reads its JSON config, resolves block tags from the
//! game's registry data, searches for connected ore with a Kotlin coroutine
//! sequence and breaks each block through `ServerPlayerGameMode.destroyBlock`.
//! The enchantment companion mod is not installed, so the mod does not ask for
//! an enchanted tool. The test host is a hash map of blocks. It needs the jars in
//! `~/horizon-fabric-test/game/horizon-mods` and a Horizon install with
//! `registries.json`; without them the test skips.

use hfc_runtime::bridge::{FabricBridge, Flow, HostBridge};
use std::collections::HashMap;
use std::path::PathBuf;

struct World {
    blocks: HashMap<[i32; 3], String>,
    broken: Vec<[i32; 3]>,
    held: &'static str,
    enchantments: Vec<(String, i32)>,
}

impl HostBridge for World {
    fn block(&self, pos: [i32; 3]) -> String {
        self.blocks
            .get(&pos)
            .cloned()
            .unwrap_or_else(|| "minecraft:air".into())
    }
    fn block_property(&self, _: [i32; 3], _: &str) -> Option<String> {
        None
    }
    fn set_block_property(&mut self, _: [i32; 3], _: &str, _: &str) -> bool {
        false
    }
    fn give(&mut self, _: &str, _: u32) -> Result<(), String> {
        Ok(())
    }
    fn held_item(&self) -> Option<String> {
        Some(self.held.to_string())
    }
    fn held_enchantments(&self) -> Vec<(String, i32)> {
        self.enchantments.clone()
    }
    fn break_block(&mut self, pos: [i32; 3]) -> bool {
        self.broken.push(pos);
        self.blocks.remove(&pos).is_some()
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn mods_dir() -> Option<PathBuf> {
    let dir = home()?.join("horizon-fabric-test/game/horizon-mods");
    dir.join("veinminer-fabric-2.13.0.jar")
        .exists()
        .then_some(dir)
}

fn registry_dir() -> Option<PathBuf> {
    std::fs::read_dir(home()?.join(".local/share/horizon-launcher/versions"))
        .ok()?
        .flatten()
        .map(|entry| entry.path().join(".local/horizon-client/26.3"))
        .find(|dir| dir.join("registries.json").exists())
}

/// A bridge with the mods initialized and `SERVER_STARTING` dispatched.
fn started_bridge() -> Option<FabricBridge> {
    let (Some(mods), Some(registries)) = (mods_dir(), registry_dir()) else {
        eprintln!("skipped: veinminer or the game's registry data is not installed");
        return None;
    };
    let game_dir = std::env::temp_dir().join(format!("hfc-veinminer-{}", std::process::id()));
    let mut bridge = FabricBridge::new();
    for jar in std::fs::read_dir(&mods).unwrap().flatten() {
        let name = jar.file_name().to_string_lossy().to_string();
        if name.starts_with("fabric-api")
            || name.starts_with("fabric-language")
            || name.starts_with("veinminer")
        {
            bridge.load_mod_jar(jar.path()).unwrap();
        }
    }
    bridge.load_registries(&registries);
    bridge.set_game_dir(Some(game_dir));
    bridge.init().unwrap();
    bridge.server_starting();
    Some(bridge)
}

fn ore_row(held: &'static str, enchanted: bool) -> World {
    let mut blocks = HashMap::new();
    for x in 0..5 {
        blocks.insert([x, 10, 0], "minecraft:coal_ore".to_string());
    }
    // Stone next to the vein must stay.
    blocks.insert([0, 9, 0], "minecraft:stone".to_string());
    World {
        blocks,
        broken: Vec::new(),
        held,
        enchantments: if enchanted {
            vec![("veinminer_enchantment:veinminer".to_string(), 1)]
        } else {
            Vec::new()
        },
    }
}

#[test]
fn breaking_a_coal_vein_breaks_the_connected_ore() {
    let Some(mut bridge) = started_bridge() else {
        return;
    };
    let mut world = ore_row("minecraft:diamond_pickaxe", false);
    let flow = bridge.block_break_before(&mut world, [0, 10, 0], true, 8);
    assert_eq!(flow, Flow::Continue);
    let mut broken = world.broken.clone();
    broken.sort();
    assert_eq!(
        broken,
        vec![[1, 10, 0], [2, 10, 0], [3, 10, 0], [4, 10, 0]],
        "the four ore blocks beside the one the player broke"
    );
    assert_eq!(world.block([0, 9, 0]), "minecraft:stone");
}

#[test]
fn a_hand_does_not_vein_mine_ore() {
    let Some(mut bridge) = started_bridge() else {
        return;
    };
    let mut world = ore_row("minecraft:air", false);
    bridge.block_break_before(&mut world, [0, 10, 0], true, 8);
    assert!(world.broken.is_empty(), "broke {:?}", world.broken);
}
