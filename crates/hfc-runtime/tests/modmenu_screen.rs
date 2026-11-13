//! The Fabric GUI path, exercised through Mod Menu's own screen.
//!
//! Mod Menu is a pinned local fixture: its screen builds a real
//! `AbstractSelectionList`, renders every loaded mod and installs itself through
//! a Fabric key mapping. Nothing here branches on Mod Menu's id or classes; the
//! test uses the same generic key mapping, screen and display-list APIs any mod
//! would.

use hfc_runtime::bridge::{FabricBridge, HostRequest};
use hfc_runtime::minecraft::{GuiDraw, MenuKind};
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
        draws.len() > 50,
        "expected the mod list and its widgets, got {} draws",
        draws.len()
    );

    let texts = text_draws(&draws);
    // Placeholder API carries the `library` badge in its `custom.modmenu`
    // metadata, and Mod Menu hides libraries unless its config shows them.
    assert!(
        texts.contains(&"Showing 2 mods") && !texts.contains(&"Placeholder API"),
        "the mod list did not hide the library mod: {texts:?}"
    );
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
    let icon = bridge
        .registered_textures()
        .get("modmenu:modmenu_icon")
        .expect("the mod's own icon was not registered with its PNG bytes");
    assert!(
        icon.png.starts_with(b"\x89PNG"),
        "the registered icon does not carry the mod's PNG bytes"
    );
    assert!(
        icon.width > 0 && icon.height > 0,
        "the icon has no size: {}x{}",
        icon.width,
        icon.height
    );

    // 26.3's `KeyEvent.key` is an SDL scancode: Escape is 41 (`InputWithModifiers::isEscape`).
    assert!(bridge.key_event(41, 0, 0, true), "Escape was not handled");
    assert!(
        bridge.current_screen().is_none(),
        "Escape did not close the mod screen"
    );
}

#[test]
fn the_title_menu_receives_mod_menu_widgets() {
    let Some(fixture) = Fixture::discover() else {
        return;
    };
    let mut bridge = fixture.bridge();
    bridge.init().unwrap();
    assert!(bridge.has_menu_injectors(MenuKind::Title));
    let widgets = bridge.prepare_menu_contributions(MenuKind::Title, 1280, 720);
    let index = widgets
        .iter()
        .position(|widget| widget.label == "Mods")
        .unwrap_or_else(|| panic!("no Mods widget in {widgets:?}"));
    let widget = &widgets[index];
    assert_eq!((widget.width, widget.height), (20, 20));
    assert!(widget.visible && widget.active);
    assert!(
        bridge.menu_widget_click(
            MenuKind::Title,
            1280,
            720,
            index,
            f64::from(widget.x) + 10.0,
            f64::from(widget.y) + 10.0,
            1,
            true,
        ),
        "clicking the title widget was not handled"
    );
    let screen = bridge
        .current_screen()
        .expect("the title widget did not open a screen");
    assert_eq!(
        screen_class(&bridge, &screen),
        "com/terraformersmc/modmenu/gui/ModsScreen"
    );
    let small = bridge.prepare_menu_contributions(MenuKind::Title, 427, 240);
    assert!(!small.is_empty(), "no title widget at the live GUI size");
    let draws = bridge.menu_widget_draw(MenuKind::Title, 427, 240, 0, 0, 0);
    assert!(!draws.is_empty(), "the title widget rendered nothing");
}

#[test]
fn the_pause_menu_receives_mod_menu_widgets() {
    let Some(fixture) = Fixture::discover() else {
        return;
    };
    let mut bridge = fixture.bridge();
    bridge.init().unwrap();
    assert!(
        bridge.has_menu_injectors(MenuKind::Pause),
        "the pause menu has no mod injectors"
    );
    let widgets = bridge.prepare_menu_contributions(MenuKind::Pause, 1280, 720);
    let index = widgets
        .iter()
        .position(|widget| widget.label == "Mods")
        .unwrap_or_else(|| panic!("no Mods widget in {widgets:?}"));
    let widget = &widgets[index];
    assert_eq!((widget.width, widget.height), (20, 20));
    assert!(widget.visible && widget.active);

    let draws = bridge.menu_widget_draw(MenuKind::Pause, 1280, 720, index, 0, 0);
    assert!(
        !draws.is_empty(),
        "the contributed widget rendered nothing: {draws:?}"
    );

    assert!(
        bridge.menu_widget_click(MenuKind::Pause, 1280, 720, index, 10.0, 10.0, 1, true),
        "clicking the contributed widget was not handled"
    );
    let screen = bridge
        .current_screen()
        .expect("the contributed widget did not open a screen");
    assert_eq!(
        screen_class(&bridge, &screen),
        "com/terraformersmc/modmenu/gui/ModsScreen"
    );
}

