//! Blocks mods register in code.
//!
//! `Registry.register(BuiltInRegistries.BLOCK, id, block)` records the block
//! object. The block itself is built by the mod's constructors and Minecraft's
//! real `Block`/`BlockBehaviour` bytecode: its `StateDefinition` creates the
//! states, `registerDefaultState` picks the default and `BlockStateBase` keeps
//! the numbers `BlockBehaviour.Properties` held. When the entrypoints have run,
//! `collect_block_facts` asks those objects for what the host needs
//! (`FabricBridge::registered_blocks`): their properties, states and shapes,
//! each through the class's own method, never from a table of our own.

use crate::bridge::items::ItemProperties;
use crate::bridge::{BlockSound, RegisteredBlock, RegisteredState};
use crate::builtins::{BuiltinResult, identifier_text, registry_kind};
use crate::heap::Heap;
use crate::interp::Interpreter;
use crate::item_builtins::ITEM_FIELD;
use crate::minecraft::{EnumProperty, IntegerProperty, TagKey};
use crate::value::{JavaObject, Value};
use hfc_core::{Error, Result};
use std::collections::{BTreeMap, HashMap};

const BLOCK: &str = "net/minecraft/world/level/block/Block";
/// Minecraft's own classes, in binary form.
const VANILLA: &str = "net.minecraft.";
const BLOCK_ITEM: &str = "net/minecraft/world/item/BlockItem";
const ENTITY_BLOCK: &str = "net/minecraft/world/level/block/EntityBlock";
const STATE_BASE: &str = "net/minecraft/world/level/block/state/BlockBehaviour$BlockStateBase";
const STATE_DEFINITION: &str = "net/minecraft/world/level/block/state/StateDefinition";
const PROPERTY: &str = "net/minecraft/world/level/block/state/properties/Property";
const VOXEL_SHAPE: &str = "net/minecraft/world/phys/shapes/VoxelShape";
const SHAPE_DESCRIPTOR: &str = "(Lnet/minecraft/world/level/BlockGetter;Lnet/minecraft/core/BlockPos;)Lnet/minecraft/world/phys/shapes/VoxelShape;";

/// A block a mod registered: its id and the block object.
#[derive(Debug, Clone)]
pub(crate) struct BlockRecord {
    pub id: String,
    pub object: usize,
}

fn failure(what: impl Into<String>) -> Error {
    Error::new("interpreter", what.into())
}

