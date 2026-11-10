//! A registered block's block entity: made by the block's own
//! `EntityBlock.newBlockEntity`, kept per position, ticked through the block's
//! own `getTicker`, and saved/loaded through its own `saveAdditional`/
//! `loadAdditional`.
//!
//! The fixtures are real mod jars (`test-mods/`), and the expectations come
//! from `javap` of their bytecode, never from the code under test. Without a
//! Minecraft install and the jars, every test skips.

mod common;

use hfc_runtime::bridge::{BlockUse, FabricBridge, HostBridge, RightClickBlockEvent};
use hfc_runtime::minecraft::GuiDraw;
use hfc_runtime::value::Value;
use std::path::PathBuf;

const POT: &str = "farmersdelight:cooking_pot";
const POT_STATE: &str = "facing=north,support=none,waterlogged=false";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf()
}

/// A jar in `test-mods/`, or in its `corpus/` folder (the Farmers Delight
/// fixtures live there).
fn jar(name: &str) -> Option<PathBuf> {
    [root().join("test-mods"), root().join("test-mods/corpus")]
        .into_iter()
        .map(|dir| dir.join(name))
        .find(|path| path.exists())
}

fn farmers_delight() -> Option<FabricBridge> {
    let jars = [
        jar("fabric-api-0.165.0+26.3.jar")?,
        jar("FarmersDelight-26.3-3.6.30+refabricated.jar")?,
    ];
    let mut bridge = common::bridge_with_vanilla()?;
    for jar in &jars {
        bridge.load_mod_jar(jar).unwrap();
    }
    bridge.init().unwrap();
    Some(bridge)
}

/// The world the mod's code sees: the pot at (4, 1, 4), a stove under it when
/// `heated` (which the pot's `isHeated` reads through its heat-source tag), and
/// anything in the acting hand.
struct World {
    heated: bool,
    held: Option<String>,
}

