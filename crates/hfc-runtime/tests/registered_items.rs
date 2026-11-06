//! Items a mod registers in code reach the host as data.
//!
//! `Item.Properties` is replaced by the runtime: its tool methods record the
//! data components vanilla would set, `Item.<init>` ties them to the item and
//! `Registry.register` remembers the item under its id.
//! `FabricBridge::registered_items` returns each as a Horizon template plus a
//! component patch.
//!
//! * `emerald_tools_register_five_items` loads Emerald Tools and checks its
//!   five items against numbers read from the mod's bytecode.
//! * `properties_match_vanilla_items` runs vanilla's own arguments for six
//!   tools through the same path and compares the components with the
//!   `items.json` export of the real game.
//!
//! Both need a Minecraft install; the first needs the jars in `test-mods/`, the
//! second the client's `items.json`. Without them they print why and skip.

mod common;

use hfc_runtime::bridge::{FabricBridge, RegisteredItem, TabPlacement};
use hfc_runtime::value::{JavaObject, Value};
use serde_json::{Value as Json, json};
use std::collections::HashMap;
use std::path::PathBuf;

const PROPERTIES: &str = "net/minecraft/world/item/Item$Properties";
const ITEM: &str = "net/minecraft/world/item/Item";
const TOOL_MATERIAL: &str = "net/minecraft/world/item/ToolMaterial";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf()
}

fn emerald_tools() -> Option<FabricBridge> {
    let jars = [
        "fabric-api-0.165.0+26.3.jar",
        "emerald-tools-justfatlard-2.2.1.jar",
    ]
    .map(|name| root().join("test-mods").join(name));
    if jars.iter().any(|jar| !jar.exists()) {
        eprintln!("skipped: test-mods jars are not present");
        return None;
    }
    let mut bridge = common::bridge_with_vanilla()?;
    for jar in &jars {
        bridge.load_mod_jar(jar).unwrap();
    }
    bridge.init().unwrap();
    Some(bridge)
}

/// The attribute modifiers vanilla's `createToolAttributes` makes.
fn attributes(damage: f64, speed: f64) -> Json {
    json!([
        {
            "type": "minecraft:attack_damage",
            "id": "minecraft:base_attack_damage",
            "amount": damage,
            "operation": "add_value",
            "slot": "mainhand",
        },
        {
            "type": "minecraft:attack_speed",
            "id": "minecraft:base_attack_speed",
            "amount": speed,
            "operation": "add_value",
            "slot": "mainhand",
        },
    ])
}

fn tool_rules(mineable: &str) -> Json {
    json!({ "rules": [
        { "blocks": "#minecraft:incorrect_for_diamond_tool", "correct_for_drops": false },
        { "blocks": format!("#minecraft:mineable/{mineable}"), "correct_for_drops": true, "speed": 8.0 },
    ] })
}

/// `CreativeModeTabEvents.modifyOutputEvent` listeners (Fabric API's own
/// bytecode makes the per-tab events) record where Emerald Tools puts its items.
#[test]
fn emerald_tools_creative_tab_placement() {
    let Some(bridge) = emerald_tools() else {
        return;
    };
    let additions = bridge.creative_tab_additions();
    let in_tab = |tab: &str| -> Vec<(String, TabPlacement)> {
        additions
            .iter()
            .filter(|a| a.tab == tab)
            .map(|a| (a.item.clone(), a.placement.clone()))
            .collect()
    };
    let end = |name: &str| {
        (
            format!("emerald-tools-justfatlard:{name}"),
            TabPlacement::End,
        )
    };
    assert_eq!(
        in_tab("minecraft:tools_and_utilities"),
        [
            end("emerald_pickaxe"),
            end("emerald_axe"),
            end("emerald_shovel"),
            end("emerald_hoe"),
        ]
    );
    assert_eq!(in_tab("minecraft:combat"), [end("emerald_sword")]);
    assert_eq!(additions.len(), 5, "no other tab: {additions:?}");
    assert!(
        bridge.unsupported_creative_tab_calls().is_empty(),
        "unmodelled calls: {:?}",
        bridge.unsupported_creative_tab_calls()
    );
}

