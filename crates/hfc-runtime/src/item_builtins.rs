//! `Item.Properties`, `Item.<init>` and `Registry.register` for items.
//!
//! The runtime replaces `Item$Properties`: its methods record what vanilla would
//! have put in the item's data components into a native [`ItemProperties`]
//! (formulas in `bridge/items.rs`), `Item.<init>` ties that record to the item
//! object, and `Registry.register(ITEM, ...)` remembers the item under its id.
//! `FabricBridge::registered_items` hands the result to the host.

use crate::bridge::items::{ItemProperties, ToolKind, ToolMaterial};
use crate::builtins::{BuiltinResult, identifier_text, registry_kind};
use crate::heap::Heap;
use crate::interp::Interpreter;
use crate::minecraft::{Item, TagKey};
use crate::value::{JavaObject, Value};

const PROPERTIES: &str = "net/minecraft/world/item/Item$Properties";
const ITEM: &str = "net/minecraft/world/item/Item";

/// The instance field holding the native record on a `Properties` object.
const PROPERTIES_FIELD: &str = "$hfc_properties";
/// The instance field holding an item's copy of its properties.
const ITEM_FIELD: &str = "$hfc_item_properties";

/// An item a mod registered: its id and what its properties recorded.
#[derive(Debug, Clone)]
pub(crate) struct RegisteredRecord {
    pub id: String,
    pub properties: ItemProperties,
}

fn properties_object(heap: &mut Heap, this: &Value) -> Option<usize> {
    let this = this.as_obj()?;
    let existing = match heap.get(this)? {
        JavaObject::Instance { fields, .. } => fields.get(PROPERTIES_FIELD).and_then(Value::as_obj),
        _ => return None,
    };
    if existing.is_some() {
        return existing;
    }
    // A `Properties` the constructor never reached (made by reflection).
    let record = heap.alloc_native(ItemProperties::default());
    if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this) {
        fields.insert(PROPERTIES_FIELD.into(), Value::Object(Some(record)));
    }
    Some(record)
}

/// A `TagKey` as `namespace:path`.
fn tag_id(heap: &Heap, value: &Value) -> Option<String> {
    let tag = heap.get_native::<TagKey>(value.as_obj()?)?;
    Some(if tag.id.contains(':') {
        tag.id.clone()
    } else {
        format!("minecraft:{}", tag.id)
    })
}

/// A `ToolMaterial` record's fields.
fn tool_material(heap: &Heap, value: &Value) -> Option<ToolMaterial> {
    let JavaObject::Instance { fields, .. } = heap.get(value.as_obj()?)? else {
        return None;
    };
    Some(ToolMaterial {
        incorrect_blocks_for_drops: tag_id(heap, fields.get("incorrectBlocksForDrops")?)?,
        durability: fields.get("durability")?.as_int(),
        speed: fields.get("speed")?.as_float(),
        attack_damage_bonus: fields.get("attackDamageBonus")?.as_float(),
        enchantment_value: fields.get("enchantmentValue")?.as_int(),
        repair_items: tag_id(heap, fields.get("repairItems")?)?,
    })
}

/// The name of an enum constant (`Rarity.EPIC` is `EPIC`).
fn enum_name(heap: &Heap, value: &Value) -> Option<String> {
    match heap.get(value.as_obj()?)? {
        JavaObject::Instance { fields, .. } => fields
            .get("name")
            .and_then(Value::as_obj)
            .and_then(|id| heap.get_string(id))
            .map(str::to_string),
        _ => None,
    }
}

/// The methods of `Item.Properties` and `Item.<init>`.
pub(crate) fn item_method(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
    descriptor: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    match class_name {
        PROPERTIES => Some(properties_method(heap, method_name, descriptor, args)),
        ITEM if method_name == "<init>" => {
            item_constructor(heap, args);
            Some(BuiltinResult::Handled(Value::NULL))
        }
        _ => None,
    }
}

/// `Item.<init>(Properties)`: the item keeps a copy of what the properties
/// held when it was built, as vanilla copies the component map.
fn item_constructor(heap: &mut Heap, args: &[Value]) {
    let (Some(this), Some(properties)) = (args.first(), args.get(1)) else {
        return;
    };
    let Some(this) = this.as_obj() else { return };
    let Some(properties) = properties_object(heap, properties) else {
        return;
    };
    let Some(snapshot) = heap.get_native::<ItemProperties>(properties).cloned() else {
        return;
    };
    let snapshot = heap.alloc_native(snapshot);
    if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this) {
        fields.insert(ITEM_FIELD.into(), Value::Object(Some(snapshot)));
    }
}

