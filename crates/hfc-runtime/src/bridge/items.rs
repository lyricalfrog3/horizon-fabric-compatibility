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

/// Where an item goes in a creative tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabPlacement {
    /// Appended after the tab's last item (`accept`).
    End,
    /// Inserted at the front of the tab (`prepend`). Several `Start` additions
    /// apply in order, each at index 0, so the last one ends up first.
    Start,
    /// Directly after this item (`insertAfter`). `insertAfter(x, a, b)` is
    /// `After(x)` for `a` and `After(a)` for `b`, so `a` stays before `b`.
    After(String),
    /// Directly before this item (`insertBefore`). `insertBefore(x, a, b)` is
    /// `Before(x)` for both, applied in order, which gives `a, b, x`.
    Before(String),
}

/// One item a mod put in a creative tab through
/// `CreativeModeTabEvents.modifyOutputEvent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreativeTabAddition {
    /// The creative tab's id, `namespace:path` (`minecraft:combat`).
    pub tab: String,
    /// The item's id, `namespace:path`.
    pub item: String,
    pub placement: TabPlacement,
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
    /// `Properties.humanoidArmor(material, ArmorType.HELMET)`.
    Helmet,
    Chestplate,
    Leggings,
    Boots,
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

/// An `ArmorMaterial` record, reduced to what `Properties.humanoidArmor` reads
/// for one `ArmorType`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ArmorMaterial {
    pub durability: i32,
    /// `defense.getOrDefault(type, 0)`.
    pub defense: i32,
    pub enchantment_value: i32,
    /// The `SoundEvents` constant of `equipSound` (`ARMOR_EQUIP_DIAMOND`).
    pub equip_sound_constant: Option<String>,
    pub toughness: f32,
    pub knockback_resistance: f32,
    /// The `repairIngredient` item tag, `namespace:path`.
    pub repair_items: String,
    /// The `assetId` key's identifier, `namespace:path`.
    pub asset_id: Option<String>,
}

/// An `ArmorType` enum constant's fields.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ArmorType {
    /// `getName()`: `helmet`, `chestplate`, `leggings`, `boots` or `body`.
    pub name: String,
    /// The slot's serialized name: `head`, `chest`, `legs`, `feet` or `body`.
    pub slot: String,
    /// `unitDurability`, the multiplier `getDurability` applies.
    pub unit_durability: i32,
}

/// What a mod's `Item.Properties` accumulated: the data components vanilla's
/// methods would have set, and the tool kind.
#[derive(Debug, Clone, Default)]
pub(crate) struct ItemProperties {
    pub components: Map<String, Value>,
    pub kind: Option<ToolKind>,
    /// The `SoundEvents` constant named by an `equippable` component's
    /// `equip_sound`. Its id lives in the class's initializer, so
    /// `Registry.register` resolves it into the component.
    pub equip_sound_constant: Option<String>,
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

    /// `Properties.humanoidArmor(material, type)`: durability scaled by the
    /// type, the material's attribute modifiers, enchantability, an
    /// `equippable` for the type's slot and the repair tag.
    pub fn humanoid_armor(&mut self, material: &ArmorMaterial, armor: &ArmorType) {
        self.durability(armor.unit_durability.wrapping_mul(material.durability));
        // `ArmorMaterial.createAttributes`: armor and toughness always, knockback
        // resistance only when positive, all under `armor.<type>`.
        let modifier = |attribute: &str, amount: f64| {
            json!({
                "type": attribute,
                "id": format!("minecraft:armor.{}", armor.name),
                "amount": double(amount),
                "operation": "add_value",
                "slot": armor.slot,
            })
        };
        let mut modifiers = vec![
            modifier("minecraft:armor", f64::from(material.defense)),
            modifier("minecraft:armor_toughness", f64::from(material.toughness)),
        ];
        if material.knockback_resistance > 0.0 {
            modifiers.push(modifier(
                "minecraft:knockback_resistance",
                f64::from(material.knockback_resistance),
            ));
        }
        self.set("minecraft:attribute_modifiers", Value::Array(modifiers));
        self.enchantable(material.enchantment_value);
        // `Equippable.builder(slot)`: swappable, dispensable and damage-on-hurt
        // default to true, which the codec's defaults already say.
        let mut equippable = Map::new();
        equippable.insert("slot".into(), json!(armor.slot));
        if let Some(asset) = &material.asset_id {
            equippable.insert("asset_id".into(), json!(asset));
        }
        self.set("minecraft:equippable", Value::Object(equippable));
        self.equip_sound_constant = material.equip_sound_constant.clone();
        self.repairable_tag(&material.repair_items);
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

/// The components `humanoidArmor` sets, which the vanilla armor templates carry.
const ARMOR_COMPONENTS: &[&str] = &[
    "minecraft:attribute_modifiers",
    "minecraft:damage",
    "minecraft:enchantable",
    "minecraft:equippable",
    "minecraft:max_damage",
    "minecraft:repairable",
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
        Some(ToolKind::Helmet) => tool("minecraft:diamond_helmet", ARMOR_COMPONENTS),
        Some(ToolKind::Chestplate) => tool("minecraft:diamond_chestplate", ARMOR_COMPONENTS),
        Some(ToolKind::Leggings) => tool("minecraft:diamond_leggings", ARMOR_COMPONENTS),
        Some(ToolKind::Boots) => tool("minecraft:diamond_boots", ARMOR_COMPONENTS),
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
    /// behaviour does not.
    pub fn registered_items(&self) -> Vec<RegisteredItem> {
        self.interp
            .heap
            .registered_items
            .iter()
            .map(|record| export(&record.id, &record.properties))
            .collect()
    }

    /// What mods' `CreativeModeTabEvents.modifyOutputEvent` listeners put in
    /// creative tabs, in the order they made it. Collected when `init` ends.
    pub fn creative_tab_additions(&self) -> Vec<CreativeTabAddition> {
        self.interp.heap.creative_tab_additions.clone()
    }

    /// Creative-tab output calls the runtime does not model; the additions lack
    /// what those would have done.
    pub fn unsupported_creative_tab_calls(&self) -> &[String] {
        &self.interp.heap.unsupported_creative_tab_calls
    }

    /// `Item.Properties` methods mods called that the runtime does not model;
    /// the exported items lack whatever those would have set.
    pub fn unsupported_item_properties(&self) -> &[String] {
        &self.interp.heap.unsupported_item_properties
    }
}