#[test]
fn the_mod_list_scrolls_with_the_wheel_and_the_scrollbar() {
    let Some(fixture) = Fixture::discover() else {
        return;
    };
    let mut bridge = fixture.bridge();
    bridge.init().unwrap();
    assert!(
        bridge.press_key_mapping("key.modmenu.open_menu"),
        "the mod's registered key mapping was not found"
    );
    let texts_at = |bridge: &mut FabricBridge| -> Vec<String> {
        text_draws(
            &bridge
                .render_screen(1280, 200, 0.0)
                .expect("screen render failed")
                .expect("no screen to render"),
        )
        .into_iter()
        .map(str::to_string)
        .collect()
    };
    // The root mods at 36 px are taller than the list's ~97 px at this height,
    // so the wheel and the scrollbar have somewhere to go.
    let top = texts_at(&mut bridge);
    assert!(
        bridge.mouse_scrolled(300.0, 100.0, 0.0, -10.0),
        "the wheel was not handled"
    );
    let wheeled = texts_at(&mut bridge);
    assert_ne!(top, wheeled, "the wheel did not move the list");

    assert!(
        bridge.mouse_scrolled(300.0, 100.0, 0.0, 1000.0),
        "the wheel was not handled"
    );
    assert_eq!(
        texts_at(&mut bridge),
        top,
        "the wheel did not return the list to the top"
    );

    // `ModListWidget.scrollBarX` is `width - 6`; press on it and drag down.
    assert!(
        bridge.mouse_button(629.0, 80.0, 1, true),
        "the scrollbar press was not handled"
    );
    bridge.mouse_moved(629.0, 260.0);
    // `AbstractContainerWidget.mouseReleased` returns its container default's
    // result (false when no child is focused), so the release's own return
    // value is not meaningful; the list moving is.
    bridge.mouse_button(629.0, 260.0, 1, false);
    let dragged = texts_at(&mut bridge);
    assert_ne!(top, dragged, "dragging the scrollbar did not move the list");
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
        !before_texts.contains(&"Search for mods"),
        "a search hint appeared even though Mod Menu never calls setHint: {before_texts:?}"
    );
    assert!(
        !before_texts
            .iter()
            .any(|text| text.starts_with("Fabric API Lookup API")),
        "a Fabric API library module was listed as a root mod: {before_texts:?}"
    );
    // ContainerEventHandler.mouseScrolled routes the wheel through
    // Optional.filter; the list must accept it even when its content fits.
    assert!(
        bridge.mouse_scrolled(300.0, 300.0, 0.0, -3.0),
        "the mod list did not accept a scroll"
    );

    let (x, y, width, height) =
        search_box_rect(&bridge, &screen).expect("the screen has no search field");
    let cx = f64::from(x) + f64::from(width) / 2.0;
    let cy = f64::from(y) + f64::from(height) / 2.0;

    // 26.3's `MouseButtonInfo` carries SDL button numbers: left is 1, not
    // GLFW's 0. `AbstractWidget.isValidClickButton` accepts exactly 1.
    assert!(
        bridge.mouse_button(cx, cy, 1, true),
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
    assert!(
        after_texts.contains(&"z"),
        "the typed character was not drawn in the search field: {after_texts:?}"
    );

    // `EditBox.keyPressed` reads Backspace through `KeyEvent.shortcutKey()`,
    // which is the SDL keycode (Backspace is 8), not the scancode in `key`.
    assert!(
        bridge.key_event(42, 8, 0, true),
        "Backspace was not handled by the search field"
    );

    let cleared = bridge
        .render_screen(1280, 720, 0.0)
        .expect("screen render failed")
        .expect("no screen to render");
    let cleared_texts = text_draws(&cleared);
    let cleared_showing = cleared_texts
        .iter()
        .find(|text| text.starts_with("Showing "))
        .expect("the cleared mod count was not rendered")
        .to_string();
    assert_eq!(
        cleared_showing, before_showing,
        "Backspace did not clear the search box and restore the list"
    );
    assert!(
        !cleared_texts.contains(&"z"),
        "the deleted character is still drawn: {cleared_texts:?}"
    );

    // The Done button is a real widget: a click on it must reach the widget
    // and call its press handler, not just the screen's default handler. At
    // 1280x720 it sits at (644, 692) with size 150x20.
    assert!(
        bridge.mouse_button(719.0, 702.0, 1, true),
        "clicking the Done button was not handled"
    );
    assert!(
        bridge.current_screen().is_none(),
        "the Done button did not close the mod screen"
    );
}