#[test]
fn emerald_tools_register_five_items() {
    let Some(bridge) = emerald_tools() else {
        return;
    };
    let items = bridge.registered_items();
    let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "emerald-tools-justfatlard:emerald_pickaxe",
            "emerald-tools-justfatlard:emerald_axe",
            "emerald-tools-justfatlard:emerald_shovel",
            "emerald-tools-justfatlard:emerald_hoe",
            "emerald-tools-justfatlard:emerald_sword",
        ],
        "registration order"
    );
    let templates: Vec<&str> = items.iter().map(|item| item.template.as_str()).collect();
    assert_eq!(
        templates,
        [
            "minecraft:diamond_pickaxe",
            "minecraft:diamond_axe",
            "minecraft:diamond_shovel",
            "minecraft:diamond_hoe",
            "minecraft:diamond_sword",
        ]
    );
    assert!(
        bridge.unsupported_item_properties().is_empty(),
        "unmodelled Properties methods: {:?}",
        bridge.unsupported_item_properties()
    );

    // EmeraldTools.<clinit>: ToolMaterial(INCORRECT_FOR_DIAMOND_TOOL, 2031,
    // 8.0f, 3.0f, 20, #emerald-tools-justfatlard:emerald_tool_repair_items).
    // attackDamageFor: axe 5.0, shovel 1.5, sword 3.0, hoe -3.0, otherwise 1.0;
    // attackSpeedFor: sword -2.4, pickaxe -2.8, hoe 0.0, otherwise -3.0.
    // The damage modifier is damage + 3.0 (the material's bonus). Speeds are
    // floats widened to double, so -2.8f is -2.799999952316284.
    let repairable = json!({ "items": "#emerald-tools-justfatlard:emerald_tool_repair_items" });
    let common = |item: &RegisteredItem, damage: f64, speed: f64, mineable: &str| {
        assert_eq!(item.max_stack_size, Some(1), "{}", item.id);
        assert_eq!(item.rarity, None, "{}", item.id);
        assert_eq!(item.components["minecraft:max_damage"], json!(2031));
        assert_eq!(item.components["minecraft:damage"], json!(0));
        assert_eq!(
            item.components["minecraft:enchantable"],
            json!({ "value": 20 })
        );
        assert_eq!(item.components["minecraft:repairable"], repairable);
        assert_eq!(item.components["minecraft:tool"], tool_rules(mineable));
        assert_eq!(
            item.components["minecraft:attribute_modifiers"],
            attributes(damage, speed)
        );
    };

    let pickaxe = &items[0];
    common(pickaxe, 4.0, -2.799999952316284, "pickaxe");
    assert_eq!(
        pickaxe.components["minecraft:weapon"],
        json!({ "item_damage_per_attack": 2 })
    );
    // Everything the properties determine is set, so nothing is removed and
    // nothing else (item_model, item_name) is set: Horizon renames those.
    let keys: Vec<&str> = pickaxe.components.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "minecraft:attribute_modifiers",
            "minecraft:damage",
            "minecraft:enchantable",
            "minecraft:max_damage",
            "minecraft:repairable",
            "minecraft:tool",
            "minecraft:weapon",
        ]
    );

    let axe = &items[1];
    common(axe, 8.0, -3.0, "axe");
    assert_eq!(
        axe.components["minecraft:weapon"],
        json!({ "item_damage_per_attack": 2, "disable_blocking_for_seconds": 5.0 })
    );
    assert_eq!(
        axe.components["minecraft:block_transformer"],
        json!("minecraft:axe")
    );

    let shovel = &items[2];
    common(shovel, 4.5, -3.0, "shovel");
    assert_eq!(
        shovel.components["minecraft:block_transformer"],
        json!("minecraft:shovel")
    );

    let hoe = &items[3];
    common(hoe, 0.0, 0.0, "hoe");
    assert_eq!(
        hoe.components["minecraft:block_transformer"],
        json!("minecraft:hoe")
    );

    // A sword mines cobweb and the sword tags instead of its material's tag.
    let sword = &items[4];
    assert_eq!(sword.max_stack_size, Some(1));
    assert_eq!(sword.components["minecraft:max_damage"], json!(2031));
    assert_eq!(
        sword.components["minecraft:attribute_modifiers"],
        attributes(6.0, -2.4000000953674316)
    );
    assert_eq!(sword.components["minecraft:weapon"], json!({}));
    assert_eq!(
        sword.components["minecraft:tool"],
        json!({
            "rules": [
                { "blocks": "minecraft:cobweb", "correct_for_drops": true, "speed": 15.0 },
                { "blocks": "#minecraft:sword_instantly_mines", "speed": 3.4028235e38 },
                { "blocks": "#minecraft:sword_efficient", "speed": 1.5 },
            ],
            "damage_per_block": 2,
            "can_destroy_blocks_in_creative": false,
        })
    );
}

