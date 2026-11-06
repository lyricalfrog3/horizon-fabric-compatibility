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
use crate::builtins::{identifier_text, registry_kind};
use crate::interp::Interpreter;
use crate::item_builtins::ITEM_FIELD;
use crate::value::{JavaObject, Value};
use hfc_core::{Error, Result};
use std::collections::BTreeMap;

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

    fn instance_field(&self, object: &Value, name: &str) -> Option<Value> {
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
        let sound = self.call_object(
            STATE_BASE,
            "getSoundType",
            "()Lnet/minecraft/world/level/block/SoundType;",
            std::slice::from_ref(state),
        )?;
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
        Ok(BlockSound {
            volume: number(self, "volume")?,
            pitch: number(self, "pitch")?,
            break_sound: event(self, "breakSound")?,
            step: event(self, "stepSound")?,
            place: event(self, "placeSound")?,
            hit: event(self, "hitSound")?,
            fall: event(self, "fallSound")?,
        })
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
        let color = self.call_object(
            STATE_BASE,
            "getMapColor",
            "(Lnet/minecraft/world/level/BlockGetter;Lnet/minecraft/core/BlockPos;)Lnet/minecraft/world/level/material/MapColor;",
            &[state.clone(), getter.clone(), pos.clone()],
        )?;
        let map_color_id = self
            .instance_field(&color, "id")
            .ok_or_else(|| failure("MapColor has no id"))?
            .as_int();
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
        self.step_budget = Some(budget);
        let Some((getter, pos)) = self.fact_arguments() else {
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
        self.heap.block_facts = facts;
    }

    /// Compute the outline and collision shapes of the registered blocks named
    /// in `only` (all of them when it is empty) by calling the states'
    /// `getShape`/`getCollisionShape` with `EmptyBlockGetter`, `BlockPos.ZERO`
    /// and an empty `CollisionContext`. Each call runs under `per_shape`
    /// instructions; a shape that does not fit stays `None` and is named in
    /// the block's `unreadable`.
    pub(crate) fn read_block_shapes(&mut self, only: &[&str], per_shape: u64) {
        let Some((getter, pos)) = self.fact_arguments() else {
            return;
        };
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
            self.step_budget = Some(per_shape);
            let states = match self.block_states(&Value::Object(Some(record.object))) {
                Ok(states) => states,
                Err(error) => {
                    block.unreadable.push(format!("shapes: {error}"));
                    continue;
                }
            };
            for (key, state) in states {
                let Some(fact) = block.states.get_mut(&key) else {
                    continue;
                };
                for (label, method) in [("outline", "getShape"), ("collision", "getCollisionShape")]
                {
                    self.step_budget = Some(per_shape);
                    let args = [state.clone(), getter.clone(), pos.clone()];
                    let boxes = self
                        .call_object(STATE_BASE, method, SHAPE_DESCRIPTOR, &args)
                        .and_then(|shape| self.shape_boxes(&shape));
                    match boxes {
                        Ok(boxes) if label == "outline" => fact.outline = Some(boxes),
                        Ok(boxes) => fact.collision = Some(boxes),
                        Err(error) => {
                            log::warn!("block {}: state {key}: {label}: {error}", block.id);
                            block
                                .unreadable
                                .push(format!("state {key}: {label}: {error}"));
                        }
                    }
                }
            }
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
    }
}
