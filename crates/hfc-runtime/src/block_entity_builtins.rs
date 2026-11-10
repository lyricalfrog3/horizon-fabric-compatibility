//! The block entities of registered blocks: made through the block's own
//! `EntityBlock.newBlockEntity`, kept per position, and ticked through the
//! block's own `getTicker` and `BlockEntityTicker.tick`.
//!
//! A block entity is the mod's real object (a `CookingPotBlockEntity`), so its
//! own fields, its `getContainerSize`/`getItem`/`setItem`, its menu and its
//! tick are the mod's code. The runtime only supplies what a JVM's Minecraft
//! would: the entity exists at the block's position in the level the code
//! sees, its `level` field is that level (`BlockEntity.setLevel`), and its
//! state is the block's own state object.

use crate::bridge::blocks::ContainerRead;
use crate::interp::Interpreter;
use crate::value::{JavaObject, Value};
use hfc_core::{Error, Result};

const ENTITY_BLOCK: &str = "net/minecraft/world/level/block/EntityBlock";
const BLOCK_ENTITY: &str = "net/minecraft/world/level/block/entity/BlockEntity";
const TICKER: &str = "net/minecraft/world/level/block/entity/BlockEntityTicker";

const NEW_BLOCK_ENTITY: &str = "(Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;)Lnet/minecraft/world/level/block/entity/BlockEntity;";
const GET_TICKER: &str = "(Lnet/minecraft/world/level/Level;Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/block/entity/BlockEntityType;)Lnet/minecraft/world/level/block/entity/BlockEntityTicker;";
const TICK: &str = "(Lnet/minecraft/world/level/Level;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/block/entity/BlockEntity;)V";

fn failure(message: impl Into<String>) -> Error {
    Error::new("interpreter", message.into())
}

impl Interpreter {
    /// The block entity of `block` at `pos`, made by the block's own
    /// `newBlockEntity` if the runtime has none for it yet. `None` for a block
    /// that makes none (a plain block, or one whose `newBlockEntity` answered
    /// null).
    pub(crate) fn block_entity_at(
        &mut self,
        block: &Value,
        id: &str,
        pos: [i32; 3],
        state: &Value,
        level: usize,
    ) -> Result<Option<usize>> {
        if let Some(entity) = self.heap.block_entities.get(&(pos, id.to_string())) {
            return Ok(Some(*entity));
        }
        let pos_value = self.heap.alloc_block_pos(pos[0], pos[1], pos[2]);
        let entity = self.execute_method(
            ENTITY_BLOCK,
            "newBlockEntity",
            NEW_BLOCK_ENTITY,
            &[block.clone(), Value::Object(Some(pos_value)), state.clone()],
        )?;
        let Some(entity_id) = entity.as_obj() else {
            return Ok(None);
        };
        // The entity knows where it is and which level it is in; the block
        // entity's own code reads both (`getBlockPos`, `getLevel`).
        self.set_block_entity_level(entity_id, level);
        self.heap
            .block_entities
            .insert((pos, id.to_string()), entity_id);
        if let Some(level) = self.heap.get_native_mut::<crate::minecraft::Level>(level) {
            level.block_entities.insert(pos, entity_id);
            level.made_block_entities.push((pos, entity_id));
        }
        Ok(Some(entity_id))
    }

    /// `BlockEntity.setLevel(level)`, the field the entity's own `getLevel`
    /// answers.
    fn set_block_entity_level(&mut self, entity: usize, level: usize) {
        let Some(JavaObject::Instance { fields, .. }) = self.heap.get_mut(entity) else {
            return;
        };
        fields.insert("level".into(), Value::Object(Some(level)));
    }

    /// The ticker the block's own `getTicker` declares for its entity, or
    /// `None` when it declares none.
    pub(crate) fn block_entity_ticker(
        &mut self,
        block: &Value,
        level: usize,
        state: &Value,
        entity: usize,
    ) -> Option<Value> {
        let entity_type = self
            .execute_method(
                BLOCK_ENTITY,
                "getType",
                "()Lnet/minecraft/world/level/block/entity/BlockEntityType;",
                &[Value::Object(Some(entity))],
            )
            .ok()?;
        if entity_type.is_null() {
            return None;
        }
        let ticker = self
            .execute_method(
                ENTITY_BLOCK,
                "getTicker",
                GET_TICKER,
                &[
                    block.clone(),
                    Value::Object(Some(level)),
                    state.clone(),
                    entity_type,
                ],
            )
            .ok()?;
        (!ticker.is_null()).then_some(ticker)
    }

    /// `BlockEntityTicker.tick(level, pos, state, entity)`: the block entity's
    /// own ticker, run by the host every game tick.
    pub(crate) fn tick_block_entity(
        &mut self,
        ticker: &Value,
        level: usize,
        pos: [i32; 3],
        state: &Value,
        entity: usize,
    ) -> Result<()> {
        let pos_value = self.heap.alloc_block_pos(pos[0], pos[1], pos[2]);
        self.execute_method(
            TICKER,
            "tick",
            TICK,
            &[
                ticker.clone(),
                Value::Object(Some(level)),
                Value::Object(Some(pos_value)),
                state.clone(),
                Value::Object(Some(entity)),
            ],
        )?;
        Ok(())
    }

    /// Whether the block's block entity is a container the mod's code fills
    /// itself: not a vanilla `BaseContainerBlockEntity`, but a `Container`
    /// (`CookingPotBlockEntity` holds an inventory of its own).
    pub(crate) fn block_entity_is_container(&mut self, entity: usize) -> bool {
        let class = match self.heap.get(entity) {
            Some(JavaObject::Instance { class_name, .. }) => class_name.clone(),
            _ => return false,
        };
        self.class_is_assignable_to(&class, "net/minecraft/world/Container")
    }

    /// The read of a block whose block entity is a `Container` of the mod's
    /// own: its slot count, and the menu its `createMenu` builds. `None` when
    /// the entity is not a container.
    pub(crate) fn read_mod_container(&mut self, entity: usize) -> Result<Option<ContainerRead>> {
        let value = Value::Object(Some(entity));
        let class = match self.heap.get(entity) {
            Some(JavaObject::Instance { class_name, .. }) => class_name.clone(),
            _ => return Ok(None),
        };
        if !self.class_is_assignable_to(&class, "net/minecraft/world/Container") {
            return Ok(None);
        }
        let slots = self
            .execute_method(
                "net/minecraft/world/Container",
                "getContainerSize",
                "()I",
                std::slice::from_ref(&value),
            )?
            .as_int();
        if slots <= 0 {
            return Err(failure(format!("getContainerSize() is {slots}")));
        }
        Ok(Some(ContainerRead::Unreadable(format!(
            "{}: a container of the mod's own with {slots} slots",
            class.replace('/', ".")
        ))))
    }
}