/// `items.json` of the installed client: every vanilla item with all its components.
fn vanilla_items() -> Option<HashMap<String, Json>> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let versions = home.join(".local/share/horizon-launcher/versions");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(versions)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join(".local/horizon-client/26.3/items.json"))
        .filter(|path| path.is_file())
        .collect();
    dirs.sort();
    let text = std::fs::read_to_string(dirs.pop()?).ok()?;
    serde_json::from_str(&text).ok()
}

/// JSON equality with a tolerance on numbers (vanilla's export prints floats
/// and doubles in different widths).
fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            (x - y).abs() <= 1e-6 * x.abs().max(y.abs()).max(1.0)
        }
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y))
        }
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

/// Builds `Items.<ITEM> = registerItem(key, new Item.Properties().<method>(
/// ToolMaterial.<MATERIAL>, ...))` through the interpreter, as vanilla's
/// `Items.<clinit>` does, and registers it under `test:<name>`.
fn register_vanilla_tool(
    bridge: &mut FabricBridge,
    name: &str,
    material: &str,
    method: &str,
    damage: f32,
    speed: f32,
    fire_resistant: bool,
) {
    let interp = &mut bridge.interp;
    interp.ensure_initialized(TOOL_MATERIAL).unwrap();
    let material = interp.resolve_static_field(TOOL_MATERIAL, material);
    let new = |interp: &mut hfc_runtime::interp::Interpreter, class: &str| {
        Value::Object(Some(interp.heap.alloc(JavaObject::Instance {
            class_name: class.into(),
            fields: HashMap::new(),
        })))
    };
    let properties = new(interp, PROPERTIES);
    interp
        .execute_special_method(
            PROPERTIES,
            "<init>",
            "()V",
            std::slice::from_ref(&properties),
        )
        .unwrap();
    let returns_properties = format!("(L{TOOL_MATERIAL};FF)L{PROPERTIES};");
    interp
        .execute_method(
            PROPERTIES,
            method,
            &returns_properties,
            &[
                properties.clone(),
                material,
                Value::Float(damage),
                Value::Float(speed),
            ],
        )
        .unwrap();
    if fire_resistant {
        interp
            .execute_method(
                PROPERTIES,
                "fireResistant",
                &format!("()L{PROPERTIES};"),
                std::slice::from_ref(&properties),
            )
            .unwrap();
    }
    let item = new(interp, ITEM);
    interp
        .execute_special_method(
            ITEM,
            "<init>",
            &format!("(L{PROPERTIES};)V"),
            &[item.clone(), properties],
        )
        .unwrap();

    let registries =
        interp.resolve_static_field("net/minecraft/core/registries/Registries", "ITEM");
    let identifier = Value::Object(Some(interp.heap.alloc_identifier("test", name)));
    let key = interp
        .execute_method(
            "net/minecraft/resources/ResourceKey",
            "create",
            "(Lnet/minecraft/resources/ResourceKey;Lnet/minecraft/resources/Identifier;)Lnet/minecraft/resources/ResourceKey;",
            &[registries, identifier],
        )
        .unwrap();
    let registry =
        interp.resolve_static_field("net/minecraft/core/registries/BuiltInRegistries", "ITEM");
    let returned = interp
        .execute_method(
            "net/minecraft/core/Registry",
            "register",
            "(Lnet/minecraft/core/Registry;Lnet/minecraft/resources/ResourceKey;Ljava/lang/Object;)Ljava/lang/Object;",
            &[registry, key, item.clone()],
        )
        .unwrap();
    assert_eq!(returned, item, "Registry.register returns the value");
}