#[test]
fn mod_gui_sprites_resolve_from_the_mods_own_jar() {
    let Some(fixture) = Fixture::discover() else {
        return;
    };
    let mut bridge = fixture.bridge();
    bridge.init().unwrap();

    // The widget blits `modmenu:mods/enabled` through `blitSprite`; the image
    // is the mod's own `assets/modmenu/textures/gui/sprites/mods/enabled.png`.
    assert!(
        !bridge
            .prepare_menu_contributions(MenuKind::Title, 427, 240)
            .is_empty()
    );
    let draws = bridge.menu_widget_draw(MenuKind::Title, 427, 240, 0, 0, 0);
    let sprite = draws
        .iter()
        .find_map(|draw| match draw {
            GuiDraw::Blit { texture, .. } if texture.starts_with("modmenu:") => {
                Some(texture.clone())
            }
            _ => None,
        })
        .expect("the title widget blitted no mod sprite");
    let image = bridge
        .gui_texture(&sprite)
        .unwrap_or_else(|| panic!("{sprite} did not resolve from the mod jar"));
    assert!(image.png.starts_with(b"\x89PNG"));
    assert!(image.width > 0 && image.height > 0);

    // Vanilla identifiers belong to the host atlas, and unknown ones stay
    // unresolved instead of inventing an image.
    assert!(bridge.gui_texture("minecraft:widget/button").is_none());
    assert!(bridge.gui_texture("modmenu:no/such_sprite").is_none());
}

#[test]
fn a_jars_declared_identity_is_readable_without_loading_it() {
    let jar = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-mods/modmenu-21.0.0.jar");
    if !jar.is_file() {
        eprintln!("skipping: missing pinned fixture {}", jar.display());
        return;
    }
    let summary = FabricBridge::jar_summary(&jar).expect("the jar declares a mod");
    assert_eq!(summary.id, "modmenu");
    assert!(!summary.name.is_empty() && !summary.version.is_empty());
    assert!(FabricBridge::jar_summary(jar.with_extension("missing")).is_none());
}

#[test]
fn a_mods_own_http_helper_runs_against_the_offline_boundary() {
    let Some(fixture) = Fixture::discover() else {
        return;
    };
    let mut bridge = fixture.bridge();
    bridge.init().unwrap();
    // The mod's real `HttpUtil.request` bytecode builds the request and calls the
    // JDK's `HttpClient.send`; the host has no network, so that one general
    // boundary answers 404. Nothing here is specific to the mod's class.
    let builder = bridge
        .interp
        .execute_method(
            "java/net/http/HttpRequest",
            "newBuilder",
            "()Ljava/net/http/HttpRequest$Builder;",
            &[],
        )
        .expect("the JDK request builder runs");
    // A real `HttpRequestBuilderImpl.build` rejects a builder with no URI, so
    // the request needs one before the offline boundary can answer it.
    let location = Value::Object(Some(
        bridge
            .interp
            .heap
            .alloc_string("https://example.com/mods.json"),
    ));
    let uri = bridge
        .interp
        .execute_method(
            "java/net/URI",
            "create",
            "(Ljava/lang/String;)Ljava/net/URI;",
            &[location],
        )
        .expect("the URI is built");
    bridge
        .interp
        .execute_method(
            "java/net/http/HttpRequest$Builder",
            "uri",
            "(Ljava/net/URI;)Ljava/net/http/HttpRequest$Builder;",
            &[builder.clone(), uri],
        )
        .expect("the builder takes the URI");
    let response = bridge
        .interp
        .execute_method(
            "com/terraformersmc/modmenu/util/HttpUtil",
            "request",
            "(Ljava/net/http/HttpRequest$Builder;Ljava/net/http/HttpResponse$BodyHandler;)Ljava/net/http/HttpResponse;",
            &[builder, Value::NULL],
        )
        .expect("the mod's request helper runs");
    let status = match response.as_obj().and_then(|id| bridge.interp.heap.get(id)) {
        Some(JavaObject::Instance { fields, .. }) => fields.get("statusCode").map(Value::as_int),
        _ => None,
    };
    assert_eq!(status, Some(404));
}

