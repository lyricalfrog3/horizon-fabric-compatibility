//! The Fabric GUI path, exercised through Mod Menu's own screen.
//!
//! Mod Menu is a pinned local fixture: its screen builds a real
//! `AbstractSelectionList`, renders every loaded mod and installs itself through
//! a Fabric key mapping. Nothing here branches on Mod Menu's id or classes; the
//! test uses the same generic key mapping, screen and display-list APIs any mod
//! would.

use hfc_runtime::bridge::FabricBridge;
use hfc_runtime::minecraft::GuiDraw;
use hfc_runtime::value::JavaObject;
use std::path::{Path, PathBuf};

fn collect_jars(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jars(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("jar") {
            out.push(path);
        }
    }
}

fn find_version_jar(root: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root.join("versions")).ok()?;
    let mut jars: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| {
            entry
                .path()
                .join(format!("{}.jar", entry.file_name().to_string_lossy()))
        })
        .filter(|jar| jar.is_file())
        .collect();
    jars.sort();
    jars.pop()
}

fn find_jimage(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_jimage(&path) {
                return Some(found);
            }
        } else if path.file_name().and_then(|s| s.to_str()) == Some("modules") {
            return Some(path);
        }
    }
    None
}

struct Fixture {
    minecraft: PathBuf,
    jars: Vec<PathBuf>,
}

impl Fixture {
    fn discover() -> Option<Self> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()?
            .parent()?
            .to_path_buf();
        let minecraft = std::env::var_os("HORIZON_MINECRAFT_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".local/share/horizon-launcher/minecraft"))
            })?;
        let version = find_version_jar(&minecraft)?;
        if find_jimage(&minecraft.join("runtime")).is_none() {
            eprintln!(
                "skipping: no JDK image under {}",
                minecraft.join("runtime").display()
            );
            return None;
        }
        let mut jars = Vec::new();
        collect_jars(&minecraft.join("libraries"), &mut jars);
        jars.push(version);
        for jar in ["modmenu-21.0.0.jar", "fabric-api-0.165.0+26.3.jar"] {
            let path = root.join("test-mods").join(jar);
            if !path.is_file() {
                eprintln!(
                    "skipping: missing pinned fixture {} (see test-mods/README.md)",
                    path.display()
                );
                return None;
            }
            jars.push(path);
        }
        Some(Self { minecraft, jars })
    }

    fn bridge(&self) -> FabricBridge {
        let mut bridge = FabricBridge::new();
        for jar in &self.jars {
            bridge.add_jar(jar).unwrap();
        }
        bridge
            .add_jimage(find_jimage(&self.minecraft.join("runtime")).unwrap())
            .unwrap();
        for jar in &self.jars {
            if jar.starts_with(&self.minecraft) {
                continue;
            }
            bridge.load_mod_jar(jar).unwrap();
        }
        bridge
    }
}

fn screen_class(bridge: &FabricBridge, screen: &hfc_runtime::value::Value) -> String {
    screen
        .as_obj()
        .and_then(|id| bridge.interp.heap.get(id))
        .and_then(|obj| match obj {
            JavaObject::Instance { class_name, .. } => Some(class_name.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn a_fabric_key_mapping_opens_its_screen_and_the_host_replays_it() {
    let Some(fixture) = Fixture::discover() else {
        return;
    };
    let mut bridge = fixture.bridge();
    bridge.init().unwrap();

    assert!(
        bridge.press_key_mapping("key.modmenu.open_menu"),
        "the mod's registered key mapping was not found"
    );
    let screen = bridge
        .current_screen()
        .expect("pressing the key mapping did not install a screen");
    assert_eq!(
        screen_class(&bridge, &screen),
        "com/terraformersmc/modmenu/gui/ModsScreen"
    );

    let draws = bridge
        .render_screen(1280, 720, 0.0)
        .expect("screen render failed")
        .expect("no screen to render");
    assert!(
        draws.len() > 100,
        "expected the mod list and its widgets, got {} draws",
        draws.len()
    );

    let texts: Vec<&str> = draws
        .iter()
        .filter_map(|draw| match draw {
            GuiDraw::Text { text, .. } if !text.is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        texts.contains(&"Minecraft"),
        "the mod list did not render the Minecraft entry: {texts:?}"
    );
    assert!(
        texts.iter().filter(|text| **text != "null").count() >= 10,
        "the list rendered too few labels: {texts:?}"
    );

    // 26.3's `KeyEvent.key` is an SDL scancode: Escape is 41 (`InputWithModifiers::isEscape`).
    assert!(bridge.key_event(41, 0, 0, true), "Escape was not handled");
    assert!(
        bridge.current_screen().is_none(),
        "Escape did not close the mod screen"
    );
}