#[test]
fn properties_match_vanilla_items() {
    let Some(vanilla) = vanilla_items() else {
        eprintln!("skipped: the client's items.json is not present");
        return;
    };
    let Some(mut bridge) = common::bridge_with_vanilla() else {
        return;
    };
    // The arguments are those of vanilla's `Items.<clinit>` (read from its
    // bytecode): `new Item.Properties().<method>(ToolMaterial.<M>, damage, speed)`,
    // and `.fireResistant()` on the netherite sword.
    let cases = [
        ("diamond_pickaxe", "DIAMOND", "pickaxe", 1.0, -2.8, false),
        ("diamond_axe", "DIAMOND", "axe", 5.0, -3.0, false),
        ("diamond_shovel", "DIAMOND", "shovel", 1.5, -3.0, false),
        ("diamond_hoe", "DIAMOND", "hoe", -3.0, 0.0, false),
        ("diamond_sword", "DIAMOND", "sword", 3.0, -2.4, false),
        ("netherite_sword", "NETHERITE", "sword", 3.0, -2.4, true),
    ];
    for (name, material, method, damage, speed, fire) in cases {
        register_vanilla_tool(&mut bridge, name, material, method, damage, speed, fire);
    }
    assert!(bridge.unsupported_item_properties().is_empty());

    let items = bridge.registered_items();
    assert_eq!(items.len(), cases.len());
    for (item, (name, ..)) in items.iter().zip(cases) {
        let oracle = &vanilla[&format!("minecraft:{name}")];
        // The template is the vanilla tool of the same kind (the netherite
        // sword's is the diamond sword), so what it adds comes from the patch.
        let kind = name.split('_').next_back().unwrap();
        assert_eq!(item.template, format!("minecraft:diamond_{kind}"), "{name}");
        assert_eq!(item.id, format!("test:{name}"));
        assert_eq!(item.max_stack_size, Some(1), "{name}");
        assert_eq!(item.rarity, None, "{name}");
        // Everything the tool's properties set. `item_model`, `item_name` and
        // the other components of every item are the template's.
        let determined = [
            "minecraft:tool",
            "minecraft:attribute_modifiers",
            "minecraft:max_damage",
            "minecraft:damage",
            "minecraft:enchantable",
            "minecraft:repairable",
            "minecraft:weapon",
            "minecraft:block_transformer",
            "minecraft:damage_resistant",
        ];
        for component in determined {
            match (oracle.get(component), item.components.get(component)) {
                (Some(want), Some(got)) => {
                    assert!(
                        same(want, got),
                        "{name} {component}: want {want}, got {got}"
                    )
                }
                (None, None) => {}
                (want, got) => panic!("{name} {component}: want {want:?}, got {got:?}"),
            }
        }
        assert_eq!(oracle["minecraft:max_stack_size"], json!(1));
        assert!(
            item.components
                .keys()
                .all(|key| determined.contains(&key.as_str())),
            "{name}: {:?}",
            item.components.keys().collect::<Vec<_>>()
        );
    }
}

