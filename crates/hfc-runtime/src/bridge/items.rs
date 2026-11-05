//! Items a mod registers in code, as data the host can turn into a content-mod item.
//!
//! Horizon declares a content item as a vanilla `template` plus a data component
//! patch. The runtime records what a mod's `Item.Properties` did (see
//! `item_builtins.rs`) with vanilla's own formulas, and [`export`] turns that
//! into the patch: every component the properties determine is set, and a
//! component the template carries that the mod's item would not have is removed.
//!
//! Only data is exported. A custom `Item` subclass's behaviour (`use`,
//! `useOn`, `inventoryTick`, ...) is mod bytecode the host never runs.

use crate::value::NativeObject;
use serde_json::{Map, Number, Value, json};
use std::any::Any;

/// An item a mod registered, in the shape of a Horizon
/// `data/<ns>/horizon/item/<path>.json` file.
#[derive(Debug, Clone, PartialEq)]
pub struct RegisteredItem {
    /// `namespace:path`, from the registry key the mod registered it under.
    pub id: String,
    /// The vanilla item whose behaviour class matches the mod's item.
    pub template: String,
    /// A data component patch over the template: `"minecraft:<type>"` sets one,
    /// `"!minecraft:<type>"` with `{}` removes it. `max_stack_size` and `rarity`
    /// are not in here; they have their own fields.
    pub components: Map<String, Value>,
    pub max_stack_size: Option<u32>,
    /// `common`, `uncommon`, `rare` or `epic`.
    pub rarity: Option<String>,
}

/// Which `Item.Properties` tool method a mod's properties went through. It picks
/// the template, because vanilla gives the matching item its behaviour class
/// (an axe strips logs, a hoe tills).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolKind {
    Pickaxe,
    Axe,
    Shovel,
    Hoe,
    Sword,
    /// `Properties.tool(...)`: vanilla's pickaxe is a plain `Item`, so the
    /// pickaxe is the template that carries nothing beyond the tool components.
    Generic,
}

/// A `ToolMaterial` record's fields.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolMaterial {
    /// The `incorrectBlocksForDrops` block tag, `namespace:path`.
    pub incorrect_blocks_for_drops: String,
    pub durability: i32,
    pub speed: f32,
    pub attack_damage_bonus: f32,
    pub enchantment_value: i32,
    /// The `repairItems` item tag, `namespace:path`.
    pub repair_items: String,
}

/// What a mod's `Item.Properties` accumulated: the data components vanilla's
/// methods would have set, and the tool kind.
#[derive(Debug, Clone, Default)]
pub(crate) struct ItemProperties {
    pub components: Map<String, Value>,
    pub kind: Option<ToolKind>,
    /// `Properties` methods this runtime does not model, as `name + descriptor`.
    pub unsupported: Vec<String>,
}

