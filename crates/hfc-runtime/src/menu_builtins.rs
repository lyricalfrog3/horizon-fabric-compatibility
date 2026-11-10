//! The menu a mod's own `MenuProvider` builds: `createMenu(int, Inventory)`
//! run on the provider, and the slots and title read off the
//! `AbstractContainerMenu` it answers.
//!
//! A menu's slots are `AbstractContainerMenu.slots`, a list of `Slot`s; a slot
//! reads `Slot.container` (the `Container` it shows) and `Slot.getContainerSlot`
//! for its index. That is the same shape vanilla's own `ChestMenu` builds, so a
//! mod's `CookingPotMenu` needs nothing special: its own `Slot` subclasses and
//! filters are what `mayPlace` answers.

use crate::bridge::blocks::OpenedMenu;
use crate::interp::Interpreter;
use crate::minecraft::ItemStack;
use crate::value::Value;
use hfc_core::Result;

const MENU: &str = "net/minecraft/world/inventory/AbstractContainerMenu";
const SLOT: &str = "net/minecraft/world/inventory/Slot";
const MENU_PROVIDER: &str = "net/minecraft/world/MenuProvider";
const INVENTORY: &str = "net/minecraft/world/entity/player/Inventory";

impl Interpreter {
    /// The menu `provider.createMenu(0, inventory)` builds, as the host shows
    /// it. `None` when the provider makes none.
    pub(crate) fn menu_of_provider(&mut self, provider: usize) -> Option<OpenedMenu> {
        let result = self.menu_of_provider_inner(provider);
        result.ok().flatten()
    }

    fn menu_of_provider_inner(&mut self, provider: usize) -> Result<Option<OpenedMenu>> {
        let provider = Value::Object(Some(provider));
        // The title is the menu's own `getDisplayName` answer, kept as the
        // `Component` the client builds its screen from (and flattened to text
        // for the host's grid fallback).
        let component = self
            .execute_method(
                MENU_PROVIDER,
                "getDisplayName",
                "()Lnet/minecraft/network/chat/Component;",
                std::slice::from_ref(&provider),
            )
            .ok()
            .filter(|component| !component.is_null())
            .unwrap_or(Value::NULL);
        let title = crate::builtins::text_of_value(&self.heap, &component);
        self.ensure_initialized(INVENTORY)?;
        let inventory = self.alloc_uninitialized(INVENTORY);
        // `MenuConstructor.createMenu(int, Inventory)`, or Fabric's
        // `ExtendedMenuProvider.createMenu(int, Inventory, Player)` (what a
        // farmer's delight pot implements): try the provider's own class first,
        // then the interface's two-argument form.
        let player = self.alloc_uninitialized("net/minecraft/world/entity/player/Player");
        let menu = {
            let three = self.execute_method(
                &class_of(&self.heap, provider.as_obj()),
                "createMenu",
                "(ILnet/minecraft/world/entity/player/Inventory;Lnet/minecraft/world/entity/player/Player;)Lnet/minecraft/world/inventory/AbstractContainerMenu;",
                &[
                    provider.clone(),
                    Value::Int(0),
                    Value::Object(Some(inventory)),
                    Value::Object(Some(player)),
                ],
            );
            match three {
                Ok(value) if !value.is_null() => Ok(value),
                _ => self.execute_method(
                    MENU_PROVIDER,
                    "createMenu",
                    "(ILnet/minecraft/world/entity/player/Inventory;)Lnet/minecraft/world/inventory/AbstractContainerMenu;",
                    &[provider.clone(), Value::Int(0), Value::Object(Some(inventory))],
                ),
            }
        }?;
        let Some(menu_id) = menu.as_obj() else {
            return Ok(None);
        };
        // The mod's own screen, when the client knows how to build one for
        // this menu type: `MenuScreens.SCREENS` is the client's own map from
        // `MenuType` to the `Screen` constructor that menu registered. The
        // menu is already built, so the constructor is called directly with
        // it (`ScreenConstructor.create`) instead of the network path
        // (`fromPacket`), which would re-create the menu from a packet. A
        // failure here is logged, not faked: the host then draws its grid
        // fallback for a menu type the client could not screen.
        match self.open_menu_screen(&menu, &component, inventory) {
            Ok(Some(screen)) => log::debug!("Fabric: opened menu screen {screen:?}"),
            Ok(None) => {}
            Err(error) => log::error!("Fabric: the menu's screen failed to open: {error}"),
        }
        // `AbstractContainerMenu.slots`: a `NonNullList<Slot>`.
        let slots = self
            .execute_method(MENU, "slotCount", "()I", &[menu.clone()])
            .ok()
            .map(|value| value.as_int())
            .filter(|count| *count > 0)
            .or_else(|| {
                self.instance_field(&menu, "slots")
                    .and_then(|slots| self.collection_elements(&slots).ok())
                    .map(|slots| slots.len() as i32)
            })
            .unwrap_or(0);
        let slots = slots.max(0) as usize;
        let slot_values = self
            .instance_field(&menu, "slots")
            .and_then(|slots| self.collection_elements(&slots).ok())
            .unwrap_or_default();
        let mut items = vec![None; slots];
        let mut input = Vec::new();
        for (index, slot) in slot_values.iter().take(slots).enumerate() {
            let Some(stack) = self
                .execute_method(
                    SLOT,
                    "getItem",
                    "()Lnet/minecraft/world/item/ItemStack;",
                    std::slice::from_ref(slot),
                )
                .ok()
                .and_then(|stack| stack.as_obj())
                .and_then(|id| self.heap.get_native::<ItemStack>(id))
                .map(|stack| (stack.item.id.clone(), stack.count))
                .filter(|(item, count)| *count > 0 && item != "minecraft:air")
            else {
                continue;
            };
            items[index] = Some(stack);
        }
        // A slot the menu shows but whose `mayPlace` refuses everything (a
        // result slot) takes no click; the host only fills `input`.
        for index in 0..slots {
            let Some(slot) = slot_values.get(index) else {
                break;
            };
            let probe = self
                .heap
                .alloc_item_stack(ItemStack::new("minecraft:stone", 1));
            let takes = self
                .execute_method(
                    SLOT,
                    "mayPlace",
                    "(Lnet/minecraft/world/item/ItemStack;)Z",
                    &[slot.clone(), Value::Object(Some(probe))],
                )
                .ok()
                .map(|value| value.as_bool())
                .unwrap_or(false);
            if takes {
                input.push(index);
            }
        }
        let _ = menu_id;
        Ok(Some(OpenedMenu {
            title,
            slots,
            items,
            input,
        }))
    }

