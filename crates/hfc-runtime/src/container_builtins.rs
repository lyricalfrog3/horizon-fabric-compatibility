//! The inventory a registered block's block entity has.
//!
//! A block that implements `EntityBlock` makes its block entity in
//! `newBlockEntity`. The runtime builds one from the block's default state and
//! asks it, through its own methods, what the host needs to give the block a
//! container: `getContainerSize()`, the menu `createMenu(int, Inventory)`
//! builds (the menu's real `MenuType`) and `getDisplayName()`. Nothing is
//! guessed from class names; a block entity that is not a vanilla container
//! (it does not extend `BaseContainerBlockEntity`) has none, and one whose
//! menu is not a vanilla `MenuType` is reported, not given a stand-in.

use crate::bridge::blocks::{BlockContainer, ContainerRead};
use crate::interp::Interpreter;
use crate::value::{JavaObject, Value};
use hfc_core::{Error, Result};

const ENTITY_BLOCK: &str = "net/minecraft/world/level/block/EntityBlock";
const BASE_CONTAINER: &str = "net/minecraft/world/level/block/entity/BaseContainerBlockEntity";
const CONTAINER: &str = "net/minecraft/world/Container";
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
            .execute_method(CONTAINER, "getContainerSize", "()I", &[entity.clone()])?
            .as_int();
        if slots <= 0 {
            return Err(failure(format!("getContainerSize() is {slots}")));
        }
        let title = self
            .execute_method(
                "net/minecraft/world/MenuProvider",
                "getDisplayName",
                "()Lnet/minecraft/network/chat/Component;",
                &[entity.clone()],
            )
            .ok()
            .map(|component| crate::builtins::text_of_value(&self.heap, &component));

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
        }))
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