/// Emerald Armor registers four pieces through `Properties.humanoidArmor`.
///
/// Numbers from `Main.<clinit>` (javap): `ArmorMaterial(37, {HELMET 3,
/// CHESTPLATE 8, LEGGINGS 6, BOOTS 3}, 20, ARMOR_EQUIP_DIAMOND, 2.0f, 0.0f,
/// #emerald-armor-justfatlard:emerald_armor_repair_items,
/// minecraft:emerald)`. Vanilla's `ArmorType` unit durabilities are HELMET 11,
/// CHESTPLATE 16, LEGGINGS 15, BOOTS 13, and `humanoidArmor` sets
/// `durability(unit * material.durability)`.
#[test]
fn emerald_armor_is_wearable_and_not_stackable() {
    let jars = [
        "fabric-api-0.165.0+26.3.jar",
        "emerald-armor-justfatlard-2.1.1.jar",
    ]
    .map(|name| root().join("test-mods").join(name));
    if jars.iter().any(|jar| !jar.exists()) {
        eprintln!("skipped: test-mods jars are not present");
        return;
    }
    let Some(mut bridge) = common::bridge_with_vanilla() else {
        return;
    };
    for jar in &jars {
        bridge.load_mod_jar(jar).unwrap();
    }
    bridge.init().unwrap();
    assert!(
        bridge.unsupported_item_properties().is_empty(),
        "unmodelled Properties methods: {:?}",
        bridge.unsupported_item_properties()
    );
    let items = bridge.registered_items();
    assert_eq!(items.len(), 4, "{items:?}");

    // (piece, vanilla unit durability, defense, slot)
    let pieces = [
        ("helmet", 11, 3.0, "head"),
        ("chestplate", 16, 8.0, "chest"),
        ("leggings", 15, 6.0, "legs"),
        ("boots", 13, 3.0, "feet"),
    ];
    for (item, (piece, unit, defense, slot)) in items.iter().zip(pieces) {
        let id = &item.id;
        assert_eq!(id, &format!("emerald-armor-justfatlard:emerald_{piece}"));
        assert_eq!(item.template, format!("minecraft:diamond_{piece}"), "{id}");
        assert_eq!(item.max_stack_size, Some(1), "{id}");
        assert_eq!(
            item.components["minecraft:max_damage"],
            json!(37 * unit),
            "{id}"
        );
        assert_eq!(item.components["minecraft:damage"], json!(0), "{id}");
        assert_eq!(
            item.components["minecraft:enchantable"],
            json!({ "value": 20 }),
            "{id}"
        );
        assert_eq!(
            item.components["minecraft:repairable"],
            json!({ "items": "#emerald-armor-justfatlard:emerald_armor_repair_items" }),
            "{id}"
        );
        assert_eq!(
            item.components["minecraft:equippable"],
            json!({
                "slot": slot,
                "asset_id": "minecraft:emerald",
                "equip_sound": "minecraft:item.armor.equip_diamond",
            }),
            "{id}"
        );
        // Toughness 2.0; knockback resistance 0.0 adds no modifier.
        let modifier = |attribute: &str, amount: f64| {
            json!({
                "type": attribute,
                "id": format!("minecraft:armor.{piece}"),
                "amount": amount,
                "operation": "add_value",
                "slot": slot,
            })
        };
        assert_eq!(
            item.components["minecraft:attribute_modifiers"],
            json!([
                modifier("minecraft:armor", defense),
                modifier("minecraft:armor_toughness", 2.0),
            ]),
            "{id}"
        );
        assert!(
            !item.components.keys().any(|key| key.starts_with('!')),
            "nothing the template carries is removed: {:?}",
            item.components.keys().collect::<Vec<_>>()
        );
    }
}

/// The world `right_click_block` sees: one dirt block, nothing changes.
struct DirtHost;