fn properties_method(
    heap: &mut Heap,
    method_name: &str,
    descriptor: &str,
    args: &[Value],
) -> BuiltinResult {
    let this = args.first().cloned().unwrap_or(Value::NULL);
    if method_name == "<init>" {
        properties_object(heap, &this);
        return BuiltinResult::Handled(Value::NULL);
    }
    let Some(record) = properties_object(heap, &this) else {
        return BuiltinResult::Handled(Value::NULL);
    };
    let arg = |index: usize| args.get(index).cloned().unwrap_or(Value::NULL);

    // Read every argument before borrowing the record.
    let material = tool_material(heap, &arg(1));
    let tag = tag_id(heap, &arg(1));
    let tool_tag = tag_id(heap, &arg(2));
    let name = enum_name(heap, &arg(1));
    let item = arg(1)
        .as_obj()
        .and_then(|id| heap.get_native::<Item>(id))
        .map(|item| item.id.clone());

    let Some(properties) = heap.get_native_mut::<ItemProperties>(record) else {
        return BuiltinResult::Handled(this);
    };
    let (f2, f3, f4, f5) = (
        arg(2).as_float(),
        arg(3).as_float(),
        arg(4).as_float(),
        arg(5).as_float(),
    );
    let mut modelled = true;
    match method_name {
        // The id is the registry key's; `Registry.register` reads it there.
        "setId" => {}
        "stacksTo" => properties.stacks_to(arg(1).as_int()),
        "durability" => properties.durability(arg(1).as_int()),
        "rarity" => match name {
            Some(name) => properties.rarity(&name),
            None => modelled = false,
        },
        "fireResistant" => properties.fire_resistant(),
        "enchantable" => properties.enchantable(arg(1).as_int()),
        "repairable" => match (tag, item) {
            (Some(tag), _) => properties.repairable_tag(&tag),
            (None, Some(item)) => properties.repairable_item(&item),
            _ => modelled = false,
        },
        "tool" => match (material, tool_tag) {
            (Some(material), Some(mineable)) => {
                properties.kind = Some(ToolKind::Generic);
                properties.tool(&material, &mineable, f3, f4, f5);
            }
            _ => modelled = false,
        },
        "pickaxe" | "axe" | "hoe" | "shovel" => match material {
            Some(material) => {
                let (kind, mineable, disable_blocking) = match method_name {
                    "pickaxe" => (ToolKind::Pickaxe, "minecraft:mineable/pickaxe", 0.0),
                    "axe" => (ToolKind::Axe, "minecraft:mineable/axe", 5.0),
                    "hoe" => (ToolKind::Hoe, "minecraft:mineable/hoe", 0.0),
                    _ => (ToolKind::Shovel, "minecraft:mineable/shovel", 0.0),
                };
                properties.kind = Some(kind);
                properties.tool(&material, mineable, f2, f3, disable_blocking);
                match kind {
                    ToolKind::Axe => properties.block_transformer("axe"),
                    ToolKind::Hoe => properties.block_transformer("hoe"),
                    ToolKind::Shovel => properties.block_transformer("shovel"),
                    _ => {}
                }
            }
            None => modelled = false,
        },
        "sword" => match material {
            Some(material) => {
                properties.kind = Some(ToolKind::Sword);
                properties.sword(&material, f2, f3);
            }
            None => modelled = false,
        },
        _ => modelled = false,
    }
    if !modelled {
        let what = format!("Item.Properties.{method_name}{descriptor}");
        properties.note_unsupported(what.clone());
        if !heap.unsupported_item_properties.contains(&what) {
            log::warn!("{what} is not modelled; the exported item lacks what it sets");
            heap.unsupported_item_properties.push(what);
        }
    }
    if descriptor.ends_with(&format!(")L{PROPERTIES};")) {
        BuiltinResult::Handled(this)
    } else {
        BuiltinResult::Handled(Value::NULL)
    }
}

/// The id a `Registry.register` key names: a `ResourceKey`, an `Identifier` or a
/// path in the `minecraft` namespace.
fn registration_id(heap: &Heap, key: &Value) -> Option<String> {
    match heap.get(key.as_obj()?)? {
        JavaObject::Instance {
            class_name, fields, ..
        } if class_name == "net/minecraft/resources/ResourceKey" => {
            identifier_text(heap, fields.get("location")?)
        }
        JavaObject::Instance { .. } => identifier_text(heap, key),
        JavaObject::String(path) => Some(if path.contains(':') {
            path.clone()
        } else {
            format!("minecraft:{path}")
        }),
        _ => None,
    }
}

impl Interpreter {
    /// `Registry.register(registry, key, value)` for the item registry: remember
    /// the item and return the value, as Java does. `None` for any other call.
    pub(crate) fn register_item(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[Value],
    ) -> Option<Value> {
        if class_name != "net/minecraft/core/Registry" || method_name != "register" {
            return None;
        }
        let [registry, key, value] = args else {
            return None;
        };
        if registry_kind(&self.heap, registry).as_deref() != Some("ITEM") {
            return None;
        }
        let id = registration_id(&self.heap, key)?;
        let (class, snapshot) = match self.heap.get(value.as_obj()?)? {
            JavaObject::Instance {
                class_name, fields, ..
            } => (
                class_name.clone(),
                fields.get(ITEM_FIELD).and_then(Value::as_obj),
            ),
            // A vanilla item (`Items.X`) registered again is not a mod's item.
            _ => return Some(value.clone()),
        };
        if let Some(vanilla) = self.vanilla_item_class(&class) {
            log::warn!(
                "item {id} extends {vanilla}, whose behaviour the host cannot take from data; \
                 it is not exported"
            );
            return Some(value.clone());
        }
        if self.heap.registered_items.iter().any(|item| item.id == id) {
            log::warn!("item {id} is registered twice; the first registration is kept");
            return Some(value.clone());
        }
        let properties = snapshot
            .and_then(|id| self.heap.get_native::<ItemProperties>(id))
            .cloned()
            .unwrap_or_default();
        self.heap
            .registered_items
            .push(RegisteredRecord { id, properties });
        Some(value.clone())
    }

    /// The first Minecraft class other than `Item` in `class`'s superclass chain:
    /// a vanilla item behaviour (`BlockItem`, `BowItem`, ...) that has no data form.
    fn vanilla_item_class(&mut self, class: &str) -> Option<String> {
        let mut current = class.to_string();
        for _ in 0..64 {
            if current == ITEM {
                return None;
            }
            if current.starts_with("net/minecraft/") {
                return Some(current);
            }
            let _ = self.loader.get_or_load(&current);
            current = self.loader.get(&current)?.super_class.clone()?;
        }
        None
    }
}
