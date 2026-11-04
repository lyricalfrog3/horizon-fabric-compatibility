//! A JamLib mod (Right Click Harvest) writes its json5 config on first start.
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