impl hfc_runtime::bridge::HostBridge for DirtHost {
    fn block(&self, _: [i32; 3]) -> String {
        "minecraft:dirt".into()
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

/// Emerald Tools registers a `UseBlockCallback` that returns vanilla's
/// `InteractionResult.PASS` (the real `InteractionResult$Pass` record) for a
/// click it does not handle. That must let vanilla use the block, so placing
/// a block against dirt with an empty hand goes on.
#[test]
fn a_passing_use_block_listener_lets_vanilla_handle_the_click() {
    let Some(mut bridge) = emerald_tools() else {
        return;
    };
    let event = hfc_runtime::bridge::RightClickBlockEvent {
        pos: [0, 64, 0],
        face: [0, 1, 0],
        hit: [0.5, 65.0, 0.5],
        sneaking: false,
        off_hand: false,
    };
    assert_eq!(
        bridge.right_click_block(&mut DirtHost, &event),
        hfc_runtime::bridge::Flow::Continue
    );
}

/// Farmer's Delight is the broadest content mod used for development: its food
/// comes from `Item.Properties.food`, its tab from a `CreativeModeTab` it
/// builds in code, and most of its blocks carry a map colour the facts do not
/// need. The test needs the jar under `test-mods/corpus/`; it skips without it.
#[test]
fn farmers_delight_food_its_tab_and_blocks_are_exported() {
    let jar = root()
        .join("test-mods/corpus")
        .join("FarmersDelight-26.3-3.6.30+refabricated.jar");
    let api = root().join("test-mods").join("fabric-api-0.165.0+26.3.jar");
    if !jar.exists() || !api.exists() {
        eprintln!("skipped: Farmer's Delight or Fabric API is not present");
        return;
    }
    let Some(mut bridge) = common::bridge_with_vanilla() else {
        return;
    };
    bridge.load_mod_jar(api).unwrap();
    bridge.load_mod_jar(jar).unwrap();
    bridge.init().unwrap();

    let items = bridge.registered_items();
    let bacon = items
        .iter()
        .find(|item| item.id == "farmersdelight:bacon")
        .expect("Farmer's Delight registers bacon");
    assert!(
        bacon.components.contains_key("minecraft:food"),
        "food is modelled: {:?}",
        bacon.components.keys().collect::<Vec<_>>()
    );
    assert!(bacon.components.contains_key("minecraft:consumable"));

    let tabs = bridge.registered_creative_tabs();
    let tab = tabs
        .iter()
        .find(|tab| tab.id.starts_with("farmersdelight:"))
        .expect("Farmer's Delight registers a creative tab");
    assert!(!tab.title.is_empty(), "{tab:?}");
    assert!(
        tab.items
            .iter()
            .any(|item| item.starts_with("farmersdelight:")),
        "{tab:?}"
    );

    let blocks = bridge.registered_blocks();
    let stove = blocks
        .iter()
        .find(|block| block.id == "farmersdelight:stove")
        .expect("the stove is a block");
    assert!(!stove.states.is_empty(), "{stove:?}");
    assert!(stove.unreadable.is_empty(), "{:?}", stove.unreadable);
    // Signs name their properties through a native IntegerProperty, which the
    // facts used to read as null; they are declared now too.
    let sign = blocks
        .iter()
        .find(|block| block.id == "farmersdelight:canvas_sign")
        .expect("the canvas sign is a block");
    assert!(!sign.states.is_empty(), "{sign:?}");
    assert!(sign.unreadable.is_empty(), "{:?}", sign.unreadable);
    // `BlockStateProperties.HALF` is an enum (lower/upper), not a boolean.
    let rice = blocks
        .iter()
        .find(|block| block.id == "farmersdelight:wild_rice")
        .expect("wild rice is a block");
    assert_eq!(
        rice.properties.get("half"),
        Some(&vec!["lower".to_string(), "upper".to_string()]),
        "{rice:?}"
    );

    // Shapes. A block whose shape comes from a map keyed by a `Direction`
    // (this sign is vanilla `WallSignBlock`'s, whose `SHAPES` is an
    // `EnumMap<Direction, VoxelShape>` built by `Shapes.rotateHorizontal`)
    // used to answer null for every state: the map was built over no enum
    // constants. javap of `WallSignBlock`: `Block.boxZ(16.0, 4.5, 12.5,
    // 14.0, 16.0)` against each wall in turn, 0.125 thick.
    bridge.read_block_shapes(&[]);
    let blocks = bridge.registered_blocks();
    let wall_sign = blocks
        .iter()
        .find(|block| block.id == "farmersdelight:canvas_wall_sign")
        .expect("the canvas wall sign is a block");
    assert!(
        wall_sign.unreadable.is_empty(),
        "{:?}",
        wall_sign.unreadable
    );
    for (facing, expect) in [
        ("north", [0.0, 0.28125, 0.875, 1.0, 0.78125, 1.0]),
        ("east", [0.0, 0.28125, 0.0, 0.125, 0.78125, 1.0]),
        ("south", [0.0, 0.28125, 0.0, 1.0, 0.78125, 0.125]),
        ("west", [0.875, 0.28125, 0.0, 1.0, 0.78125, 1.0]),
    ] {
        let key = format!("facing={facing},waterlogged=false");
        let state = wall_sign
            .states
            .get(&key)
            .unwrap_or_else(|| panic!("{key} is a state of the canvas wall sign"));
        assert_eq!(state.outline.as_deref(), Some(&[expect][..]), "{key}");
        // A wall sign is decorative: vanilla gives it no collision shape, and
        // an empty shape is a real answer (not one that failed to read).
        assert_eq!(state.collision.as_deref(), Some(&[][..]), "{key}");
    }
    // Every state of every block has a shape: the null-shape classes (signs,
    // pies, feasts, baskets, beds) were the bulk of this mod's blocks.
    let missing: Vec<String> = blocks
        .iter()
        .flat_map(|block| {
            block
                .states
                .iter()
                .filter(|(_, state)| state.outline.is_none() || state.collision.is_none())
                .map(move |(key, _)| format!("{} [{key}]", block.id))
        })
        .collect();
    assert!(missing.is_empty(), "states without a shape: {missing:?}");
}
