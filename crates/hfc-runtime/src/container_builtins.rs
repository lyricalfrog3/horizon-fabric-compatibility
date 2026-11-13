//! The inventory a registered block's block entity has.
//!
//! A block that implements `EntityBlock` makes its block entity in
//! `newBlockEntity`. The runtime builds one from the block's default state and
//! asks it, through its own methods, what the host needs to give the block a
//! container: `getContainerSize()`, the menu `createMenu(int, Inventory)`
//! builds (the menu's real `MenuType`), `getDisplayName()` (with the
//! translation key when it was a `Component.translatable`), `getMaxStackSize()`
//! and, for a `WorldlyContainer`, `getSlotsForFace(Direction)` per face.
//! Nothing is guessed from class names; a block entity that is not a vanilla
//! container (it does not extend `BaseContainerBlockEntity`) has none, and one
//! whose menu is not a vanilla `MenuType` is reported, not given a stand-in.
//! The per-stack rules (`canPlaceItem`, `canPlaceItemThroughFace`,
//! `canTakeItemThroughFace`) cannot be read as one fact and are not run.

use crate::bridge::blocks::{BlockContainer, ContainerRead};
use crate::interp::Interpreter;
use crate::value::{JavaObject, Value};
use hfc_core::{Error, Result};

const ENTITY_BLOCK: &str = "net/minecraft/world/level/block/EntityBlock";
const BASE_CONTAINER: &str = "net/minecraft/world/level/block/entity/BaseContainerBlockEntity";
const CONTAINER: &str = "net/minecraft/world/Container";
const WORLDLY_CONTAINER: &str = "net/minecraft/world/WorldlyContainer";
const DIRECTION: &str = "net/minecraft/core/Direction";
/// `Direction`'s constants, in the order `BlockContainer::faces` uses.
const FACES: [&str; 6] = ["DOWN", "UP", "NORTH", "SOUTH", "WEST", "EAST"];
const MENU_TYPE: &str = "net/minecraft/world/inventory/MenuType";
const INVENTORY: &str = "net/minecraft/world/entity/player/Inventory";
const MENU: &str = "net/minecraft/world/inventory/AbstractContainerMenu";

fn failure(message: impl Into<String>) -> Error {
    Error::new("interpreter", message.into())
}

impl Interpreter {
    /// Read the container of the registered block `block` standing in its
    /// default state `state`.
    pub(crate) fn read_container(&mut self, block: &Value, state: &Value) -> ContainerRead {
        match self.read_container_inner(block, state) {
            Ok(read) => read,
            Err(error) => ContainerRead::Unreadable(error.to_string()),
        }
    }