/// Hovering the title widget runs the widget's render with the pointer over it,
/// which reaches LWJGL's static initializers through `Class.forName`. Those walk
/// class hierarchies with `Class.getSuperclass`; a null `Class` must throw as in a
/// JVM, or the walk never ends and the game freezes on hover.
#[test]
fn hovering_a_title_widget_renders_and_returns() {
    let Some(fixture) = Fixture::discover() else {
        return;
    };
    let mut bridge = fixture.bridge();
    bridge.init().unwrap();
    let widgets = bridge.prepare_menu_contributions(MenuKind::Title, 427, 240);
    let widget = &widgets[0];
    for _ in 0..3 {
        let draws =
            bridge.menu_widget_draw(MenuKind::Title, 427, 240, 0, widget.x + 5, widget.y + 5);
        assert!(!draws.is_empty(), "the hovered widget rendered nothing");
    }
}

/// Mod Menu's Give Feedback and Report Bugs buttons open vanilla's
/// `ConfirmLinkScreen`, whose `MultiLineTextWidget` draws each line through
/// `ActiveTextCollector.accept(x, y, FormattedCharSequence)`: the text comes
/// after the position, and the link itself is one of the lines.
#[test]
fn the_confirm_link_screen_draws_its_message_and_link() {
    let Some(fixture) = Fixture::discover() else {
        return;
    };
    let mut bridge = fixture.bridge();
    bridge.init().unwrap();
    assert!(bridge.press_key_mapping("key.modmenu.open_menu"));
    bridge.render_screen(1280, 720, 0.0).unwrap().unwrap();
    let parent = bridge.current_screen().unwrap();
    let link = Value::Object(Some(
        bridge
            .interp
            .heap
            .alloc_string("https://example.com/feedback"),
    ));
    let uri = bridge
        .interp
        .execute_method(
            "java/net/URI",
            "create",
            "(Ljava/lang/String;)Ljava/net/URI;",
            &[link],
        )
        .unwrap();
    bridge
        .interp
        .execute_method(
            "net/minecraft/client/gui/screens/ConfirmLinkScreen",
            "confirmLinkNow",
            "(Lnet/minecraft/client/gui/screens/Screen;Ljava/net/URI;Z)V",
            &[parent, uri, Value::from_bool(false)],
        )
        .unwrap();
    let screen = bridge.current_screen().unwrap();
    assert_eq!(
        screen_class(&bridge, &screen),
        "net/minecraft/client/gui/screens/ConfirmLinkScreen"
    );
    let draws = bridge.render_screen(1280, 720, 0.0).unwrap().unwrap();
    let texts = text_draws(&draws);
    assert!(
        texts.contains(&"https://example.com/feedback"),
        "the link was not drawn: {texts:?}"
    );
    assert!(
        !texts.iter().any(|text| text.parse::<i32>().is_ok()),
        "a coordinate was drawn as text: {texts:?}"
    );
}

/// The confirm-link screen's Open and Copy buttons reach the desktop, which is
/// the host's: each press becomes a request the host performs.
#[test]
fn the_confirm_link_buttons_ask_the_host_to_open_or_copy_the_link() {
    let Some(fixture) = Fixture::discover() else {
        return;
    };
    let link = "https://example.com/feedback";
    for (label, expected) in [
        ("gui.yes", HostRequest::OpenUri(link.to_string())),
        ("chat.copy", HostRequest::SetClipboard(link.to_string())),
    ] {
        let mut bridge = fixture.bridge();
        bridge.init().unwrap();
        assert!(bridge.press_key_mapping("key.modmenu.open_menu"));
        bridge.render_screen(1280, 720, 0.0).unwrap().unwrap();
        let parent = bridge.current_screen().unwrap();
        let text = Value::Object(Some(bridge.interp.heap.alloc_string(link)));
        let uri = bridge
            .interp
            .execute_method(
                "java/net/URI",
                "create",
                "(Ljava/lang/String;)Ljava/net/URI;",
                &[text],
            )
            .unwrap();
        bridge
            .interp
            .execute_method(
                "net/minecraft/client/gui/screens/ConfirmLinkScreen",
                "confirmLinkNow",
                "(Lnet/minecraft/client/gui/screens/Screen;Ljava/net/URI;Z)V",
                &[parent, uri, Value::from_bool(false)],
            )
            .unwrap();
        let draws = bridge.render_screen(1280, 720, 0.0).unwrap().unwrap();
        let (x, y) = draws
            .iter()
            .find_map(|draw| match draw {
                GuiDraw::Text { text, x, y, .. } if text == label => Some((*x, *y)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no {label} button"));
        bridge.take_host_requests();
        bridge.mouse_button(f64::from(x + 3), f64::from(y + 3), 1, true);
        bridge.mouse_button(f64::from(x + 3), f64::from(y + 3), 1, false);
        assert_eq!(bridge.take_host_requests(), vec![expected], "{label}");
    }
}
