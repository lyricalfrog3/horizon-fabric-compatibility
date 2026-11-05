//! Mods write their config files on first start: Right Click Harvest (JamLib,
//! json5) and Legendary Tooltips (Iceberg over Night Config, TOML).
//!
//! The file comes from the mod's own bytecode: JamLib reflects over the config
//! class's fields and annotations, Jankson serializes them with comments, and
//! the text goes through `String.replaceAll`/`split` and `Files.writeString`.
//! The expected text is what the real game writes. The test needs the jars in
//! `test-mods/` and a Minecraft install; without them it skips.

mod common;
use std::path::PathBuf;

const EXPECTED: &str = "{
\t// - default: false
\t\"requireHoe\": false,
\t// - default: true
\t\"harvestInRadius\": true,
\t/* - default: NONE
\t   - must be one of: NONE, LOW, NORMAL, HIGH
\t*/
\t\"hungerLevel\": \"NONE\",
\t/* - default: NONE
\t   - must be one of: NONE, COST, REWARD
\t*/
\t\"experienceType\": \"NONE\",
\t/* Modpack developers, set this to true to stop RCH telling users that they probably need to equip a hoe to harvest crops (if requireHoe is set to true). This message will only be displayed once.
\t   - default: false
\t*/
\t\"hasUserBeenWarnedForNotUsingHoe\": false,
\t/* Deprecated for removal in Minecraft 26.1
\t   - default: false
\t*/
\t\"showServerWarning\": false
}";

