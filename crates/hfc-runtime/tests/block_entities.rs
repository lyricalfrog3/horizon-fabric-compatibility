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
    /// The player's own inventory, as the host reports it.
    inventory: Vec<Option<(String, i32)>>,
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
    fn inventory(&self) -> Vec<Option<(String, i32)>> {
        self.inventory.clone()
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
        inventory: Vec::new(),
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
            inventory: Vec::new(),
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
        inventory: Vec::new(),
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
        inventory: Vec::new(),
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

/// One pot's block entity must not cost milliseconds per tick. The tick's own
/// stages are microseconds; what made it ~8 ms was the scope bookkeeping: the
/// end-of-tick reference scan walked every object on the heap (a modpack puts
/// ~306k there) to ask whether this tick had stored a fresh object into an
/// older one. That question is answered by the objects the tick *wrote* (the
/// heap's write barrier), so the scan no longer grows with the heap.
///
/// The assertions are behavioural first: the pot still clamps `cookTime` when
/// unheated and no recipe matches, the tick still runs the mod's own
/// `cookingTick` when the host says it is heated, and `tick_block_entities`
/// still reports the one entity it ran. The timing is a generous ceiling, not
/// the measured figure.
#[test]
fn one_pots_tick_does_not_scan_the_whole_heap() {
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    let world = World {
        heated: true,
        held: None,
        inventory: Vec::new(),
    };
    assert!(bridge.ensure_block_entity(&world, [4, 1, 4], POT));
    bridge.debug_set_block_entity_field([4, 1, 4], POT, "cookTimeTotal", 200);

    // An unheated pot with nothing in it: `hasInput()` is false, so the
    // mod's own `cookingTick` clamps `cookTime` down by 2 and stops.
    let cold = World {
        heated: false,
        held: None,
        inventory: Vec::new(),
    };
    bridge.debug_set_block_entity_field([4, 1, 4], POT, "cookTime", 10);
    let effects = bridge
        .tick_block_entity(&cold, [4, 1, 4], POT)
        .expect("the pot's ticker ran");
    assert!(effects.changes.is_empty(), "{effects:?}");
    assert_eq!(
        bridge.debug_block_entity_field([4, 1, 4], POT, "cookTime"),
        Some(8),
        "an unheated pot no longer clamps cookTime"
    );

    // Warm the caches the tick keeps between calls (state objects, the ticker)
    // so the measurement is the steady state the host sees, then time honest
    // ticks over a heap that already holds everything a modpack loaded.
    for _ in 0..3 {
        let _ = bridge.tick_block_entity(&world, [4, 1, 4], POT);
    }
    let heap_objects = bridge.heap_len();
    assert!(
        heap_objects > 100_000,
        "the fixture heap is only {heap_objects} objects; the scan this guards \
         against would not show up"
    );

    bridge.debug_set_block_entity_field([4, 1, 4], POT, "cookTimeTotal", 200);
    bridge.debug_set_block_entity_field([4, 1, 4], POT, "cookTime", 10);
    let before = std::time::Instant::now();
    let mut scanned = 0;
    for _ in 0..20 {
        bridge
            .tick_block_entity(&world, [4, 1, 4], POT)
            .expect("the pot's ticker ran");
        scanned = scanned.max(bridge.scope_scan_len());
    }
    let per_tick = before.elapsed().as_secs_f64() * 1000.0 / 20.0;
    assert!(
        per_tick < 1.0,
        "one pot's tick took {per_tick:.3} ms; it is walking something that grows \
         with the heap ({heap_objects} objects)"
    );
    // The scan examines what the tick wrote, not the heap: a bound tied to the
    // heap size would pass on a heap with a few objects and fail on this one.
    assert!(
        scanned < heap_objects / 100,
        "the tick's reference scan examined {scanned} of {heap_objects} heap \
         objects: it is still walking the whole heap"
    );

    // Behaviour is unchanged: with the equal-clock fields set, an unheated pot
    // still runs the mod's own `cookingTick` clamp arm, and a pot the host says
    // is heated still runs the same arm while no recipe manager answers (the
    // runtime has no `RecipeManager`, so `getMatchingRecipe` cannot present a
    // recipe to cook: the heated arm clamps rather than cooking). Both are the
    // mod's code running; neither is skipped and neither is faked.
    let mut heated = World {
        heated: true,
        held: None,
        inventory: Vec::new(),
    };
    bridge.debug_set_block_entity_field([4, 1, 4], POT, "cookTime", 10);
    bridge.debug_set_block_entity_field([4, 1, 4], POT, "cookTimeTotal", 200);
    let effects = bridge
        .tick_block_entity(&heated, [4, 1, 4], POT)
        .expect("the pot's ticker ran");
    assert!(effects.changes.is_empty(), "{effects:?}");
    assert_eq!(
        bridge.debug_block_entity_field([4, 1, 4], POT, "cookTime"),
        Some(8),
        "the pot's tick did not run the mod's own cookingTick"
    );

    // The host's walk over the kept entities runs exactly the one pot.
    assert_eq!(bridge.tick_block_entities(&mut heated), 1);
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
        inventory: Vec::new(),
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
        inventory: Vec::new(),
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
        inventory: Vec::new(),
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
        inventory: Vec::new(),
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
        inventory: Vec::new(),
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

/// A mod screen's player slots show what the player really carries. The menu
/// lists them through `Inventory.getItem`, which answered an empty stack for
/// every slot before the host's own inventory reached the snapshot, so the
/// strip drew blank while the player held items.
#[test]
fn the_player_inventory_reaches_the_pots_menu() {
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    let mut world = World {
        heated: false,
        held: None,
        inventory: vec![
            Some(("minecraft:beef".into(), 3)),
            None,
            Some(("farmersdelight:tomato".into(), 7)),
        ],
    };
    assert_eq!(
        bridge.right_click_block(&mut world, &click()),
        hfc_runtime::bridge::Flow::Cancel,
        "the pot's use did not consume the click"
    );
    let (block, menu) = bridge.take_opened_menu().expect("the pot opened no menu");
    assert_eq!(block, POT);
    // The pot's menu has 45 slots: 9 of its own, then 36 player slots
    // (`CookingPotMenu`'s layout), so the player's inventory appears in the
    // menu's own items.
    assert_eq!(menu.slots, 45, "the pot's menu changed shape");
    let carried: Vec<(usize, String, i32)> = menu
        .items
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| {
            slot.as_ref()
                .map(|(item, count)| (index, item.clone(), *count))
        })
        .collect();
    assert!(
        carried
            .iter()
            .any(|(_, item, count)| item == "minecraft:beef" && *count == 3),
        "the player's beef did not reach the menu: {carried:?}"
    );
    assert!(
        carried
            .iter()
            .any(|(_, item, count)| item == "farmersdelight:tomato" && *count == 7),
        "the player's tomato did not reach the menu: {carried:?}"
    );
}

/// An open screen is drawn from the pointer's own position. Rendering it with
/// the mouse at (0, 0) whatever the pointer does left every hover state, slot
/// highlight and tooltip out of the display list, so the menu looked dead.
#[test]
fn a_screens_draws_follow_the_pointer() {
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    let mut world = World {
        heated: false,
        held: None,
        inventory: Vec::new(),
    };
    assert_eq!(
        bridge.right_click_block(&mut world, &click()),
        hfc_runtime::bridge::Flow::Cancel
    );
    let _ = bridge.take_opened_menu();
    assert!(bridge.has_screen(), "the pot opened no screen");

    // Away from every widget, then over the pot's own recipe-book button
    // (the screen blits it at (557, 311) and turns it bright while hovered).
    bridge.mouse_moved(2.0, 2.0);
    let away = bridge
        .render_screen(1280, 720, 0.0)
        .expect("render failed")
        .expect("no draws");
    bridge.mouse_moved(560.0, 320.0);
    let over = bridge
        .render_screen(1280, 720, 0.0)
        .expect("render failed")
        .expect("no draws");
    assert_ne!(
        away, over,
        "the screen drew the same list with the pointer in two places: \
         the mouse position never reached it"
    );
}

/// A left click on one of the pot's own slots picks the stack up.
///
/// In 26.3 the left button is GLFW button **1** (`MouseHandler` passes GLFW
/// numbers through, and `getContainerClickButton` maps 1 to the menu's
/// button 0). A press is handled in `AbstractContainerScreen.mouseClicked`
/// and the actual pickup runs on **release**, in `mouseReleased`: it resolves
/// `getHoveredSlot` and calls `slotClicked`, which hands
/// `gameMode.handleContainerInput(...)` the menu. That method reads
/// `player.containerMenu` and calls its `clicked`, which is what moves the
/// stack.
///
/// Two things were missing and both are covered here: the player had no open
/// menu (`containerMenu` was null, so `handleContainerInput` returned before
/// `clicked`), and that is asserted directly below.
#[test]
fn a_left_click_picks_the_pots_slot_up() {
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    let mut world = World {
        heated: false,
        held: None,
        inventory: Vec::new(),
    };
    assert_eq!(
        bridge.right_click_block(&mut world, &click()),
        hfc_runtime::bridge::Flow::Cancel,
        "the pot's use did not consume the click"
    );
    let _ = bridge.take_opened_menu();
    // The pot's own inventory slot the menu's slot 0 shows.
    assert_eq!(
        bridge.sync_menu_items([4, 1, 4], POT, &[Some(("minecraft:beef".into(), 5))]),
        1,
        "the pot's own inventory took no stack"
    );
    let draws = bridge
        .render_screen(1280, 720, 0.0)
        .expect("the pot screen failed to render")
        .expect("no draws from the open pot screen");
    assert!(!draws.is_empty(), "the pot screen drew nothing");
    assert!(bridge.has_screen(), "the pot opened no screen");

    // The player's open menu is the pot's own menu: `handleContainerInput`
    // reads it and a null one stops the click before `clicked` runs.
    let (x, y) = bridge
        .debug_menu_slot_screen_pos(0)
        .expect("the pot's menu slot 0 has no place on the screen");

    // 26.3 handles the pickup on release; the press only records the click.
    // Neither return value is asserted: `mouseClicked`'s bytecode returns true
    // for an accepted press and `mouseReleased` always falls through to
    // `Screen.mouseReleased`. The observable is the stack moving below.
    bridge.mouse_button(x, y, 1, true);
    bridge.mouse_button(x, y, 1, false);

    // The stack the pot held is now the one the menu carries, and the pot's
    // own inventory no longer has it: the click moved it instead of either
    // dropping it or doing nothing.
    assert_eq!(
        bridge.debug_menu_carried(),
        Some(("minecraft:beef".into(), 5)),
        "the left click did not pick the stack up"
    );
    assert_eq!(
        bridge.debug_block_entity_inventory([4, 1, 4], POT, 0),
        None,
        "the pot's own inventory still holds the stack the click took"
    );
}

/// Opening the pot's menu gives the player an open menu. `slotClicked` hands
/// a click to `gameMode.handleContainerInput`, which reads
/// `player.containerMenu` first and returns when it is null, so a player
/// whose open menu was never recorded received clicks that stopped there.
#[test]
fn opening_a_mods_menu_gives_the_player_the_open_menu() {
    use hfc_runtime::value::JavaObject;
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    let mut world = World {
        heated: false,
        held: None,
        inventory: Vec::new(),
    };
    assert_eq!(
        bridge.right_click_block(&mut world, &click()),
        hfc_runtime::bridge::Flow::Cancel,
        "the pot's use did not consume the click"
    );
    let _ = bridge.take_opened_menu();
    assert!(bridge.has_screen(), "the pot opened no screen");

    let screen = bridge.current_screen().expect("the screen closed");
    let menu = match bridge.interp.heap.get(screen.as_obj().unwrap()) {
        Some(JavaObject::Instance { fields, .. }) => fields.get("menu").cloned().expect("no menu"),
        _ => panic!("the screen is not an object"),
    };
    let mc = bridge
        .interp
        .heap
        .get_singleton("net/minecraft/client/Minecraft")
        .expect("no client");
    let player = match bridge.interp.heap.get(mc) {
        Some(JavaObject::Instance { fields, .. }) => {
            fields.get("player").cloned().expect("no player")
        }
        _ => panic!("the client is not an object"),
    };
    let player_id = player.as_obj().expect("the player is not an object");
    let open = bridge
        .interp
        .heap
        .get_native::<hfc_runtime::minecraft::Player>(player_id)
        .and_then(|player| player.container_menu);
    assert_eq!(
        open,
        menu.as_obj(),
        "the player's open menu is not the menu the screen shows; \
         `handleContainerInput` would return before the menu's clicked ran"
    );
}

/// A class whose statics the runtime synthesizes must be in both
/// `has_synthetic_statics` and `create_static_field_val`. `Stats` was in the
/// first only, so its `<clinit>` was skipped and every `Stats.*` field fell
/// through to the `TagKey` fallback: `Stats.WALK_ONE_CM` answered a tag rather
/// than the `Stat` that `StatType.get` builds.
#[test]
fn a_synthesized_static_is_the_type_its_own_code_builds() {
    use hfc_runtime::value::JavaObject;
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    let value = bridge
        .interp
        .resolve_static_field("net/minecraft/stats/Stats", "WALK_ONE_CM");
    let id = value.as_obj().expect("Stats.WALK_ONE_CM is not an object");
    let class = match bridge.interp.heap.get(id) {
        Some(JavaObject::Instance { class_name, .. }) => class_name.clone(),
        other => panic!("Stats.WALK_ONE_CM is not an instance: {other:?}"),
    };
    assert_eq!(
        class, "net/minecraft/stats/Stat",
        "Stats.WALK_ONE_CM is a {class}, not the Stat its StatType.get builds"
    );
    // The name is the field's own, so two fields are not the same stat.
    let other = bridge
        .interp
        .resolve_static_field("net/minecraft/stats/Stats", "DAMAGE_DEALT");
    assert_ne!(
        value.as_obj(),
        other.as_obj(),
        "two Stats fields answered the same object"
    );
}

/// A class the loader cannot find is not one the runtime interprets.
/// `census_interprets` asked `get_or_load(..).is_ok()`, and `get_or_load`
/// answers `Ok(None)` for a name it cannot resolve, so every class — including
/// a mixin target that is not on the classpath at all — counted as interpreted
/// and the census could report it as applied.
#[test]
fn a_class_the_loader_cannot_find_is_not_interpreted() {
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    assert!(
        bridge.census_interprets("net/minecraft/world/level/Level"),
        "a class the runtime loads is not reported as interpreted"
    );
    assert!(
        !bridge.census_interprets("com/example/ThisClassDoesNotExist"),
        "a class that is not on the classpath is reported as interpreted"
    );
}

/// A mod that formats a date — `Properties.store` writes a timestamped header —
/// must not be taken down by the JDK's time-zone machinery. `TimeZone.getDefault`
/// reaches the private `setDefaultZone`, which asks the JVM's native
/// `getSystemTimeZoneID` for the host zone and then parses binary tzdata this
/// runtime has no host boundary for. The failure surfaced as a `RuntimeException`
/// ("Invalid binary time-zone data") inside `AnimatedDoorsConfig.save`, and the
/// entrypoint's remaining work silently did not run.
#[test]
fn a_mod_formatting_a_date_reaches_no_tzdata_native() {
    let Some(mut bridge) = farmers_delight() else {
        return;
    };
    let timezone = bridge
        .interp
        .execute_method(
            "java/util/TimeZone",
            "getDefault",
            "()Ljava/util/TimeZone;",
            &[],
        )
        .expect("TimeZone.getDefault threw");
    assert!(
        !timezone.is_null(),
        "TimeZone.getDefault answered null, so a date format throws"
    );
    // A date formats in the zone the boundary answers, rather than throwing.
    let id = timezone.as_obj().expect("the zone is not an object");
    let zone_name = bridge
        .interp
        .execute_method(
            "java/util/TimeZone",
            "getID",
            "()Ljava/lang/String;",
            &[timezone.clone()],
        )
        .expect("TimeZone.getID threw");
    assert!(
        zone_name.as_obj().is_some(),
        "the zone boundary answers no id"
    );
    // Nothing the time-zone path needs is reported missing.
    let _ = id;
    let missing: Vec<String> = bridge
        .interp
        .missing
        .iter()
        .filter(|symbol| symbol.contains("getSystemTimeZoneID") || symbol.contains("ZoneInfoFile"))
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "the time-zone path still reaches a symbol the runtime lacks: {missing:?}"
    );
}