impl HostBridge for World {
    fn block(&self, pos: [i32; 3]) -> String {
        if pos == [4, 1, 4] {
            POT.into()
        } else if self.heated && pos == [4, 0, 4] {
            "farmersdelight:stove".into()
        } else if pos[1] <= 0 {
            "minecraft:grass_block".into()
        } else {
            "minecraft:air".into()
        }
    }
    fn block_property(&self, pos: [i32; 3], key: &str) -> Option<String> {
        if pos != [4, 1, 4] {
            return None;
        }
        let value = match key {
            "facing" => "north",
            "support" => "none",
            "waterlogged" => "false",
            _ => return None,
        };
        Some(value.to_string())
    }
    fn set_block_property(&mut self, _: [i32; 3], _: &str, _: &str) -> bool {
        true
    }
    fn give(&mut self, _: &str, _: u32) -> Result<(), String> {
        Ok(())
    }
    fn block_properties(&self, pos: [i32; 3]) -> Vec<(String, String)> {
        if pos != [4, 1, 4] {
            return Vec::new();
        }
        [
            ("facing", "north"),
            ("support", "none"),
            ("waterlogged", "false"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .to_vec()
    }
    fn held_item(&self) -> Option<String> {
        self.held.clone()
    }
}

fn click() -> RightClickBlockEvent {
    RightClickBlockEvent {
        pos: [4, 1, 4],
        face: [0, 1, 0],
        hit: [4.5, 1.5, 4.5],
        sneaking: false,
        off_hand: false,
    }
}

/// The pot's block entity is the mod's own object: `newBlockEntity` makes a
/// `CookingPotBlockEntity`, kept for the position, and its own `getInventory`
/// answers an `ItemStackHandler` that holds what is put in it.
#[test]
fn a_blocks_new_block_entity_is_the_mods_own_object_and_keeps_its_contents() {
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    let world = World {
        heated: false,
        held: None,
    };
    assert!(
        bridge.ensure_block_entity(&world, [4, 1, 4], POT),
        "the pot made no block entity"
    );
    // javap of CookingPotBlock.newBlockEntity: `new CookingPotBlockEntity(pos,
    // state)`, whose inventory is 4 slots (MEAL_DISPLAY 0, INPUT[0..2],
    // CONTAINER 2... the class's own `createHandler`). The container is the
    // mod's, not a vanilla `BaseContainerBlockEntity`, so the facts say so.
    let block = bridge
        .registered_blocks()
        .into_iter()
        .find(|b| b.id == POT)
        .expect("the pot is registered");
    assert!(block.has_block_entity);
    assert!(
        matches!(
            block.container,
            hfc_runtime::bridge::ContainerRead::NotVanillaContainer(_)
        ),
        "{:?}",
        block.container
    );

    // The entity's own inventory keeps a stack, through its own methods.
    // javap of `createHandler`: `new ItemStackHandler(9)`.
    let slots = bridge.debug_block_entity_inventory_slots([4, 1, 4], POT);
    assert_eq!(
        slots,
        Some(9),
        "CookingPotBlockEntity's inventory is 9 slots"
    );
    bridge.debug_set_block_entity_inventory([4, 1, 4], POT, 0, "minecraft:beef", 2);
    assert_eq!(
        bridge.debug_block_entity_inventory([4, 1, 4], POT, 0),
        Some(("minecraft:beef".into(), 2))
    );

    // A second `ensure` keeps the one object: its contents are still there.
    assert!(bridge.ensure_block_entity(&world, [4, 1, 4], POT));
    assert_eq!(
        bridge.debug_block_entity_inventory([4, 1, 4], POT, 0),
        Some(("minecraft:beef".into(), 2))
    );
}

/// The pot's `useItemOn` runs on an empty hand too (vanilla's
/// `ServerPlayerGameMode.useItemOn` calls it with the empty stack): it reads its
/// own block entity and answers `SUCCESS`, so the click is consumed instead of
/// the host's own path. An item in the hand runs the same method with that
/// stack.
#[test]
fn the_pots_own_use_runs_with_an_empty_hand_and_consumes_the_click() {
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    for held in [None, Some("minecraft:bowl".to_string())] {
        let world = World {
            heated: false,
            held: held.clone(),
        };
        let outcome = bridge.block_used(&world, &click());
        assert!(
            matches!(outcome, BlockUse::Handled(_)),
            "held {held:?}: {outcome:?}"
        );
    }
    // The click made the block's entity, which the pot's use read.
    assert!(
        bridge
            .interp
            .heap
            .kept_block_entities()
            .contains_key(&([4, 1, 4], POT.to_string())),
        "the use did not make the block entity"
    );
}

/// The pot's ticker is its own: `getTicker` answers a `BlockEntityTicker` on the
/// server, and ticking it runs `CookingPotBlockEntity.cookingTick`, which reads
/// the heat below (`isHeated`) and keeps `cookTime` inside `0..cookTimeTotal`.
#[test]
fn the_pots_ticker_runs_its_own_cooking_tick() {
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    let world = World {
        heated: true,
        held: None,
    };
    assert!(bridge.ensure_block_entity(&world, [4, 1, 4], POT));
    // With nothing in it, `hasInput()` is false: the tick clamps `cookTime`
    // to 0 rather than cooking, which is still the mod's own code running.
    bridge.debug_set_block_entity_field([4, 1, 4], POT, "cookTimeTotal", 200);
    bridge.debug_set_block_entity_field([4, 1, 4], POT, "cookTime", 10);
    let effects = bridge
        .tick_block_entity(&world, [4, 1, 4], POT)
        .expect("the pot's ticker ran");
    assert!(effects.changes.is_empty(), "{effects:?}");
    // javap of `cookingTick`, the not-heated-and-no-recipe arm: `cookTime =
    // Mth.clamp(cookTime - 2, 0, cookTimeTotal)`.
    assert_eq!(
        bridge.debug_block_entity_field([4, 1, 4], POT, "cookTime"),
        Some(8)
    );

    // `tick_block_entities` walks the kept entities and ticks each whose block
    // is still there; a broken pot's entity is dropped, not run.
    let mut world = World {
        heated: true,
        held: None,
    };
    assert_eq!(bridge.tick_block_entities(&mut world), 1);
    world.heated = false;
    // The block is gone: the kept entity goes with it.
    struct Gone;
    impl HostBridge for Gone {
        fn block(&self, _: [i32; 3]) -> String {
            "minecraft:air".into()
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
    }
    assert_eq!(bridge.tick_block_entities(&mut Gone), 0);
    assert!(bridge.interp.heap.kept_block_entities().is_empty());
}

/// `saveAdditional` writes the pot's own fields through `ValueOutput`, and
/// `loadAdditional` reads them back through `ValueInput`: `CookTime`,
/// `CookTimeTotal` and the nested `Inventory`.
#[test]
fn the_pots_save_and_load_runs_its_own_methods() {
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    let world = World {
        heated: false,
        held: None,
    };
    assert!(bridge.ensure_block_entity(&world, [4, 1, 4], POT));
    let saved = bridge
        .save_block_entity([4, 1, 4], POT)
        .expect("the pot's save ran");
    // javap of CookingPotBlockEntity.saveAdditional: putInt("CookTime"),
    // putInt("CookTimeTotal"), storeNullable("Container"), ("CustomName") and
    // the inventory under child("Inventory").
    let lookup = |key: &str| {
        saved
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(
        lookup("CookTime"),
        Some(hfc_runtime::value_io_builtins::StoredValue::Int(0))
    );
    assert_eq!(
        lookup("CookTimeTotal"),
        Some(hfc_runtime::value_io_builtins::StoredValue::Int(0))
    );
    // `ItemStackHandler.serialize` writes the slots through
    // `ContainerHelper.saveAllItems` -> `ValueOutput.list(name, codec)`, a
    // `Codec` list this runtime does not model: the `Inventory` child is made
    // (its `child("Inventory")` call is answered) but holds no slots, so the
    // saved tree is the two ints. The next line is the gap, not a success:
    // when codec lists are modelled it will hold the slots too.
    assert_eq!(
        saved
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["CookTime", "CookTimeTotal"]
    );

    // Into a fresh entity the same values load back.
    let loaded = bridge.load_block_entity(&world, [4, 1, 4], POT, saved);
    assert!(loaded, "the pot's load did not run");
}

/// The state a block entity saves is real: a value written into the pot's
/// `CookTime` field comes back from `saveAdditional` unchanged.
#[test]
fn a_saved_field_is_the_one_the_entity_holds() {
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    let world = World {
        heated: false,
        held: None,
    };
    assert!(bridge.ensure_block_entity(&world, [4, 1, 4], POT));
    bridge.debug_set_block_entity_field([4, 1, 4], POT, "cookTime", 37);
    let saved = bridge.save_block_entity([4, 1, 4], POT).unwrap();
    assert_eq!(
        saved
            .iter()
            .find(|(name, _)| name == "CookTime")
            .map(|(_, value)| value.clone()),
        Some(hfc_runtime::value_io_builtins::StoredValue::Int(37))
    );
}

/// The value tree itself: `putInt`/`getIntOr`, a nested `child`, `putString`,
/// `putBoolean` and `discard` are one implementation for every block entity that
/// saves this way, not the pot's.
#[test]
fn the_value_tree_round_trips_primitives_and_children() {
    let mut heap = hfc_runtime::heap::Heap::new();
    let output = hfc_runtime::value_io_builtins::alloc_output(&mut heap);
    let this = Value::Object(Some(output));
    let name = |heap: &mut hfc_runtime::heap::Heap, text: &str| {
        Value::Object(Some(heap.alloc_string(text.to_string())))
    };
    let key = name(&mut heap, "CookTime");
    hfc_runtime::value_io_builtins::dispatch(
        &mut heap,
        "net/minecraft/world/level/storage/ValueOutput",
        "putInt",
        &[this.clone(), key, Value::Int(120)],
    );
    let key = name(&mut heap, "Custom");
    let pot = name(&mut heap, "a pot");
    hfc_runtime::value_io_builtins::dispatch(
        &mut heap,
        "net/minecraft/world/level/storage/ValueOutput",
        "putString",
        &[this.clone(), key, pot],
    );
    let key = name(&mut heap, "Lit");
    hfc_runtime::value_io_builtins::dispatch(
        &mut heap,
        "net/minecraft/world/level/storage/ValueOutput",
        "putBoolean",
        &[this.clone(), key, Value::from_bool(true)],
    );
    let key = name(&mut heap, "Gone");
    hfc_runtime::value_io_builtins::dispatch(
        &mut heap,
        "net/minecraft/world/level/storage/ValueOutput",
        "putInt",
        &[this.clone(), key.clone(), Value::Int(5)],
    );
    hfc_runtime::value_io_builtins::dispatch(
        &mut heap,
        "net/minecraft/world/level/storage/ValueOutput",
        "discard",
        &[this.clone(), key],
    );
    let values = hfc_runtime::value_io_builtins::output_values(&heap, output)
        .cloned()
        .unwrap();
    assert_eq!(values.len(), 3);
    assert_eq!(
        values.get("CookTime"),
        Some(&hfc_runtime::value_io_builtins::StoredValue::Int(120))
    );
    assert_eq!(
        values.get("Custom"),
        Some(&hfc_runtime::value_io_builtins::StoredValue::Text(
            "a pot".into()
        ))
    );
    assert_eq!(
        values.get("Lit"),
        Some(&hfc_runtime::value_io_builtins::StoredValue::Bool(true))
    );

    // The same values read back through a `ValueInput`.
    let input = hfc_runtime::value_io_builtins::alloc_input(&mut heap, values);
    let this = Value::Object(Some(input));
    let key = Value::Object(Some(heap.alloc_string("CookTime".to_string())));
    let read = hfc_runtime::value_io_builtins::dispatch(
        &mut heap,
        "net/minecraft/world/level/storage/ValueInput",
        "getIntOr",
        &[this, key, Value::Int(0)],
    );
    assert_eq!(read, Some(Value::Int(120)));
}

/// The pot's own menu is built by its `createMenu`: a 45-slot
/// `CookingPotMenu` titled "Cooking Pot", whose input slots take a stack and
/// whose result slot does not.
#[test]
fn the_pots_own_menu_builds_from_its_create_menu() {
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    let mut world = World {
        heated: false,
        held: None,
    };
    // The host's own path: the click runs the block's use and keeps the menu
    // it opened for the host to show.
    assert_eq!(
        bridge.right_click_block(&mut world, &click()),
        hfc_runtime::bridge::Flow::Cancel,
        "the pot's use did not consume the click"
    );
    let taken = bridge.take_opened_menu();
    let (block, menu) = taken.expect("the pot opened no menu");
    assert_eq!(block, POT);
    // javap of CookingPotMenu's constructor: `super(MenuType.COOKING_POT, id)`,
    // then 1 meal display + 6 input + 1 container + 1 result + 9x3 player
    // inventory + 9 hotbar = 45 slots.
    assert_eq!(menu.title, "Cooking Pot");
    assert_eq!(menu.slots, 45);
    // The result slot (6) takes nothing; the inputs (the first seven slots of
    // the pot's own grid) do.
    assert!(!menu.input.contains(&6), "{:?}", menu.input);
    assert!(menu.input.contains(&9), "{:?}", menu.input);
    // The menu is handed to the host once.
    assert!(bridge.take_opened_menu().is_none());
}

/// The stacks the host's menu shows reach the pot's own inventory, which is
/// what its cooking tick reads.
#[test]
fn a_menu_click_reaches_the_pots_own_inventory() {
    use hfc_runtime::bridge::BlockUse;
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    let world = World {
        heated: true,
        held: None,
    };
    let BlockUse::Handled(_) = bridge.block_used(&world, &click()) else {
        panic!("the pot's use did not run");
    };
    // Slot 0 is the pot's first input (`MEAL_DISPLAY`, `INPUT[0]` after the
    // meal display in `CookingPotBlockEntity`'s handler order).
    let written = bridge.sync_menu_items([4, 1, 4], POT, &[Some(("minecraft:beef".into(), 2))]);
    assert_eq!(written, 1, "the menu's first slot was not written");
    assert_eq!(
        bridge.debug_block_entity_inventory([4, 1, 4], POT, 0),
        Some(("minecraft:beef".into(), 2))
    );
}

/// When the pot's menu type is registered with the client's own `MenuScreens`,
/// opening it builds and shows the mod's real screen (its own background
/// texture, slot outlines and heat gauge), not a host-drawn grid. The screen
/// comes from the menu's own `getType`, so this exercises the general path:
/// any menu whose type the client knows a screen for opens that screen.
#[test]
fn a_registered_menu_type_opens_the_mods_own_screen() {
    use hfc_runtime::value::JavaObject;
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    let mut world = World {
        heated: false,
        held: None,
    };
    // The pot's own use runs and opens its menu; with a `MenuScreens` entry
    // for the pot's `MenuType`, the host also shows the mod's real screen.
    assert_eq!(
        bridge.right_click_block(&mut world, &click()),
        hfc_runtime::bridge::Flow::Cancel,
        "the pot's use did not consume the click"
    );
    let (block, menu) = bridge.take_opened_menu().expect("the pot opened no menu");
    assert_eq!(block, POT);
    assert_eq!(menu.title, "Cooking Pot");

    // A real screen is now open, and it is the mod's own `CookingPotScreen`,
    // built by the client's registered constructor for the pot's menu type.
    let Some(screen) = bridge.current_screen() else {
        panic!("opening the pot's menu did not open a screen");
    };
    let Some(id) = screen.as_obj() else {
        panic!("the open screen is not an object");
    };
    let class = match bridge.interp.heap.get(id) {
        Some(JavaObject::Instance { class_name, .. }) => class_name.as_str(),
        other => panic!("the open screen has no class: {other:?}"),
    };
    assert!(
        class.contains("CookingPotScreen"),
        "the pot's screen is {class}, not the mod's CookingPotScreen"
    );
    assert!(bridge.has_screen());

    // Rendering the open screen draws the mod's own background texture (the
    // pot's `cooking_pot.png`), not a host grid: the real screen's `blit` of
    // `BACKGROUND_TEXTURE` is in the draw list.
    let draws = bridge
        .render_screen(1280, 720, 0.0)
        .expect("the pot screen failed to render")
        .expect("no draws from the open pot screen");
    assert!(
        draws.iter().any(|draw| matches!(
            draw,
            GuiDraw::Blit { texture, .. } if texture.contains("cooking_pot")
        )),
        "the pot screen's draws miss its own background texture: {draws:?}"
    );
}
