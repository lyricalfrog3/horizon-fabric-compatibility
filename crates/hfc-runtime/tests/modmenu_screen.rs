//! The Fabric GUI path, exercised through Mod Menu's own screen.
//!
//! Mod Menu is a pinned local fixture: its screen builds a real
//! `AbstractSelectionList`, renders every loaded mod and installs itself through
//! a Fabric key mapping. Nothing here branches on Mod Menu's id or classes; the
//! test uses the same generic key mapping, screen and display-list APIs any mod
//! would.

use hfc_runtime::bridge::FabricBridge;
use hfc_runtime::minecraft::GuiDraw;
use hfc_runtime::value::{JavaObject, Value};
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

fn text_draws(draws: &[GuiDraw]) -> Vec<&str> {
    draws
        .iter()
        .filter_map(|draw| match draw {
            GuiDraw::Text { text, .. } if !text.is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn search_box_rect(bridge: &FabricBridge, screen: &Value) -> Option<(i32, i32, i32, i32)> {
    let fields = match screen.as_obj().and_then(|id| bridge.interp.heap.get(id)) {
        Some(JavaObject::Instance { fields, .. }) => fields,
        _ => return None,
    };
    let box_id = fields.get("searchBox").and_then(Value::as_obj)?;
    let fields = match bridge.interp.heap.get(box_id) {
        Some(JavaObject::Instance { fields, .. }) => fields,
        _ => return None,
    };
    let get = |name: &str| fields.get(name).map(Value::as_int).unwrap_or(0);
    Some((get("x"), get("y"), get("width"), get("height")))
}

#[test]
fn a_fabric_key_mapping_opens_its_screen_and_the_host_replays_it() {
    let Some(fixture) = Fixture::discover() else {
        return;
    };
    let mut bridge = fixture.bridge();
    bridge.init().unwrap();
    assert!(
        bridge.translations().contains_key("modmenu.showingMods.n"),
        "the mod's own language file was not loaded"
    );

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

    let texts = text_draws(&draws);
    assert!(
        texts.contains(&"Minecraft"),
        "the mod list did not render the Minecraft entry: {texts:?}"
    );
    assert!(
        texts.len() >= 10,
        "the list rendered too few labels: {texts:?}"
    );
    assert!(
        texts.iter().any(|text| text.contains("Website")),
        "the Website button rendered no label: {texts:?}"
    );
    assert!(
        texts.iter().any(|text| text.starts_with("Showing ")),
        "the mod count line did not render: {texts:?}"
    );
    assert!(
        !texts.iter().any(|text| text.starts_with("modmenu.")),
        "raw translation keys were rendered: {texts:?}"
    );
    assert!(
        !texts
            .iter()
            .any(|text| *text == "null" || text.contains("[Object")),
        "objects were rendered instead of their text: {texts:?}"
    );

    // Mod Menu's search field has no `setHint` call: it passes its search label
    // to the `EditBox` constructor, which stores it as the narration message.
    // Vanilla 26.3 therefore renders no placeholder here either, and the test
    // must not invent one to look more complete than the runtime is.

    let blits: Vec<&str> = draws
        .iter()
        .filter_map(|draw| match draw {
            GuiDraw::Blit { texture, .. } => Some(texture.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        blits
            .iter()
            .any(|texture| texture.contains("inworld_menu_background")),
        "the screen background was not recorded: {blits:?}"
    );
    assert!(
        blits.iter().any(|texture| texture.starts_with("modmenu:")),
        "no mod icon cell recorded a texture: {blits:?}"
    );

    // 26.3's `KeyEvent.key` is an SDL scancode: Escape is 41 (`InputWithModifiers::isEscape`).
    assert!(bridge.key_event(41, 0, 0, true), "Escape was not handled");
    assert!(
        bridge.current_screen().is_none(),
        "Escape did not close the mod screen"
    );
}

#[test]
fn the_mod_list_search_box_accepts_focus_and_filters_the_list() {
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

    let before = bridge
        .render_screen(1280, 720, 0.0)
        .expect("screen render failed")
        .expect("no screen to render");
    let before_texts = text_draws(&before);
    let before_showing = before_texts
        .iter()
        .find(|text| text.starts_with("Showing "))
        .expect("the unfiltered mod count was not rendered")
        .to_string();
    assert!(
        !before_texts.iter().any(|text| *text == "Search for mods"),
        "a search hint appeared even though Mod Menu never calls setHint: {before_texts:?}"
    );

    let (x, y, width, height) =
        search_box_rect(&bridge, &screen).expect("the screen has no search field");
    let cx = f64::from(x) + f64::from(width) / 2.0;
    let cy = f64::from(y) + f64::from(height) / 2.0;

    assert!(
        bridge.mouse_button(cx, cy, 0, true),
        "clicking the search field was not handled"
    );
    assert!(
        bridge.char_event('z' as i32),
        "typing into the focused search field was not handled"
    );

    let after = bridge
        .render_screen(1280, 720, 0.0)
        .expect("screen render failed")
        .expect("no screen to render");
    let after_texts = text_draws(&after);
    let after_showing = after_texts
        .iter()
        .find(|text| text.starts_with("Showing "))
        .expect("the filtered mod count was not rendered")
        .to_string();
    assert_ne!(
        before_showing, after_showing,
        "typing into the search field did not filter the list"
    );
    assert!(
        after_showing.contains('/'),
        "the filtered count should include the total: {after_showing:?}"
    );
    assert!(
        after_texts.len() < before_texts.len(),
        "the filtered list should draw fewer entries: {} -> {}",
        before_texts.len(),
        after_texts.len()
    );
}
