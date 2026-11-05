//! Legendary Tooltips builds its tooltip from Minecraft's real
//! `ClientTooltipComponent` list: Iceberg's `GATHER` adds the item-model and
//! padding components, Legendary's `tooltip` mixin centres the title, and
//! `POSTEXT` draws the frame and name separator. The item model comes from
//! Iceberg's `CustomItemRenderer`.
//!
//! The test loads the pinned jars, lets the mod write and load its default
//! config, then drives the same bridge calls the client does. It needs the
//! jars in `test-mods/` and a Minecraft install; without them it skips.

mod common;

use hfc_runtime::bridge::TooltipRender;
use hfc_runtime::minecraft::GuiDraw;
use std::path::PathBuf;

struct Tooltip {
    render: TooltipRender,
    background: u32,
    border_top: u32,
    border_bottom: u32,
}

fn tooltip(rarity: &str) -> Option<Tooltip> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf();
    let jars = [
        "fabric-api-0.165.0+26.3.jar",
        "Iceberg-26.3-fabric-1.4.3.1.jar",
        "Prism-26.3-fabric-1.1.3.jar",
        "LegendaryTooltips-26.3-fabric-1.6.3.jar",
    ]
    .map(|name| root.join("test-mods").join(name));
    if jars.iter().any(|jar| !jar.exists()) {
        eprintln!("skipped: test-mods jars are not present");
        return None;
    }
    let mut bridge = common::bridge_with_vanilla()?;
    for jar in &jars {
        bridge.load_mod_jar(jar).unwrap();
    }
    let dir = std::env::temp_dir().join(format!(
        "hfc-legendary-tooltips-{}-{rarity}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    bridge.set_game_dir(Some(dir.clone()));
    bridge.init().unwrap();

    let (mut background, mut border_top, mut border_bottom) =
        (0xf0100010u32, 0x505000ffu32, 0x5028007fu32);
    bridge.item_tooltip_color(
        Some("minecraft:diamond_sword"),
        Some(rarity),
        &mut background,
        &mut border_top,
        &mut border_bottom,
    );
    let lines = vec![
        ("Diamond Sword".to_string(), 0xFF55FFFFu32),
        ("Efficiency V".to_string(), 0xFFAAAAAAu32),
        (String::new(), 0xFFAAAAAAu32),
        ("When in Main Hand:".to_string(), 0xFFAAAAAAu32),
        (" 5 Attack Damage".to_string(), 0xFF55FF55u32),
        (" 1.2 Attack Speed".to_string(), 0xFF55FF55u32),
    ];
    let render = bridge.item_tooltip_render(
        "minecraft:diamond_sword",
        Some(rarity),
        1561,
        &lines,
        640,
        360,
    );
    let _ = std::fs::remove_dir_all(&dir);
    render.map(|render| Tooltip {
        render,
        background,
        border_top,
        border_bottom,
    })
}

fn frame_blits(draws: &[GuiDraw]) -> Vec<(&str, f32, f32, f32, f32)> {
    draws
        .iter()
        .filter_map(|draw| match draw {
            GuiDraw::Blit {
                texture,
                u0,
                u1,
                v0,
                v1,
                ..
            } => Some((texture.as_str(), *u0, *u1, *v0, *v1)),
            _ => None,
        })
        .collect()
}

#[test]
fn epic_item_draws_the_legendary_frame_and_title() {
    let Some(tooltip) = tooltip("epic") else {
        return;
    };
    let draws = &tooltip.render.draws;
    let blits = frame_blits(draws);
    let frame: Vec<_> = blits
        .iter()
        .filter(|(texture, ..)| texture.contains("tooltip_borders"))
        .collect();
    assert!(
        frame.len() >= 4,
        "expected at least the frame's four corners, got {:?}",
        blits.iter().map(|b| b.0).collect::<Vec<_>>()
    );
    assert!(
        frame
            .iter()
            .any(|(_, u0, u1, v0, v1)| (*u0, *u1, *v0, *v1) != (0.0, 1.0, 0.0, 1.0)),
        "the frame blits sample the whole sheet instead of its parts: {frame:?}"
    );

    // GATHER adds Legendary's item-model component; its extractImage asks
    // Iceberg's item renderer to draw this stack.
    assert!(
        draws.iter().any(|draw| matches!(
            draw,
            GuiDraw::Item { item, .. } if item == "minecraft:diamond_sword"
        )),
        "the item-model component recorded no item draw: {draws:#?}"
    );

    // The title component is centred by Legendary's tooltip HEAD injector:
    // Iceberg pads it with spaces to the tooltip's width.
    let (title, title_color) = draws
        .iter()
        .find_map(|draw| match draw {
            GuiDraw::Text { text, color, .. } if text.contains("Diamond Sword") => {
                Some((text.as_str(), *color))
            }
            _ => None,
        })
        .expect("the title line was not drawn");
    assert_ne!(title, "Diamond Sword", "the title was not padded");
    assert_eq!(title.trim(), "Diamond Sword");
    assert_eq!(
        title_color, 0xFF55FFFFu32 as i32,
        "the title lost its color"
    );

    // `drawShadow` draws five one-pixel gradient rectangles; the name
    // separator adds two more, so seven fills means it was drawn.
    let fills = draws
        .iter()
        .filter(|draw| matches!(draw, GuiDraw::Fill { .. }))
        .count();
    assert!(fills >= 7, "the name separator did not draw: {draws:#?}");
}

#[test]
fn render_ticks_advance_the_shine_timer() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf();
    let jars = [
        "fabric-api-0.165.0+26.3.jar",
        "Iceberg-26.3-fabric-1.4.3.1.jar",
        "Prism-26.3-fabric-1.1.3.jar",
        "LegendaryTooltips-26.3-fabric-1.6.3.jar",
    ]
    .map(|name| root.join("test-mods").join(name));
    if jars.iter().any(|jar| !jar.exists()) {
        return;
    }
    let Some(mut bridge) = common::bridge_with_vanilla() else {
        return;
    };
    for jar in &jars {
        bridge.load_mod_jar(jar).unwrap();
    }
    let dir = std::env::temp_dir().join(format!("hfc-legendary-ticks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    bridge.set_game_dir(Some(dir.clone()));
    bridge.init().unwrap();
    let lines = vec![("Diamond Sword".to_string(), 0xFF55FFFFu32)];
    let render = |bridge: &mut hfc_runtime::bridge::FabricBridge| {
        bridge.item_tooltip_render(
            "minecraft:diamond_sword",
            Some("epic"),
            1561,
            &lines,
            640,
            360,
        )
    };
    let shine = |bridge: &mut hfc_runtime::bridge::FabricBridge| {
        bridge
            .interp
            .resolve_static_field(
                "com/anthonyhilyard/legendarytooltips/tooltip/TooltipDecor",
                "shineTimer",
            )
            .as_float()
    };
    // The first frame with time passing is also the first hover of this
    // stack, which restarts the shine; measure from after that.
    assert!(render(&mut bridge).is_some());
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert!(render(&mut bridge).is_some());
    let first_shine = shine(&mut bridge);
    std::thread::sleep(std::time::Duration::from_millis(80));
    assert!(render(&mut bridge).is_some());
    assert!(
        shine(&mut bridge) < first_shine,
        "the shine timer did not advance with real time"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn common_item_has_no_custom_frame_but_real_colors() {
    let Some(tooltip) = tooltip("common") else {
        return;
    };
    let blits = frame_blits(&tooltip.render.draws);
    assert!(
        blits
            .iter()
            .all(|(texture, ..)| !texture.contains("tooltip_borders")),
        "a common item selected a custom frame: {:?}",
        blits.iter().map(|b| b.0).collect::<Vec<_>>()
    );
    assert_ne!(
        tooltip.background, 0,
        "the common tooltip background is fully transparent"
    );
    assert_ne!(
        tooltip.border_top, 0,
        "the common tooltip border is fully transparent"
    );
    assert_ne!(tooltip.border_bottom, 0);
}

#[test]
fn rotated_gradients_land_inside_the_tooltip() {
    let Some(tooltip) = tooltip("epic") else {
        return;
    };
    let render = &tooltip.render;
    let gradients: Vec<_> = render
        .draws
        .iter()
        .filter_map(|draw| match draw {
            GuiDraw::Gradient {
                x1,
                y1,
                x2,
                y2,
                horizontal,
                ..
            } => Some((*x1, *y1, *x2, *y2, *horizontal)),
            _ => None,
        })
        .collect();
    assert!(
        gradients.iter().any(|g| g.4),
        "no horizontal gradient was recorded: {gradients:?}"
    );
    for &(x1, y1, x2, y2, horizontal) in &gradients {
        assert!(x2 > 0, "gradient left of the tooltip: {gradients:?}");
        if horizontal {
            assert!(
                x1 >= -6 && x2 <= render.width + 6 && y1 >= -6 && y2 <= render.height + 6,
                "horizontal gradient {x1},{y1},{x2},{y2} outside the {}x{} frame",
                render.width,
                render.height
            );
        }
    }
}

/// Legendary restarts its shine when the hovered slot holds a different
/// stack (`onRenderTick` reads `Gui.screen().hoveredSlot`), and draws the
/// shine as a white horizontal sweep along the frame's top edge while the
/// timer runs.
#[test]
fn hovering_another_item_restarts_and_draws_the_shine() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf();
    let jars = [
        "fabric-api-0.165.0+26.3.jar",
        "Iceberg-26.3-fabric-1.4.3.1.jar",
        "Prism-26.3-fabric-1.1.3.jar",
        "LegendaryTooltips-26.3-fabric-1.6.3.jar",
    ]
    .map(|name| root.join("test-mods").join(name));
    if jars.iter().any(|jar| !jar.exists()) {
        return;
    }
    let Some(mut bridge) = common::bridge_with_vanilla() else {
        return;
    };
    for jar in &jars {
        bridge.load_mod_jar(jar).unwrap();
    }
    let dir = std::env::temp_dir().join(format!("hfc-legendary-shine-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    bridge.set_game_dir(Some(dir.clone()));
    bridge.init().unwrap();
    let render = |bridge: &mut hfc_runtime::bridge::FabricBridge, item: &str, name: &str| {
        bridge
            .item_tooltip_render(
                item,
                Some("epic"),
                1561,
                &[(name.to_string(), 0xFF55FFFFu32)],
                640,
                360,
            )
            .expect("no tooltip render")
    };
    let shine = |bridge: &mut hfc_runtime::bridge::FabricBridge| {
        bridge
            .interp
            .resolve_static_field(
                "com/anthonyhilyard/legendarytooltips/tooltip/TooltipDecor",
                "shineTimer",
            )
            .as_float()
    };
    let pause = |ms| std::thread::sleep(std::time::Duration::from_millis(ms));

    render(&mut bridge, "minecraft:diamond_sword", "Diamond Sword");
    pause(30);
    render(&mut bridge, "minecraft:diamond_sword", "Diamond Sword");
    pause(600);
    render(&mut bridge, "minecraft:diamond_sword", "Diamond Sword");
    let before = shine(&mut bridge);
    pause(30);
    // The host now shows another item's tooltip: a new hovered stack.
    render(&mut bridge, "minecraft:diamond_pickaxe", "Diamond Pickaxe");
    let restarted = shine(&mut bridge);
    assert!(
        restarted > before && restarted > 2.0,
        "hovering another item did not restart the shine: {before} -> {restarted}"
    );
    // Same item again: no restart, the timer keeps running down.
    pause(700);
    let draws = render(&mut bridge, "minecraft:diamond_pickaxe", "Diamond Pickaxe").draws;
    let running = shine(&mut bridge);
    assert!(running < restarted, "the shine restarted on the same stack");
    let sweep = draws.iter().any(|draw| {
        matches!(
            draw,
            GuiDraw::Gradient { horizontal: true, color_from, color_to, y1, .. }
                if *y1 == -3
                    && (*color_from as u32 & 0x00FF_FFFF) == 0x00FF_FFFF
                    && (*color_to as u32 & 0x00FF_FFFF) == 0x00FF_FFFF
        )
    });
    assert!(
        sweep,
        "no shine sweep along the top edge at timer {running}: {draws:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
