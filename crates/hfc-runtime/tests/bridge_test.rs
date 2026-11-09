use hfc_runtime::bridge::{FabricBridge, Flow, HostBridge, RightClickBlockEvent};
use std::collections::HashMap;
use std::path::PathBuf;

mod common;

#[derive(Default)]
struct MockHost {
    blocks: HashMap<[i32; 3], (String, HashMap<String, String>)>,
    inventory: HashMap<String, u32>,
}

impl HostBridge for MockHost {
    fn block(&self, pos: [i32; 3]) -> String {
        self.blocks
            .get(&pos)
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| "minecraft:air".to_string())
    }

    fn block_property(&self, pos: [i32; 3], key: &str) -> Option<String> {
        self.blocks
            .get(&pos)
            .and_then(|(_, props)| props.get(key).cloned())
    }

    fn set_block_property(&mut self, pos: [i32; 3], key: &str, value: &str) -> bool {
        if let Some((_, props)) = self.blocks.get_mut(&pos) {
            props.insert(key.to_string(), value.to_string());
            true
        } else {
            false
        }
    }

    fn give(&mut self, item: &str, count: u32) -> Result<(), String> {
        *self.inventory.entry(item.to_string()).or_insert(0) += count;
        Ok(())
    }
}

#[test]
fn test_fabric_bridge_right_click_harvest() {
    let mut host = MockHost::default();

    // Place a mature wheat block at (5, 64, 5) with age 7
    let mut props = HashMap::new();
    props.insert("age".into(), "7".into());
    host.blocks
        .insert([5, 64, 5], ("minecraft:wheat".into(), props));

    // The mods' bytecode runs against the real Minecraft and JDK classes;
    // without an installation the test has nothing real to run on.
    let Some(mut bridge) = common::bridge_with_vanilla() else {
        eprintln!("skipping: no Minecraft installation");
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    for jar in &[
        "Iceberg-26.3-fabric-1.4.3.1.jar",
        "LegendaryTooltips-26.3-fabric-1.6.3.jar",
        "Prism-26.3-fabric-1.1.3.jar",
        "fabric-api-0.165.0+26.3.jar",
        "jamlib-fabric-2.3.1+26.3.x.jar",
        "modmenu-21.0.0.jar",
        "placeholder-api-3.2.0+26.3.jar",
        "rightclickharvest-fabric-4.6.2+26.3.x.jar",
    ] {
        bridge
            .load_mod_jar(&root.join("test-mods").join(jar))
            .expect("load jar");
    }

    // Dispatch right click block event on the mature crop
    let event = RightClickBlockEvent {
        pos: [5, 64, 5],
        face: [0, 1, 0], // UP
        hit: [5.5, 65.0, 5.5],
        sneaking: false,
        off_hand: false,
    };

    let flow = bridge.right_click_block(&mut host, &event);
    assert_eq!(flow, Flow::Cancel, "bridge should handle and cancel event");

    // Verify block was replanted to age 0
    assert_eq!(
        host.block_property([5, 64, 5], "age"),
        Some("0".into()),
        "crop age should be reset to 0"
    );

    // Verify harvested wheat and seeds were given to player
    assert!(
        host.inventory.get("minecraft:wheat").copied().unwrap_or(0) >= 1,
        "player should receive wheat"
    );
    assert!(
        host.inventory
            .get("minecraft:wheat_seeds")
            .copied()
            .unwrap_or(0)
            >= 1,
        "player should receive wheat seeds"
    );
}

#[test]
fn test_fabric_bridge_immature_crop_passes() {
    let mut host = MockHost::default();

    // Place an immature wheat block at (5, 64, 5) with age 3
    let mut props = HashMap::new();
    props.insert("age".into(), "3".into());
    host.blocks
        .insert([5, 64, 5], ("minecraft:wheat".into(), props));

    // The mods' bytecode runs against the real Minecraft and JDK classes;
    // without an installation the test has nothing real to run on.
    let Some(mut bridge) = common::bridge_with_vanilla() else {
        eprintln!("skipping: no Minecraft installation");
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let jamlib_path = root.join("test-mods/jamlib-fabric-2.3.1+26.3.x.jar");
    let rch_path = root.join("test-mods/rightclickharvest-fabric-4.6.2+26.3.x.jar");

    bridge.load_mod_jar(&jamlib_path).expect("load jamlib");
    bridge.load_mod_jar(&rch_path).expect("load rch");

    let event = RightClickBlockEvent {
        pos: [5, 64, 5],
        face: [0, 1, 0],
        hit: [5.5, 65.0, 5.5],
        sneaking: false,
        off_hand: false,
    };

    let flow = bridge.right_click_block(&mut host, &event);
    assert_eq!(
        flow,
        Flow::Continue,
        "immature crop should not be harvested"
    );
    assert_eq!(
        host.block_property([5, 64, 5], "age"),
        Some("3".into()),
        "crop age should remain unchanged"
    );
    assert!(host.inventory.is_empty(), "no items should be given");
}

#[test]
fn mods_discover_their_dependencies_instead_of_hiding_them() {
    let mut bridge = FabricBridge::new();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

    let jars = [
        "fabric-api-0.165.0+26.3.jar",
        "placeholder-api-3.2.0+26.3.jar",
        "Iceberg-26.3-fabric-1.4.3.1.jar",
        "jamlib-fabric-2.3.1+26.3.x.jar",
        "Prism-26.3-fabric-1.1.3.jar",
        "modmenu-21.0.0.jar",
        "rightclickharvest-fabric-4.6.2+26.3.x.jar",
        "LegendaryTooltips-26.3-fabric-1.6.3.jar",
    ];

    for j in jars {
        let p = root.join("test-mods").join(j);
        bridge
            .load_mod_jar(&p)
            .unwrap_or_else(|_| panic!("load {j}"));
    }

    let init_res = bridge.init();
    assert!(init_res.is_ok(), "bridge.init() should succeed");

    let report = bridge.class_closure();
    println!(
        "closure: {} resolved, {} missing",
        report.resolved.len(),
        report.missing.len()
    );

    assert!(
        report
            .resolved
            .contains(&"com/terraformersmc/modmenu/ModMenu".to_string()),
        "the mod's own entrypoint class must be part of its closure"
    );
    assert!(
        report.resolved.len() > 50,
        "discovery must walk beyond the entrypoint class"
    );
    assert!(
        report
            .missing
            .iter()
            .any(|name| name.starts_with("net/fabricmc/loader/api/")),
        "the Fabric Loader API is a real dependency; a runtime that cannot \
         provide it must say so instead of silently returning null"
    );
}

#[test]
fn test_loaded_mods_metadata() {
    let mut bridge = FabricBridge::new();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

    let jamlib_path = root.join("test-mods/jamlib-fabric-2.3.1+26.3.x.jar");
    let rch_path = root.join("test-mods/rightclickharvest-fabric-4.6.2+26.3.x.jar");
    let lt_path = root.join("test-mods/LegendaryTooltips-26.3-fabric-1.6.3.jar");

    bridge.load_mod_jar(&jamlib_path).expect("load jamlib");
    bridge.load_mod_jar(&rch_path).expect("load rch");
    bridge
        .load_mod_jar(&lt_path)
        .expect("load legendarytooltips");

    let mods = bridge.loaded_mods();
    assert!(mods.iter().any(|m| m.id == "jamlib" && !m.is_nested));
    assert!(
        mods.iter()
            .any(|m| m.id == "rightclickharvest" && !m.is_nested)
    );
    assert!(
        mods.iter()
            .any(|m| m.id == "legendarytooltips" && !m.is_nested)
    );

    let rch = mods.iter().find(|m| m.id == "rightclickharvest").unwrap();
    assert_eq!(rch.name, "Right Click Harvest");
    assert!(!rch.version.is_empty());
}

#[test]
fn test_item_tooltip_and_color_dispatch() {
    let mut bridge = FabricBridge::new();

    // Test tooltip dispatch with initial lines
    let mut lines = vec![("Diamond Sword".to_string(), 0xFFFFFF)];
    bridge.item_tooltip("minecraft:diamond_sword", 1, &mut lines);
    assert_eq!(lines.len(), 1);

    // Test tooltip color dispatch with default values
    let mut bg = 0xf0100010;
    let mut border_top = 0x505000ff;
    let mut border_bottom = 0x5028007f;
    bridge.item_tooltip_color(
        Some("minecraft:diamond_sword"),
        Some("common"),
        &mut bg,
        &mut border_top,
        &mut border_bottom,
    );
    // When no listener changes it, values remain unchanged
    assert_eq!(bg, 0xf0100010);
    assert_eq!(border_top, 0x505000ff);
    assert_eq!(border_bottom, 0x5028007f);
}
