//! Legendary Tooltips draws its own tooltip frame from Iceberg's
//! `RenderTooltipEvents.POSTEXT`: `TooltipDecor.drawBorder` slices
//! `legendarytooltips:textures/gui/tooltip_borders.png` into corners and
//! edges and blits each part. The frame is selected by the item's rarity
//! through Iceberg's `Selectors.itemMatches`, which reads
//! `ItemStack.getRarity()`.
//!
//! The test loads the pinned jars, lets the mod write and load its default
//! config, then drives the same bridge calls the client does. It needs the
//! jars in `test-mods/` and a Minecraft install; without them it skips.

mod common;

use hfc_runtime::minecraft::GuiDraw;
use std::path::PathBuf;

struct Tooltip {
    draws: Vec<GuiDraw>,
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
    let draws = bridge.item_tooltip_frame(
        "minecraft:diamond_sword",
        Some(rarity),
        640,
        360,
        100,
        100,
        120,
        40,
    );
    let _ = std::fs::remove_dir_all(&dir);
    Some(Tooltip {
        draws,
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
fn epic_item_draws_the_legendary_frame() {
    let Some(tooltip) = tooltip("epic") else {
        return;
    };
    let blits = frame_blits(&tooltip.draws);
    let frame: Vec<_> = blits
        .iter()
        .filter(|(texture, ..)| texture.contains("tooltip_borders"))
        .collect();
    assert!(
        frame.len() >= 4,
        "expected at least the frame's four corners, got {:?}",
        blits.iter().map(|b| b.0).collect::<Vec<_>>()
    );
    // A frame is drawn as its corners and edges, not the whole sheet: at
    // least one blit samples a sub-rectangle of the texture.
    assert!(
        frame
            .iter()
            .any(|(_, u0, u1, v0, v1)| (*u0, *u1, *v0, *v1) != (0.0, 1.0, 0.0, 1.0)),
        "the frame blits sample the whole sheet instead of its parts: {frame:?}"
    );
}

#[test]
fn common_item_has_no_custom_frame_but_real_colors() {
    let Some(tooltip) = tooltip("common") else {
        return;
    };
    let blits = frame_blits(&tooltip.draws);
    assert!(
        blits
            .iter()
            .all(|(texture, ..)| !texture.contains("tooltip_borders")),
        "a common item selected a custom frame: {:?}",
        blits.iter().map(|b| b.0).collect::<Vec<_>>()
    );
    // Rarity-matched colors come through Prism's DynamicColor: a fallback
    // TextColor read as an `IColor`. Both used to come back 0 (a fully
    // transparent tooltip) when `List.of` or the TextColor adapter was
    // missing.
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