    /// Open the screen a menu type registers with the client's own
    /// `MenuScreens` (vanilla's map for vanilla menus and the one
    /// `ExtendedMenuType` registrations add for mods'). `None` from there
    /// means the client has no screen for the menu type; the host then draws
    /// its grid fallback. A constructor that throws is an error, not a
    /// fallback: the mod registered a screen and it should not silently go
    /// missing.
    fn open_menu_screen(
        &mut self,
        menu: &Value,
        title: &Value,
        inventory: usize,
    ) -> Result<Option<Value>> {
        let menu_type = self.execute_method(
            MENU,
            "getType",
            "()Lnet/minecraft/world/inventory/MenuType;",
            std::slice::from_ref(menu),
        )?;
        let Some(ctor) = self
            .resolve_static_field("net/minecraft/client/gui/screens/MenuScreens", "SCREENS")
            .as_obj()
            .and_then(|id| {
                self.execute_method(
                    "java/util/Map",
                    "get",
                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                    &[Value::Object(Some(id)), menu_type],
                )
                .ok()
                .filter(|ctor| !ctor.is_null())
            })
        else {
            return Ok(None);
        };
        let screen = self.execute_method(
            "net/minecraft/client/gui/screens/MenuScreens$ScreenConstructor",
            "create",
            "(Lnet/minecraft/world/inventory/AbstractContainerMenu;Lnet/minecraft/world/entity/player/Inventory;Lnet/minecraft/network/chat/Component;)Lnet/minecraft/client/gui/screens/Screen;",
            &[ctor, menu.clone(), Value::Object(Some(inventory)), title.clone()],
        )?;
        let Some(mc) = self
            .heap
            .get_singleton("net/minecraft/client/Minecraft")
            .map(|id| Value::Object(Some(id)))
        else {
            // No client to show it in (a server-side open): nothing to open.
            return Ok(None);
        };
        self.execute_method(
            "net/minecraft/client/Minecraft",
            "setScreen",
            "(Lnet/minecraft/client/gui/screens/Screen;)V",
            &[mc, screen.clone()],
        )?;
        Ok(Some(screen))
    }
}

/// The class of a heap object, for a virtual call's declared class.
fn class_of(heap: &crate::heap::Heap, id: Option<usize>) -> String {
    id.and_then(|id| heap.get(id))
        .map(|object| object.class_name().to_string())
        .unwrap_or_else(|| "java/lang/Object".into())
}