    fn read_container_inner(&mut self, block: &Value, state: &Value) -> Result<ContainerRead> {
        let pos = self.heap.alloc_block_pos(0, 0, 0);
        let entity = self.execute_method(
            ENTITY_BLOCK,
            "newBlockEntity",
            "(Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;)Lnet/minecraft/world/level/block/entity/BlockEntity;",
            &[block.clone(), Value::Object(Some(pos)), state.clone()],
        )?;
        let Some(entity_id) = entity.as_obj() else {
            return Ok(ContainerRead::NoBlockEntity);
        };
        let class = match self.heap.get(entity_id) {
            Some(JavaObject::Instance { class_name, .. }) => class_name.clone(),
            _ => return Err(failure("newBlockEntity returned no object")),
        };
        if !self.class_is_assignable_to(&class, BASE_CONTAINER) {
            return Ok(ContainerRead::NotVanillaContainer(class.replace('/', ".")));
        }
        let slots = self
            .execute_method(
                CONTAINER,
                "getContainerSize",
                "()I",
                std::slice::from_ref(&entity),
            )?
            .as_int();
        if slots <= 0 {
            return Err(failure(format!("getContainerSize() is {slots}")));
        }
        let display = self
            .execute_method(
                "net/minecraft/world/MenuProvider",
                "getDisplayName",
                "()Lnet/minecraft/network/chat/Component;",
                std::slice::from_ref(&entity),
            )
            .ok();
        let title = display
            .as_ref()
            .map(|component| crate::builtins::text_of_value(&self.heap, component));
        let title_key = display
            .as_ref()
            .and_then(|component| self.component_key(component));
        let mut notes = Vec::new();
        let max_stack_size = match self.execute_method(
            CONTAINER,
            "getMaxStackSize",
            "()I",
            std::slice::from_ref(&entity),
        ) {
            Ok(value) => {
                let size = value.as_int();
                if (1..=99).contains(&size) {
                    Some(size as u32)
                } else {
                    notes.push(format!(
                        "getMaxStackSize() is {size}, which no slot can hold"
                    ));
                    None
                }
            }
            Err(error) => {
                notes.push(format!("getMaxStackSize could not be read: {error}"));
                None
            }
        };
        let faces = if self.class_is_assignable_to(&class, WORLDLY_CONTAINER) {
            match self.worldly_faces(&entity, slots as usize) {
                Ok(faces) => Some(faces),
                Err(error) => {
                    notes.push(format!("getSlotsForFace could not be read: {error}"));
                    None
                }
            }
        } else {
            None
        };
        self.ensure_initialized(INVENTORY)?;
        let inventory = self.alloc_uninitialized(INVENTORY);
        let menu = self.execute_method(
            BASE_CONTAINER,
            "createMenu",
            "(ILnet/minecraft/world/entity/player/Inventory;)Lnet/minecraft/world/inventory/AbstractContainerMenu;",
            &[entity, Value::Int(0), Value::Object(Some(inventory))],
        )?;
        if menu.is_null() {
            return Err(failure("createMenu returned null"));
        }
        let menu_type = self.execute_method(
            MENU,
            "getType",
            "()Lnet/minecraft/world/inventory/MenuType;",
            &[menu],
        )?;
        let Some(menu_type) = self.menu_type_name(&menu_type)? else {
            return Ok(ContainerRead::Unreadable(
                "its menu's type is not one of vanilla's".into(),
            ));
        };
        Ok(ContainerRead::Container(BlockContainer {
            slots: slots as usize,
            menu_type,
            title,
            title_key,
            max_stack_size,
            faces,
            notes,
        }))
    }

    /// The translation key of a `Component.translatable` as this runtime built
    /// it (`key` on the component), if it was one.
    fn component_key(&self, component: &Value) -> Option<String> {
        let id = component.as_obj()?;
        let JavaObject::Instance { fields, .. } = self.heap.get(id)? else {
            return None;
        };
        let key = fields.get("key")?.as_obj()?;
        self.heap.get_string(key).map(str::to_string)
    }

    /// `WorldlyContainer.getSlotsForFace` for each of the six directions, in
    /// `Direction` order; slots outside the container are left out.
    fn worldly_faces(&mut self, entity: &Value, slots: usize) -> Result<[Vec<usize>; 6]> {
        self.ensure_initialized(DIRECTION)?;
        let mut faces: [Vec<usize>; 6] = std::array::from_fn(|_| Vec::new());
        for (at, name) in FACES.iter().enumerate() {
            let direction = self.resolve_static_field(DIRECTION, name);
            let array = self.execute_method(
                WORLDLY_CONTAINER,
                "getSlotsForFace",
                "(Lnet/minecraft/core/Direction;)[I",
                &[entity.clone(), direction],
            )?;
            let Some(id) = array.as_obj() else {
                return Err(failure(format!("getSlotsForFace({name}) returned null")));
            };
            let Some(JavaObject::Array { elements, .. }) = self.heap.get(id) else {
                return Err(failure(format!(
                    "getSlotsForFace({name}) did not return an int array"
                )));
            };
            faces[at] = elements
                .iter()
                .map(Value::as_int)
                .filter(|slot| (0..slots as i32).contains(slot))
                .map(|slot| slot as usize)
                .collect();
        }
        Ok(faces)
    }

    /// The id of one of vanilla's `MenuType` constants (`GENERIC_9x3` is
    /// `minecraft:generic_9x3`).
    fn menu_type_name(&mut self, menu_type: &Value) -> Result<Option<String>> {
        self.ensure_initialized(MENU_TYPE)?;
        let names: Vec<String> = self
            .loader
            .get(MENU_TYPE)
            .map(|c| c.static_fields.keys().cloned().collect())
            .unwrap_or_default();
        let mut found: Vec<String> = Vec::new();
        for name in names {
            if self.resolve_static_field(MENU_TYPE, &name) == *menu_type {
                found.push(name);
            }
        }
        found.sort();
        Ok(found
            .into_iter()
            .next()
            .map(|n| format!("minecraft:{}", n.to_ascii_lowercase())))
    }
}