/// Calls on a block-state `Property`. A native [`IntegerProperty`] has no
/// Java fields, so its `getName`/`getPossibleValues`/name-of-a-value are
/// answered here instead of running the abstract Java methods.
pub(crate) fn property_method(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
    descriptor: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    if class_name != PROPERTY {
        return None;
    }
    let this = args.first()?.as_obj()?;
    let name = |heap: &Heap| -> Option<String> {
        if let Some(property) = heap.get_native::<IntegerProperty>(this) {
            return Some(property.name.clone());
        }
        if let Some(property) = heap.get_native::<EnumProperty>(this) {
            return Some(property.name.clone());
        }
        // An unset `BlockStateProperties` static is the runtime's TagKey
        // placeholder; its id is the property's field name (`waterlogged`).
        if let Some(tag) = heap.get_native::<TagKey>(this) {
            return Some(tag.id.clone());
        }
        match heap.get(this)? {
            JavaObject::Instance { fields, .. } => heap
                .get_string(fields.get("name")?.as_obj()?)
                .map(str::to_string),
            _ => None,
        }
    };
    let string = |heap: &mut Heap, text: String| {
        let id = heap.alloc_string(text);
        Value::Object(Some(id))
    };
    match (method_name, descriptor) {
        ("getName", "()Ljava/lang/String;") => {
            Some(BuiltinResult::Handled(string(heap, name(heap)?)))
        }
        ("getName", "(Ljava/lang/Comparable;)Ljava/lang/String;") => {
            if let Some(property) = heap.get_native::<IntegerProperty>(this) {
                let value = args.get(1).map(Value::as_int).unwrap_or(property.min);
                return Some(BuiltinResult::Handled(string(heap, value.to_string())));
            }
            if heap
                .get_native::<TagKey>(this)
                .is_some_and(|tag| tag.id == "half")
            {
                let text = args
                    .get(1)
                    .and_then(|value| match value.as_obj().and_then(|id| heap.get(id)) {
                        Some(JavaObject::String(text)) => Some(text.clone()),
                        Some(JavaObject::Instance { fields, .. }) => fields
                            .get("name")
                            .and_then(Value::as_obj)
                            .and_then(|id| heap.get_string(id))
                            .map(str::to_string),
                        _ => None,
                    })
                    .unwrap_or_else(|| "lower".into())
                    .to_ascii_lowercase();
                return Some(BuiltinResult::Handled(string(heap, text)));
            }
            if let Some(property) = heap.get_native::<EnumProperty>(this) {
                let wanted = args.get(1).cloned().unwrap_or(Value::NULL);
                let text = wanted
                    .as_obj()
                    .and_then(|id| heap.get_string(id))
                    .map(str::to_string)
                    .or_else(|| {
                        let JavaObject::Instance { fields, .. } = heap.get(wanted.as_obj()?)?
                        else {
                            return None;
                        };
                        fields
                            .get("name")
                            .and_then(Value::as_obj)
                            .and_then(|id| heap.get_string(id))
                            .map(str::to_string)
                    })
                    .or_else(|| property.values.first().cloned())?;
                let text = text.to_ascii_lowercase();
                return Some(BuiltinResult::Handled(string(heap, text)));
            }
            if heap.get_native::<TagKey>(this).is_some() {
                let text = args
                    .get(1)
                    .and_then(|value| {
                        value
                            .as_obj()
                            .and_then(|id| heap.get_string(id))
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| {
                        if args.get(1).map(Value::as_int).unwrap_or(0) != 0 {
                            "true".into()
                        } else {
                            "false".into()
                        }
                    });
                return Some(BuiltinResult::Handled(string(heap, text)));
            }
            None
        }
        ("getPossibleValues", "()Ljava/util/List;") => {
            let values: Vec<Value> =
                if let Some(property) = heap.get_native::<IntegerProperty>(this) {
                    (property.min..=property.max).map(Value::Int).collect()
                } else if heap
                    .get_native::<TagKey>(this)
                    .is_some_and(|tag| tag.id == "half")
                {
                    vec![
                        Value::Object(Some(heap.alloc_string("lower".to_string()))),
                        Value::Object(Some(heap.alloc_string("upper".to_string()))),
                    ]
                } else if heap.get_native::<TagKey>(this).is_some() {
                    vec![
                        Value::Object(Some(heap.alloc_string("true".to_string()))),
                        Value::Object(Some(heap.alloc_string("false".to_string()))),
                    ]
                } else {
                    return None;
                };
            let list = heap.alloc_list(values);
            Some(BuiltinResult::Handled(Value::Object(Some(list))))
        }
        _ => None,
    }
}

impl Interpreter {
    /// `Registry.register(registry, key, value)` for the block registry:
    /// remember the block and return the value, as Java does. `None` for any
    /// other call.
    pub(crate) fn register_block(
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
        if registry_kind(&self.heap, registry).as_deref() != Some("BLOCK") {
            return None;
        }
        let id = crate::item_builtins::registration_id(&self.heap, key)?;
        let object = value.as_obj()?;
        if !matches!(self.heap.get(object), Some(JavaObject::Instance { .. })) {
            // A vanilla block (`Blocks.X`) registered again is not a mod's block.
            return Some(value.clone());
        }
        if self.heap.registered_blocks.iter().any(|b| b.id == id) {
            log::warn!("block {id} is registered twice; the first registration is kept");
            return Some(value.clone());
        }
        self.heap.registered_blocks.push(BlockRecord { id, object });
        Some(value.clone())
    }

    /// `new BlockItem(block, ...)` registered in the item registry: remember
    /// which block the item places. `item` is the registered item object.
    pub(crate) fn note_block_item(&mut self, id: &str, item: usize) {
        let Some(JavaObject::Instance { class_name, fields }) = self.heap.get(item) else {
            return;
        };
        let class = class_name.clone();
        let block = fields.get("block").and_then(Value::as_obj);
        if !self.class_is_assignable_to(&class, BLOCK_ITEM) {
            return;
        }
        if let Some(block) = block {
            self.heap
                .registered_block_items
                .push((id.to_string(), block, item));
        }
    }

    /// The elements of a collection, in iteration order: a heap list or set
    /// directly, anything else (a Guava `ImmutableList`) through `Iterator`.
    pub fn collection_elements(&mut self, collection: &Value) -> Result<Vec<Value>> {
        let Some(id) = collection.as_obj() else {
            return Err(failure("a collection was null"));
        };
        if let Some(JavaObject::List(items) | JavaObject::Set(items)) = self.heap.get(id) {
            return Ok(items.clone());
        }
        let iterator = self.execute_method(
            "java/util/Collection",
            "iterator",
            "()Ljava/util/Iterator;",
            std::slice::from_ref(collection),
        )?;
        // The call returned because the collection threw: reading it as an
        // empty collection would hide the failure.
        if let Some(exception) = self.pending_exception.take() {
            let text = self.describe_exception(&exception);
            self.pending_exception = Some(exception);
            return Err(failure(format!("iterating threw {text}")));
        }
        let mut items = Vec::new();
        while self
            .execute_method(
                "java/util/Iterator",
                "hasNext",
                "()Z",
                std::slice::from_ref(&iterator),
            )?
            .as_int()
            != 0
        {
            items.push(self.execute_method(
                "java/util/Iterator",
                "next",
                "()Ljava/lang/Object;",
                std::slice::from_ref(&iterator),
            )?);
        }
        Ok(items)
    }

    pub(crate) fn instance_field(&self, object: &Value, name: &str) -> Option<Value> {
        match self.heap.get(object.as_obj()?)? {
            JavaObject::Instance { fields, .. } => fields.get(name).cloned(),
            _ => None,
        }
    }

    fn text(&self, value: &Value) -> Option<String> {
        self.heap.get_string(value.as_obj()?).map(str::to_string)
    }

    /// Run a method that must answer an object.
    fn call_object(
        &mut self,
        class: &str,
        method: &str,
        descriptor: &str,
        args: &[Value],
    ) -> Result<Value> {
        let value = self.execute_method(class, method, descriptor, args)?;
        if value.is_null() {
            return Err(failure(format!("{class}.{method} returned null")));
        }
        Ok(value)
    }

    /// A `Property`'s name and its values' names, in the property's order.
    fn property_facts(&mut self, property: &Value) -> Result<(String, Vec<String>)> {
        let name = self.call_object(
            PROPERTY,
            "getName",
            "()Ljava/lang/String;",
            &[property.clone()],
        )?;
        let name = self
            .text(&name)
            .ok_or_else(|| failure("a property name was not a string"))?;
        let values = self.call_object(
            PROPERTY,
            "getPossibleValues",
            "()Ljava/util/List;",
            &[property.clone()],
        )?;
        let mut names = Vec::new();
        for value in self.collection_elements(&values)? {
            names.push(self.property_value_name(property, &value)?);
        }
        Ok((name, names))
    }

    /// `Property.getName(value)`: how the property spells one of its values.
    fn property_value_name(&mut self, property: &Value, value: &Value) -> Result<String> {
        let name = self.call_object(
            PROPERTY,
            "getName",
            "(Ljava/lang/Comparable;)Ljava/lang/String;",
            &[property.clone(), value.clone()],
        )?;
        self.text(&name)
            .ok_or_else(|| failure("a property value name was not a string"))
    }

    /// `name=value,...` for a state, from the keys and values its constructor stored.
    pub(crate) fn state_key(&mut self, state: &Value) -> Result<String> {
        let keys = self.instance_field(state, "propertyKeys");
        let values = self.instance_field(state, "propertyValues");
        let array = |interp: &Interpreter, value: &Option<Value>| match value
            .as_ref()
            .and_then(Value::as_obj)
            .and_then(|id| interp.heap.get(id))
        {
            Some(JavaObject::Array { elements, .. }) => Some(elements.clone()),
            _ => None,
        };
        let (Some(keys), Some(values)) = (array(self, &keys), array(self, &values)) else {
            return Err(failure("a state has no propertyKeys/propertyValues"));
        };
        let mut parts = Vec::new();
        for (key, value) in keys.iter().zip(&values) {
            let name =
                self.call_object(PROPERTY, "getName", "()Ljava/lang/String;", &[key.clone()])?;
            let name = self.text(&name).unwrap_or_default();
            parts.push(format!("{name}={}", self.property_value_name(key, value)?));
        }
        Ok(parts.join(","))
    }

    /// [`Self::shape_boxes`], answered from `memo` for a shape object already
    /// read. The heap is not collected while the shapes are read, so an
    /// object id names one shape for the whole pass.
    fn shape_boxes_memo(
        &mut self,
        shape: &Value,
        memo: &mut HashMap<usize, Vec<[f64; 6]>>,
    ) -> Result<Vec<[f64; 6]>> {
        let Some(id) = shape.as_obj() else {
            return self.shape_boxes(shape);
        };
        if let Some(boxes) = memo.get(&id) {
            return Ok(boxes.clone());
        }
        let boxes = self.shape_boxes(shape)?;
        memo.insert(id, boxes.clone());
        Ok(boxes)
    }

    /// A `VoxelShape` as `toAabbs()` boxes.
    fn shape_boxes(&mut self, shape: &Value) -> Result<Vec<[f64; 6]>> {
        let list = self.call_object(
            VOXEL_SHAPE,
            "toAabbs",
            "()Ljava/util/List;",
            &[shape.clone()],
        )?;
        let mut boxes = Vec::new();
        for aabb in self.collection_elements(&list)? {
            let mut b = [0.0; 6];
            for (slot, name) in b
                .iter_mut()
                .zip(["minX", "minY", "minZ", "maxX", "maxY", "maxZ"])
            {
                *slot = self
                    .instance_field(&aabb, name)
                    .ok_or_else(|| failure(format!("an AABB has no {name}")))?
                    .as_double();
            }
            boxes.push(b);
        }
        Ok(boxes)
    }

    fn sound_type(&mut self, state: &Value) -> Result<BlockSound> {
        // A block whose `Properties` did not carry a `SoundType` still has
        // vanilla's default (stone); a missing sound must not drop the block.
        let default = || BlockSound {
            volume: 1.0,
            pitch: 1.0,
            break_sound: "minecraft:block.stone.break".into(),
            step: "minecraft:block.stone.step".into(),
            place: "minecraft:block.stone.place".into(),
            hit: "minecraft:block.stone.hit".into(),
            fall: "minecraft:block.stone.fall".into(),
        };
        let sound = match self.call_object(
            STATE_BASE,
            "getSoundType",
            "()Lnet/minecraft/world/level/block/SoundType;",
            std::slice::from_ref(state),
        ) {
            Ok(sound) => sound,
            Err(_) => return Ok(default()),
        };
        let event = |interp: &Interpreter, name: &str| {
            interp
                .instance_field(&sound, name)
                .and_then(|e| interp.instance_field(&e, "location"))
                .and_then(|id| identifier_text(&interp.heap, &id))
                .ok_or_else(|| failure(format!("SoundType.{name} has no location")))
        };
        let number = |interp: &Interpreter, name: &str| {
            interp
                .instance_field(&sound, name)
                .map(|v| v.as_float())
                .ok_or_else(|| failure(format!("SoundType.{name} is missing")))
        };
        let parts = (
            number(self, "volume"),
            number(self, "pitch"),
            event(self, "breakSound"),
            event(self, "stepSound"),
            event(self, "placeSound"),
            event(self, "hitSound"),
            event(self, "fallSound"),
        );
        match parts {
            (Ok(volume), Ok(pitch), Ok(break_sound), Ok(step), Ok(place), Ok(hit), Ok(fall)) => {
                Ok(BlockSound {
                    volume,
                    pitch,
                    break_sound,
                    step,
                    place,
                    hit,
                    fall,
                })
            }
            _ => Ok(default()),
        }
    }

    /// The facts of a state that are fields its constructor stored or trivial
    /// accessors. Shapes are read separately (`read_block_shapes`).
    fn state_facts(
        &mut self,
        state: &Value,
        getter: &Value,
        pos: &Value,
    ) -> Result<RegisteredState> {
        let this = std::slice::from_ref(state);
        let hardness = self
            .execute_method(
                STATE_BASE,
                "getDestroySpeed",
                "(Lnet/minecraft/world/level/BlockGetter;Lnet/minecraft/core/BlockPos;)F",
                &[state.clone(), getter.clone(), pos.clone()],
            )?
            .as_float();
        let emission = self
            .execute_method(STATE_BASE, "getLightEmission", "()I", this)?
            .as_int();
        let requires_tool = self
            .execute_method(STATE_BASE, "requiresCorrectToolForDrops", "()Z", this)?
            .as_int()
            != 0;
        let can_occlude = self
            .execute_method(STATE_BASE, "canOcclude", "()Z", this)?
            .as_int()
            != 0;
        // A mod block without a map colour is still a block; the colour is
        // only what the map draws it as, so a missing one is 0, not an error.
        let map_color_id = self
            .call_object(
                STATE_BASE,
                "getMapColor",
                "(Lnet/minecraft/world/level/BlockGetter;Lnet/minecraft/core/BlockPos;)Lnet/minecraft/world/level/material/MapColor;",
                &[state.clone(), getter.clone(), pos.clone()],
            )
            .ok()
            .and_then(|color| self.instance_field(&color, "id"))
            .map_or(0, |id| id.as_int());
        let sound = self.sound_type(state)?;
        Ok(RegisteredState {
            hardness,
            emission: emission.clamp(0, 255) as u8,
            requires_tool,
            map_color_id: map_color_id.clamp(0, 255) as u8,
            can_occlude,
            sound,
            outline: None,
            collision: None,
        })
    }

    /// The `EmptyBlockGetter` and `BlockPos.ZERO` the facts are read with.
    fn fact_arguments(&mut self) -> Option<(Value, Value)> {
        let getter = self
            .ensure_initialized("net/minecraft/world/level/EmptyBlockGetter")
            .map(|()| {
                self.resolve_static_field("net/minecraft/world/level/EmptyBlockGetter", "INSTANCE")
            });
        let pos = self
            .ensure_initialized("net/minecraft/core/BlockPos")
            .map(|()| self.resolve_static_field("net/minecraft/core/BlockPos", "ZERO"));
        match (getter, pos) {
            (Ok(getter), Ok(pos)) => Some((getter, pos)),
            _ => {
                log::warn!("EmptyBlockGetter or BlockPos could not be initialised; no block facts");
                None
            }
        }
    }

    /// The states of a block object, as `(key, state)`.
    pub(crate) fn block_states(&mut self, block: &Value) -> Result<Vec<(String, Value)>> {
        let definition = self
            .instance_field(block, "stateDefinition")
            .ok_or_else(|| failure("the block has no stateDefinition"))?;
        let list = self.call_object(
            STATE_DEFINITION,
            "getPossibleStates",
            "()Lcom/google/common/collect/ImmutableList;",
            &[definition],
        )?;
        let mut states = Vec::new();
        for state in self.collection_elements(&list)? {
            states.push((self.state_key(&state)?, state));
        }
        Ok(states)
    }

    /// Whether every state of a block answers the same shapes. `BlockBehaviour`
    /// (the class every block derives from) answers `Shapes.block()` from
    /// `getShape` and, from `getCollisionShape`, that shape or `Shapes.empty()`
    /// depending on the block-wide `hasCollision`; neither reads the state. A
    /// class below it in the chain that declares either method may.
    fn shapes_are_state_independent(&self, chain: &[String]) -> bool {
        const BASE: &str = "net.minecraft.world.level.block.state.BlockBehaviour";
        const STATE: &str = "(Lnet/minecraft/world/level/block/state/BlockState;";
        for class in chain {
            if class == BASE {
                return true;
            }
            let Some(loaded) = self.loader.get(&class.replace('.', "/")) else {
                return false;
            };
            if loaded.methods.keys().any(|(method, descriptor)| {
                matches!(method.as_str(), "getShape" | "getCollisionShape")
                    && descriptor.starts_with(STATE)
            }) {
                return false;
            }
        }
        false
    }

    /// The components the `Item.Properties` of the `item` object set (the snapshot `Item.<init>` keeps on the item).
    fn block_item_components(
        &self,
        item: usize,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        let Some(JavaObject::Instance { fields, .. }) = self.heap.get(item) else {
            return None;
        };
        let snapshot = fields.get(ITEM_FIELD).and_then(Value::as_obj)?;
        self.heap
            .get_native::<ItemProperties>(snapshot)
            .map(|p| p.components.clone())
    }

    /// The vanilla methods that the mod classes of `chain` (binary names,
    /// most derived first) override: for each instance method a non-vanilla
    /// class declares, the most derived vanilla class after it that declares
    /// the same name and descriptor, as `<vanilla class>.<name>`.
    fn vanilla_overrides(&self, chain: &[String]) -> Vec<String> {
        const PRIVATE: u16 = 0x0002;
        let classes: Vec<(&String, Option<&crate::class::Class>)> = chain
            .iter()
            .map(|name| (name, self.loader.get(&name.replace('.', "/"))))
            .collect();
        let mut out = std::collections::BTreeSet::new();
        for (at, (name, class)) in classes.iter().enumerate() {
            if name.starts_with(VANILLA) {
                continue;
            }
            let Some(class) = class else { continue };
            for ((method, descriptor), info) in &class.methods {
                if info.is_static || info.access & PRIVATE != 0 || method.starts_with('<') {
                    continue;
                }
                let declarer = classes[at + 1..].iter().find(|(vanilla, c)| {
                    vanilla.starts_with(VANILLA)
                        && c.is_some_and(|c| {
                            c.methods
                                .get(&(method.clone(), descriptor.clone()))
                                .is_some_and(|m| !m.is_static && m.access & PRIVATE == 0)
                        })
                });
                if let Some((vanilla, _)) = declarer {
                    out.insert(format!("{vanilla}.{method}"));
                }
            }
        }
        out.into_iter().collect()
    }

    /// Everything about one registered block, read through the block's own
    /// methods. A fact that cannot be read goes into `unreadable`.
    fn block_facts(
        &mut self,
        record: &BlockRecord,
        getter: &Value,
        pos: &Value,
    ) -> RegisteredBlock {
        let block = Value::Object(Some(record.object));
        let block_item = self
            .heap
            .registered_block_items
            .iter()
            .find(|(_, object, _)| *object == record.object)
            .cloned();
        let mut facts = RegisteredBlock {
            id: record.id.clone(),
            class: Vec::new(),
            has_block_entity: false,
            overrides: Vec::new(),
            properties: BTreeMap::new(),
            default: String::new(),
            item_components: block_item
                .as_ref()
                .and_then(|(_, _, item)| self.block_item_components(*item))
                .unwrap_or_default(),
            item: block_item.map(|(id, _, _)| id),
            container: Default::default(),
            explosion_resistance: 0.0,
            friction: 0.0,
            speed_factor: 0.0,
            jump_factor: 0.0,
            states: BTreeMap::new(),
            unreadable: Vec::new(),
        };
        let mut class = match self.heap.get(record.object) {
            Some(JavaObject::Instance { class_name, .. }) => class_name.clone(),
            _ => String::new(),
        };
        facts.has_block_entity = self.class_is_assignable_to(&class, ENTITY_BLOCK);
        for _ in 0..64 {
            if class.is_empty() || class == "java/lang/Object" {
                break;
            }
            facts.class.push(class.replace('/', "."));
            let _ = self.loader.get_or_load(&class);
            class = self
                .loader
                .get(&class)
                .and_then(|c| c.super_class.clone())
                .unwrap_or_default();
        }
        facts.overrides = self.vanilla_overrides(&facts.class);

        for (name, method) in [
            ("explosion_resistance", "getExplosionResistance"),
            ("friction", "getFriction"),
            ("speed_factor", "getSpeedFactor"),
            ("jump_factor", "getJumpFactor"),
        ] {
            match self.execute_method(BLOCK, method, "()F", std::slice::from_ref(&block)) {
                Ok(value) => {
                    let value = value.as_float();
                    match name {
                        "explosion_resistance" => facts.explosion_resistance = value,
                        "friction" => facts.friction = value,
                        "speed_factor" => facts.speed_factor = value,
                        _ => facts.jump_factor = value,
                    }
                }
                Err(error) => facts.unreadable.push(format!("{name}: {error}")),
            }
        }

        let Some(definition) = self.instance_field(&block, "stateDefinition") else {
            facts
                .unreadable
                .push("the block has no stateDefinition".into());
            return facts;
        };
        match self
            .call_object(
                STATE_DEFINITION,
                "getProperties",
                "()Ljava/util/Collection;",
                std::slice::from_ref(&definition),
            )
            .and_then(|list| self.collection_elements(&list))
        {
            Ok(properties) => {
                for property in properties {
                    match self.property_facts(&property) {
                        Ok((name, values)) => {
                            facts.properties.insert(name, values);
                        }
                        Err(error) => facts.unreadable.push(format!("property: {error}")),
                    }
                }
            }
            Err(error) => facts.unreadable.push(format!("properties: {error}")),
        }

        match self.instance_field(&block, "defaultBlockState") {
            Some(state) if !state.is_null() => match self.state_key(&state) {
                Ok(key) => facts.default = key,
                Err(error) => facts.unreadable.push(format!("default state: {error}")),
            },
            _ => facts
                .unreadable
                .push("the block registered no default state".into()),
        }

        if facts.has_block_entity {
            if let Some(state) = self
                .instance_field(&block, "defaultBlockState")
                .filter(|s| !s.is_null())
            {
                facts.container = self.read_container(&block, &state);
            }
        }

        match self.block_states(&block) {
            Ok(states) => {
                for (key, state) in states {
                    match self.state_facts(&state, getter, pos) {
                        Ok(state_facts) => {
                            facts.states.insert(key, state_facts);
                        }
                        Err(error) => facts.unreadable.push(format!("state {key}: {error}")),
                    }
                }
            }
            Err(error) => facts.unreadable.push(format!("states: {error}")),
        }
        facts
    }

    /// Read the facts of every registered block except shapes. Each block runs
    /// under its own instruction `budget`.
    pub(crate) fn collect_block_facts(&mut self, budget: u64) {
        let records = self.heap.registered_blocks.clone();
        if records.is_empty() {
            return;
        }
        let previous_budget = self.step_budget;
        self.step_budget = Some(budget);
        let Some((getter, pos)) = self.fact_arguments() else {
            self.step_budget = previous_budget;
            return;
        };
        let mut facts = Vec::with_capacity(records.len());
        for record in &records {
            self.step_budget = Some(budget);
            let started = std::time::Instant::now();
            let block = self.block_facts(record, &getter, &pos);
            log::debug!(
                "block {} read: {} states, {} instructions, {:?}",
                record.id,
                block.states.len(),
                budget - self.step_budget.unwrap_or(0),
                started.elapsed()
            );
            for line in &block.unreadable {
                log::warn!("block {}: {line}", record.id);
            }
            facts.push(block);
        }
        self.heap.block_ids = facts.iter().map(|block| block.id.clone()).collect();
        self.heap.block_facts = facts;
        self.step_budget = previous_budget;
    }

    /// Compute the outline and collision shapes of the registered blocks named
    /// in `only` (all of them when it is empty) by calling the states'
    /// `getShape`/`getCollisionShape` with `EmptyBlockGetter`, `BlockPos.ZERO`
    /// and an empty `CollisionContext`. Each call runs under `per_shape`
    /// instructions and each block under that same total, so one block with
    /// many states cannot hold the whole start; a shape that does not fit
    /// stays `None` and is named in the block's `unreadable`.
    ///
    /// A block whose classes do not replace `getShape`/`getCollisionShape`
    /// answers the same shapes for every state (both base implementations in
    /// `BlockBehaviour` ignore the state: `Shapes.block()`, and that or
    /// `Shapes.empty()` from the block-wide `hasCollision`), so one state's
    /// pair is read and given to the rest.
    pub(crate) fn read_block_shapes(&mut self, only: &[&str], per_shape: u64) {
        let Some((getter, pos)) = self.fact_arguments() else {
            return;
        };
        let previous_budget = self.step_budget;
        let records = self.heap.registered_blocks.clone();
        let mut facts = std::mem::take(&mut self.heap.block_facts);
        let total = if only.is_empty() {
            facts.len()
        } else {
            facts
                .iter()
                .filter(|b| only.contains(&b.id.as_str()))
                .count()
        };
        // A cold start spends most of its time here (mdm: 344 blocks, ~13 s),
        // on the content-root thread. One line per quarter tells a log reader —
        // and a player looking at a loading screen that stopped moving — that
        // it is still working.
        let started = std::time::Instant::now();
        // A `VoxelShape` is immutable and blocks share them: the states of a
        // block that differ only in a property the shape ignores (waterlogged,
        // lit, powered) answer the same object, and a block's outline is often
        // its collision shape. Its boxes are read once per object.
        let mut boxes_of: HashMap<usize, Vec<[f64; 6]>> = HashMap::new();
        let mut done = 0usize;
        let mut next_report = 1usize;
        if total > 0 {
            log::info!("Fabric: reading the shapes of {total} registered block(s)");
        }
        for block in &mut facts {
            if !only.is_empty() && !only.contains(&block.id.as_str()) {
                continue;
            }
            let Some(record) = records.iter().find(|r| r.id == block.id) else {
                continue;
            };
            let block_started = std::time::Instant::now();
            // The block's total, not one budget per state: a block whose
            // shapes are expensive cannot spend `per_shape` for every state.
            // Enumerating the states is part of it.
            let mut remaining = per_shape;
            self.step_budget = Some(remaining);
            let states = match self.block_states(&Value::Object(Some(record.object))) {
                Ok(states) => states,
                Err(error) => {
                    block.unreadable.push(format!("shapes: {error}"));
                    continue;
                }
            };
            remaining = remaining.saturating_sub(per_shape - self.step_budget.unwrap_or(0));
            let uniform = self.shapes_are_state_independent(&block.class);
            let mut reused = 0usize;
            let mut over_budget = 0usize;
            let mut first: Option<(Vec<[f64; 6]>, Vec<[f64; 6]>)> = None;
            for (key, state) in states {
                let Some(fact) = block.states.get_mut(&key) else {
                    continue;
                };
                if let Some((outline, collision)) = &first {
                    fact.outline = Some(outline.clone());
                    fact.collision = Some(collision.clone());
                    reused += 1;
                    continue;
                }
                if remaining == 0 {
                    over_budget += 1;
                    continue;
                }
                let mut read = (None, None);
                for (slot, label, method) in [
                    (0, "outline", "getShape"),
                    (1, "collision", "getCollisionShape"),
                ] {
                    let budget = remaining.min(per_shape);
                    self.step_budget = Some(budget);
                    let args = [state.clone(), getter.clone(), pos.clone()];
                    let boxes = self
                        .call_object(STATE_BASE, method, SHAPE_DESCRIPTOR, &args)
                        .and_then(|shape| self.shape_boxes_memo(&shape, &mut boxes_of));
                    remaining = remaining.saturating_sub(budget - self.step_budget.unwrap_or(0));
                    match boxes {
                        Ok(boxes) if slot == 0 => read.0 = Some(boxes),
                        Ok(boxes) => read.1 = Some(boxes),
                        Err(error) => {
                            log::warn!("block {}: state {key}: {label}: {error}", block.id);
                            block
                                .unreadable
                                .push(format!("state {key}: {label}: {error}"));
                        }
                    }
                }
                fact.outline = read.0.clone();
                fact.collision = read.1.clone();
                if uniform {
                    if let (Some(outline), Some(collision)) = read {
                        first = Some((outline, collision));
                    }
                }
            }
            if over_budget > 0 {
                let line = format!(
                    "{over_budget} state(s) left unread: the block's shapes overran the \
                     {per_shape}-instruction budget"
                );
                log::warn!("block {}: {line}", block.id);
                block.unreadable.push(line);
            }
            log::debug!(
                "block {} shapes: {} states, {reused} reused{}, {} instructions, {:?}",
                block.id,
                block.states.len(),
                if uniform {
                    " (the block answers one shape for every state)"
                } else {
                    ""
                },
                per_shape - remaining,
                block_started.elapsed()
            );
            done += 1;
            if done * 4 >= next_report * total {
                log::info!(
                    "Fabric: {done}/{total} registered block shapes read in {:.1?}",
                    started.elapsed()
                );
                next_report += 1;
            }
        }
        if only.is_empty() {
            self.heap.block_facts_shapes = true;
        }
        self.heap.block_facts = facts;
        self.step_budget = previous_budget;
    }
}