#[test]
fn jamlib_mod_writes_its_json5_config_with_comments() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf();
    let jars = [
        "fabric-api-0.165.0+26.3.jar",
        "jamlib-fabric-2.3.1+26.3.x.jar",
        "rightclickharvest-fabric-4.6.2+26.3.x.jar",
    ]
    .map(|name| root.join("test-mods").join(name));
    if jars.iter().any(|jar| !jar.exists()) {
        eprintln!("skipped: test-mods jars are not present");
        return;
    }
    let Some(mut bridge) = common::bridge_with_vanilla() else {
        eprintln!("skipped: no Minecraft installation");
        return;
    };
    for jar in &jars {
        bridge.load_mod_jar(jar).unwrap();
    }
    let dir = std::env::temp_dir().join(format!("hfc-config-files-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    bridge.set_game_dir(Some(dir.clone()));
    bridge.init().unwrap();

    let written = std::fs::read_to_string(dir.join("config/rightclickharvest.json5")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(written, EXPECTED);
}

/// Load `names` from `test-mods/` into a fresh bridge, run `init`, and return
/// the game directory the mods wrote into.
fn run_mods(names: &[&str], tag: &str) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("hfc-config-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    launch_mods(names, &dir)?;
    Some(dir)
}

/// Load `names` into a fresh bridge on `dir` (kept as is) and run `init`, as a
/// new game launch does. Returns the bridge so a test can inspect mod state.
fn launch_mods(names: &[&str], dir: &std::path::Path) -> Option<hfc_runtime::bridge::FabricBridge> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf();
    let jars: Vec<PathBuf> = names
        .iter()
        .map(|name| root.join("test-mods").join(name))
        .collect();
    if jars.iter().any(|jar| !jar.exists()) {
        eprintln!("skipped: test-mods jars are not present");
        return None;
    }
    let mut bridge = common::bridge_with_vanilla()?;
    for jar in &jars {
        bridge.load_mod_jar(jar).unwrap();
    }
    bridge.set_game_dir(Some(dir.to_path_buf()));
    bridge.init().unwrap();
    Some(bridge)
}

const LEGENDARY_MODS: [&str; 4] = [
    "fabric-api-0.165.0+26.3.jar",
    "Iceberg-26.3-fabric-1.4.3.1.jar",
    "Prism-26.3-fabric-1.1.3.jar",
    "LegendaryTooltips-26.3-fabric-1.6.3.jar",
];

/// `LegendaryTooltipsConfig.getInstance().modelRotationSpeed.get()`.
fn model_rotation_speed(bridge: &mut hfc_runtime::bridge::FabricBridge) -> f64 {
    use hfc_runtime::value::{JavaObject, Value};
    const CONFIG: &str = "com/anthonyhilyard/legendarytooltips/config/LegendaryTooltipsConfig";
    let instance = bridge
        .interp
        .execute_special_method(CONFIG, "getInstance", &format!("()L{CONFIG};"), &[])
        .unwrap();
    let id = instance.as_obj().expect("config instance");
    let supplier = match bridge.interp.heap.get(id) {
        Some(JavaObject::Instance { fields, .. }) => fields["modelRotationSpeed"].clone(),
        other => panic!("config is not an instance: {}", other.is_some()),
    };
    assert!(matches!(supplier, Value::Object(Some(_))));
    let value = bridge
        .interp
        .execute_method(
            "java/util/function/Supplier",
            "get",
            "()Ljava/lang/Object;",
            &[supplier],
        )
        .unwrap();
    match value {
        Value::Double(d) => d,
        other => panic!("the supplier did not return a Double: {other:?}"),
    }
}

#[test]
fn existing_toml_config_is_read_back_without_corruption() {
    let dir = std::env::temp_dir().join(format!("hfc-config-reread-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("config/legendarytooltips.toml");
    if launch_mods(&LEGENDARY_MODS, &dir).is_none() {
        return;
    }
    let first = std::fs::read_to_string(&path).unwrap();
    assert!(first.contains("model_rotation_speed = 12\n"), "{first}");

    let mut second_launch = launch_mods(&LEGENDARY_MODS, &dir).unwrap();
    let second = std::fs::read_to_string(&path).unwrap();
    assert_eq!(first, second, "relaunch rewrote the config");
    let speed = model_rotation_speed(&mut second_launch);
    assert_eq!(speed, 12.0);

    std::fs::write(
        &path,
        first.replace("model_rotation_speed = 12", "model_rotation_speed = 20"),
    )
    .unwrap();
    let mut third_launch = launch_mods(&LEGENDARY_MODS, &dir).unwrap();
    let speed = model_rotation_speed(&mut third_launch);
    let third = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(speed, 20.0, "{third}");
    assert!(third.contains("model_rotation_speed = 20\n"), "{third}");
    assert_eq!(third.matches("name_separator").count(), 1, "{third}");
}

#[test]
fn iceberg_mod_writes_its_toml_config_with_comments() {
    let Some(dir) = run_mods(
        &[
            "fabric-api-0.165.0+26.3.jar",
            "Iceberg-26.3-fabric-1.4.3.1.jar",
            "Prism-26.3-fabric-1.1.3.jar",
            "LegendaryTooltips-26.3-fabric-1.6.3.jar",
        ],
        "toml",
    ) else {
        return;
    };
    let written = std::fs::read_to_string(dir.join("config/legendarytooltips.toml")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    // Values are typed (booleans are booleans), each option carries its comment,
    // and enum options list their allowed values.
    assert!(
        written.starts_with(
            "# Whether item names in tooltips should have a line under them separating them from the rest of the tooltip.\nname_separator = true\n"
        ),
        "{written}"
    );
    assert!(
        written.contains("enforce_minimum_width = false\n"),
        "{written}"
    );
    assert!(
        written
            .contains("#Allowed values: NONE, EQUIPMENT, ALL\nrender_item_model = \"EQUIPMENT\"\n"),
        "{written}"
    );
    assert!(
        written.contains("#Range: 0 ~ 50\nmodel_rotation_speed = 12\n"),
        "{written}"
    );
    assert!(written.contains("level15_entries = []\n"), "{written}");
    assert!(
        written.contains("priorities = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]"),
        "{written}"
    );
    assert!(!written.contains('%'), "unformatted name in {written}");
}

#[test]
fn gson_mod_writes_its_pretty_printed_json_config() {
    let Some(dir) = run_mods(
        &["fabric-api-0.165.0+26.3.jar", "modmenu-21.0.0.jar"],
        "gson",
    ) else {
        return;
    };
    let written = std::fs::read_to_string(dir.join("config/modmenu.json")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    // Gson's own writer runs: two-space indentation, booleans as booleans, empty
    // arrays, and enum options written as lower-case names through the field's
    // generic type.
    assert!(
        written.starts_with("{\n  \"sorting\": \"ascending\",\n  \"count_libraries\": true,\n"),
        "{written}"
    );
    assert!(
        written.contains("  \"sorting\": \"ascending\",\n"),
        "{written}"
    );
    assert!(
        written.contains("  \"update_channel\": \"release\""),
        "{written}"
    );
    assert!(written.contains("  \"hidden_mods\": []"), "{written}");
}