impl NativeObject for ItemProperties {
    fn class_name(&self) -> &str {
        "net/minecraft/world/item/Item$Properties"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

const MAX_STACK_SIZE: &str = "minecraft:max_stack_size";
const RARITY: &str = "minecraft:rarity";

/// A float field as vanilla writes it: the shortest decimal that is the same `f32`.
fn float(value: f32) -> Value {
    format!("{value:?}")
        .parse::<f64>()
        .ok()
        .and_then(Number::from_f64)
        .map_or(Value::Null, Value::Number)
}

/// A double field. Attribute amounts are `float` widened with `f2d`, so a
/// `-2.8f` speed is `-2.799999952316284`, as vanilla's own data shows.
fn double(value: f64) -> Value {
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// A tag as a `HolderSet` field spells it: `#namespace:path`.
fn tag_reference(tag: &str) -> String {
    if tag.contains(':') {
        format!("#{tag}")
    } else {
        format!("#minecraft:{tag}")
    }
}

impl ItemProperties {
    fn set(&mut self, component: &str, value: Value) {
        self.components.insert(component.to_string(), value);
    }

    /// `Properties.stacksTo`.
    pub fn stacks_to(&mut self, count: i32) {
        self.set(MAX_STACK_SIZE, json!(count));
    }

    /// `Properties.durability`: max damage, a stack of one, no damage.
    pub fn durability(&mut self, durability: i32) {
        self.set("minecraft:max_damage", json!(durability));
        self.set(MAX_STACK_SIZE, json!(1));
        self.set("minecraft:damage", json!(0));
    }

    /// `Properties.rarity` with the enum constant's name (`EPIC`).
    pub fn rarity(&mut self, name: &str) {
        self.set(RARITY, json!(name.to_lowercase()));
    }

    /// `Properties.fireResistant`: `DamageResistant(DamageTypeTags.IS_FIRE)`.
    pub fn fire_resistant(&mut self) {
        self.set(
            "minecraft:damage_resistant",
            json!({ "types": "#minecraft:is_fire" }),
        );
    }

    /// `Properties.enchantable`.
    pub fn enchantable(&mut self, value: i32) {
        self.set("minecraft:enchantable", json!({ "value": value }));
    }

    /// `Properties.repairable(TagKey)`.
    pub fn repairable_tag(&mut self, tag: &str) {
        self.set(
            "minecraft:repairable",
            json!({ "items": tag_reference(tag) }),
        );
    }

    /// `Properties.repairable(Item)`: a single item.
    pub fn repairable_item(&mut self, item: &str) {
        self.set("minecraft:repairable", json!({ "items": item }));
    }

    /// `ToolMaterial.applyCommonProperties`.
    fn apply_common(&mut self, material: &ToolMaterial) {
        self.durability(material.durability);
        self.repairable_tag(&material.repair_items);
        self.enchantable(material.enchantment_value);
    }

    /// `createToolAttributes` and `createSwordAttributes`, which are the same
    /// two main-hand `add_value` modifiers.
    fn attributes(material: &ToolMaterial, attack_damage: f32, attack_speed: f32) -> Value {
        json!([
            {
                "type": "minecraft:attack_damage",
                "id": "minecraft:base_attack_damage",
                "amount": double(f64::from(attack_damage + material.attack_damage_bonus)),
                "operation": "add_value",
                "slot": "mainhand",
            },
            {
                "type": "minecraft:attack_speed",
                "id": "minecraft:base_attack_speed",
                "amount": double(f64::from(attack_speed)),
                "operation": "add_value",
                "slot": "mainhand",
            },
        ])
    }

    /// `ToolMaterial.applyToolProperties` through `Properties.tool` and the
    /// pickaxe, axe, shovel and hoe wrappers.
    pub fn tool(
        &mut self,
        material: &ToolMaterial,
        mineable: &str,
        attack_damage: f32,
        attack_speed: f32,
        disable_blocking_seconds: f32,
    ) {
        self.apply_common(material);
        // `Tool(rules, 1.0F, 1, true)`: the default speed, damage per block and
        // creative flag are the codec's defaults, so they are left out.
        self.set(
            "minecraft:tool",
            json!({
                "rules": [
                    {
                        "blocks": tag_reference(&material.incorrect_blocks_for_drops),
                        "correct_for_drops": false,
                    },
                    {
                        "blocks": tag_reference(mineable),
                        "correct_for_drops": true,
                        "speed": float(material.speed),
                    },
                ],
            }),
        );
        self.set(
            "minecraft:attribute_modifiers",
            Self::attributes(material, attack_damage, attack_speed),
        );
        // `Weapon(2, disableBlockingSeconds)`; one damage per attack is the default.
        let mut weapon = Map::new();
        weapon.insert("item_damage_per_attack".into(), json!(2));
        if disable_blocking_seconds != 0.0 {
            weapon.insert(
                "disable_blocking_for_seconds".into(),
                float(disable_blocking_seconds),
            );
        }
        self.set("minecraft:weapon", Value::Object(weapon));
    }

    /// `ToolMaterial.applySwordProperties`.
    pub fn sword(&mut self, material: &ToolMaterial, attack_damage: f32, attack_speed: f32) {
        self.apply_common(material);
        self.set(
            "minecraft:tool",
            json!({
                "rules": [
                    {
                        "blocks": "minecraft:cobweb",
                        "correct_for_drops": true,
                        "speed": float(15.0),
                    },
                    {
                        "blocks": "#minecraft:sword_instantly_mines",
                        "speed": float(f32::MAX),
                    },
                    {
                        "blocks": "#minecraft:sword_efficient",
                        "speed": float(1.5),
                    },
                ],
                "damage_per_block": 2,
                "can_destroy_blocks_in_creative": false,
            }),
        );
        self.set(
            "minecraft:attribute_modifiers",
            Self::attributes(material, attack_damage, attack_speed),
        );
        // `Weapon(1)`: every field at its default.
        self.set("minecraft:weapon", json!({}));
    }

    /// The block transformer `Properties.axe`, `hoe` and `shovel` add.
    pub fn block_transformer(&mut self, name: &str) {
        self.set(
            "minecraft:block_transformer",
            json!(format!("minecraft:{name}")),
        );
    }

    /// The `unsupported` entry for a method the runtime does not model.
    pub fn note_unsupported(&mut self, what: String) {
        if !self.unsupported.contains(&what) {
            self.unsupported.push(what);
        }
    }
}

/// A vanilla template, with the components it carries beyond what every item has.
struct Template {
    id: &'static str,
    /// Components the template has and a plain item does not. A mod item whose
    /// properties did not set one of them gets it removed.
    specific: &'static [&'static str],
    /// The template's stack size; a mod item that never set one must say 64.
    stack_size: u32,
}

/// The components `Properties.durability` and the tool methods set, which the
/// vanilla tool templates carry.
const TOOL_COMPONENTS: &[&str] = &[
    "minecraft:attribute_modifiers",
    "minecraft:damage",
    "minecraft:enchantable",
    "minecraft:max_damage",
    "minecraft:repairable",
    "minecraft:tool",
    "minecraft:weapon",
];
const TRANSFORMING_TOOL_COMPONENTS: &[&str] = &[
    "minecraft:attribute_modifiers",
    "minecraft:block_transformer",
    "minecraft:damage",
    "minecraft:enchantable",
    "minecraft:max_damage",
    "minecraft:repairable",
    "minecraft:tool",
    "minecraft:weapon",
];

fn template(kind: Option<ToolKind>) -> Template {
    let tool = |id, specific| Template {
        id,
        specific,
        stack_size: 1,
    };
    match kind {
        Some(ToolKind::Pickaxe | ToolKind::Generic) => {
            tool("minecraft:diamond_pickaxe", TOOL_COMPONENTS)
        }
        Some(ToolKind::Sword) => tool("minecraft:diamond_sword", TOOL_COMPONENTS),
        Some(ToolKind::Axe) => tool("minecraft:diamond_axe", TRANSFORMING_TOOL_COMPONENTS),
        Some(ToolKind::Shovel) => tool("minecraft:diamond_shovel", TRANSFORMING_TOOL_COMPONENTS),
        Some(ToolKind::Hoe) => tool("minecraft:diamond_hoe", TRANSFORMING_TOOL_COMPONENTS),
        // Vanilla builds flint with `registerItem(key)`, i.e. `new Item(new
        // Properties())`: no item-specific behaviour class, and only the
        // components every item has (no fuel value, trim material or food).
        None => Template {
            id: "minecraft:flint",
            specific: &[],
            stack_size: 64,
        },
    }
}

/// The Horizon item for `id` registered with `properties`.
pub(crate) fn export(id: &str, properties: &ItemProperties) -> RegisteredItem {
    let template = template(properties.kind);
    let mut components = properties.components.clone();
    let max_stack_size = components
        .remove(MAX_STACK_SIZE)
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .or((template.stack_size != 64).then_some(64));
    let rarity = components
        .remove(RARITY)
        .and_then(|v| v.as_str().map(str::to_string));
    for component in template.specific {
        if !components.contains_key(*component) {
            components.insert(format!("!{component}"), json!({}));
        }
    }
    RegisteredItem {
        id: id.to_string(),
        template: template.id.to_string(),
        components,
        max_stack_size,
        rarity,
    }
}

impl super::FabricBridge {
    /// The items mods registered in the item registry, in registration order, as
    /// Horizon content items. Only data crosses: a custom `Item` subclass's
    /// behaviour does not, and neither does creative-tab placement.
    pub fn registered_items(&self) -> Vec<RegisteredItem> {
        self.interp
            .heap
            .registered_items
            .iter()
            .map(|record| export(&record.id, &record.properties))
            .collect()
    }

    /// `Item.Properties` methods mods called that the runtime does not model;
    /// the exported items lack whatever those would have set.
    pub fn unsupported_item_properties(&self) -> &[String] {
        &self.interp.heap.unsupported_item_properties
    }
}
