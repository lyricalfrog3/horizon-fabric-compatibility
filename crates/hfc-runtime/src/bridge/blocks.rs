//! Blocks a mod registers in code, as facts the host can turn into a content-mod block.
//!
//! The runtime runs the mod's own block constructors and Minecraft's real
//! `Block`, `BlockBehaviour.Properties` and `StateDefinition` bytecode, so a
//! block's states, defaults and shapes come from the classes themselves (see
//! `block_builtins.rs`). The facts have the shape of the NeoForge oracle's
//! `Block` and `State` (`horizon-port-pack`'s `oracle.rs`).
//!
//! Only data is exported. A block's behaviour (`useWithoutItem`, ticks, block
//! entities) is mod bytecode the host never runs.

use crate::minecraft::Level;
use crate::value::{JavaObject, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A block's `SoundType`, as the oracle writes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockSound {
    pub volume: f32,
    pub pitch: f32,
    /// Sound event ids, `namespace:path`.
    pub break_sound: String,
    pub step: String,
    pub place: String,
    pub hit: String,
    pub fall: String,
}

/// One block state's facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisteredState {
    /// `BlockState.getDestroySpeed`.
    pub hardness: f32,
    /// `BlockState.getLightEmission`.
    pub emission: u8,
    pub requires_tool: bool,
    /// `MapColor.id` of `BlockState.getMapColor`.
    pub map_color_id: u8,
    pub can_occlude: bool,
    pub sound: BlockSound,
    /// `getShape(EmptyBlockGetter, BlockPos.ZERO)` as `toAabbs()` boxes,
    /// `[min_x, min_y, min_z, max_x, max_y, max_z]`. `None` when the shape
    /// could not be computed within the instruction budget (the block's
    /// `unreadable` says why); an empty list is a real empty shape.
    pub outline: Option<Vec<[f64; 6]>>,
    /// `getCollisionShape(EmptyBlockGetter, BlockPos.ZERO)`, likewise.
    pub collision: Option<Vec<[f64; 6]>>,
}

/// The inventory of a block whose block entity is a vanilla container
/// (`BaseContainerBlockEntity`), as its own methods answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockContainer {
    /// `getContainerSize()`.
    pub slots: usize,
    /// The id of the vanilla `MenuType` of the menu `createMenu` builds
    /// (`minecraft:generic_9x3`).
    pub menu_type: String,
    /// The plain text of `getDisplayName()`: a literal, or a translation the
    /// mod's language files resolved (the key itself when they did not).
    pub title: Option<String>,
    /// The translation key `getDisplayName()` answered, when it was a
    /// `Component.translatable` (`block.<ns>.<path>`, `container.<name>`); the
    /// host can translate it itself.
    pub title_key: Option<String>,
    /// `getMaxStackSize()`: how many of an item one slot holds. `None` when
    /// the call could not be read (`notes` says why), in which case the host
    /// keeps vanilla's own limit.
    pub max_stack_size: Option<u32>,
    /// `WorldlyContainer.getSlotsForFace`, in `Direction` order (down, up,
    /// north, south, west, east): the slots each face reaches. `None` when the
    /// block entity is not a `WorldlyContainer` (every slot is reachable from
    /// every face) or the read failed.
    pub faces: Option<[Vec<usize>; 6]>,
    /// What could not be read, one line each. Nothing was invented for it.
    pub notes: Vec<String>,
}

/// What reading a block's container found.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum ContainerRead {
    /// The block has no block entity (or none was asked for).
    #[default]
    NoBlockEntity,
    /// Its block entity is not a vanilla container; the class it is.
    NotVanillaContainer(String),
    Container(BlockContainer),
    /// Something the code did could not be run; the reason.
    Unreadable(String),
}

/// A block a mod registered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisteredBlock {
    /// `namespace:path`, from the registry key the mod registered it under.
    pub id: String,
    /// The block's class and its superclasses up to `Object` (excluded), most
    /// derived first, in binary form (`net.minecraft.world.level.block.Block`).
    pub class: Vec<String>,
    /// Whether the block implements `EntityBlock` (it makes a block entity).
    pub has_block_entity: bool,
    /// The vanilla methods the mod's own classes in `class` override, as
    /// `<vanilla class>.<method name>`, the vanilla class being the most
    /// derived one in `class` that declares the method (same name and
    /// descriptor; constructors, static and private methods excluded). Sorted,
    /// no repeats. It tells which of a vanilla class's behaviours the mod
    /// replaced: a bed subclass overriding `BedBlock.setPlacedBy` does not
    /// place vanilla's two halves.
    pub overrides: Vec<String>,
    /// Property name to its values' names, in the property's order.
    pub properties: BTreeMap<String, Vec<String>>,
    /// The default state's key (`facing=north,open=false`).
    pub default: String,
    /// The id of the `BlockItem` registered for this block, when the mod
    /// registered one (`Item.registerBlock`/`new BlockItem(block, ...)`).
    pub item: Option<String>,
    /// The data components the `BlockItem`'s `Item.Properties` set
    /// (`minecraft:max_stack_size`, `minecraft:rarity`, ...), as
    /// [`super::RegisteredItem::components`] holds them before its stack size
    /// and rarity are taken out. Empty without an item.
    pub item_components: serde_json::Map<String, serde_json::Value>,
    /// What the block's block entity says about its inventory, read by
    /// running its `newBlockEntity` and the container's own methods.
    pub container: ContainerRead,
    pub explosion_resistance: f32,
    pub friction: f32,
    pub speed_factor: f32,
    pub jump_factor: f32,
    /// Every state, by key (`property=value` pairs, comma separated, sorted by
    /// property name; empty for a block with no properties).
    pub states: BTreeMap<String, RegisteredState>,
    /// Facts the runtime could not read, one line each. A block listed here is
    /// missing what the line says; nothing was invented in its place.
    pub unreadable: Vec<String>,
}

impl super::FabricBridge {
    /// Compute the outline and collision shapes of the registered blocks whose
    /// ids are in `ids` (every block when `ids` is empty). `init` does not do
    /// this: a `VoxelShape` is built by Minecraft's own bytecode, which costs
    /// from thousands to tens of millions of interpreter instructions per
    /// shape (ten odd-sized boxes OR-ed together is about 10M), so the caller
    /// chooses which blocks are worth it. Each `getShape`/`getCollisionShape`
    /// call is capped; a shape over the cap stays `None` and the block's
    /// `unreadable` names it.
    pub fn read_block_shapes(&mut self, ids: &[&str]) {
        self.interp
            .read_block_shapes(ids, super::BLOCK_SHAPE_BUDGET);
    }

    /// The blocks mods registered in the block registry, in registration order.
    /// Filled in when `init` ends, from the real block objects.
    pub fn registered_blocks(&self) -> Vec<RegisteredBlock> {
        self.interp.heap.block_facts.clone()
    }
}

/// What a registered block's own `getStateForPlacement` decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// The block does not override `getStateForPlacement` (or is not a mod's
    /// block): the host places it as it would anyway.
    NotOverridden,
    /// The mod returned null: the placement is refused.
    Refused,
    /// The state the mod chose, as `property -> value` of the block.
    State(BTreeMap<String, String>),
    /// The mod's code could not be run to an answer; nothing was invented.
    Failed(String),
}

impl super::FabricBridge {
    /// Ask the registered block `request.block` for its placement state by
    /// running its real `getStateForPlacement` with a context built from
    /// `request`, over the host's real blocks around the position.
    pub fn state_for_placement(
        &mut self,
        host: &dyn super::HostBridge,
        request: &super::PlacementRequest,
    ) -> Placement {
        let overrides = self
            .interp
            .heap
            .block_facts
            .iter()
            .find(|b| b.id == request.block)
            .is_some_and(|b| {
                b.overrides
                    .iter()
                    .any(|o| o.ends_with(".getStateForPlacement"))
            });
        if !overrides {
            return Placement::NotOverridden;
        }
        let started = std::time::Instant::now();
        let world = world_around(host, request.pos);
        let previous = self.interp.step_budget;
        self.interp.step_budget = Some(crate::place_builtins::PLACEMENT_BUDGET);
        let result = self.interp.state_for_placement(request, &world);
        let used = crate::place_builtins::PLACEMENT_BUDGET - self.interp.step_budget.unwrap_or(0);
        self.interp.step_budget = previous;
        let took = started.elapsed();
        if took.as_millis() > 50 {
            log::warn!(
                "placing {} took {took:?} ({used} instructions)",
                request.block
            );
        }
        match result {
            Ok(None) => Placement::Refused,
            Ok(Some(key)) => Placement::State(crate::place_builtins::parse_state_key(&key)),
            Err(error) => Placement::Failed(error.to_string()),
        }
    }
}

/// The `BlockState` object of a registered block's state, made on demand.
/// Tests use it; nothing in the host calls it.
#[doc(hidden)]
impl super::FabricBridge {
    /// The mod's own inventory on a kept block entity (`getInventory()` of a
    /// block entity that holds an `ItemStackHandler` rather than a `Container`):
    /// how many slots it has, for tests.
    #[doc(hidden)]
    pub fn debug_block_entity_inventory_slots(
        &mut self,
        pos: [i32; 3],
        block: &str,
    ) -> Option<i32> {
        let handler = self.debug_block_entity_handler(pos, block)?;
        Some(
            self.interp
                .execute_method(
                    "vectorwing/farmersdelight/refabricated/inventory/ItemStackHandler",
                    "getSlotCount",
                    "()I",
                    &[handler],
                )
                .ok()?
                .as_int(),
        )
    }

    /// Put a stack in a kept block entity's own inventory slot, for a mod whose
    /// inventory is not a `Container` (`ItemStackHandler.setStackInSlot`).
    #[doc(hidden)]
    pub fn debug_set_block_entity_inventory(
        &mut self,
        pos: [i32; 3],
        block: &str,
        slot: i32,
        item: &str,
        count: i32,
    ) {
        let Some(handler) = self.debug_block_entity_handler(pos, block) else {
            return;
        };
        let stack = self
            .interp
            .heap
            .alloc_item_stack(crate::minecraft::ItemStack::new(item, count));
        let _ = self.interp.execute_method(
            "vectorwing/farmersdelight/refabricated/inventory/ItemStackHandler",
            "setStackInSlot",
            "(ILnet/minecraft/world/item/ItemStack;)V",
            &[handler, Value::Int(slot), Value::Object(Some(stack))],
        );
    }

    /// The stack in a kept block entity's own inventory slot, for a mod whose
    /// inventory is not a `Container`.
    #[doc(hidden)]
    pub fn debug_block_entity_inventory(
        &mut self,
        pos: [i32; 3],
        block: &str,
        slot: i32,
    ) -> Option<(String, i32)> {
        let handler = self.debug_block_entity_handler(pos, block)?;
        let stack = self
            .interp
            .execute_method(
                "vectorwing/farmersdelight/refabricated/inventory/ItemStackHandler",
                "getStackInSlot",
                "(I)Lnet/minecraft/world/item/ItemStack;",
                &[handler, Value::Int(slot)],
            )
            .ok()?;
        let stack = self
            .interp
            .heap
            .get_native::<crate::minecraft::ItemStack>(stack.as_obj()?)?;
        Some((stack.item.id.clone(), stack.count))
    }

    /// A kept block entity's own inventory object (`getInventory()`), asked
    /// through the entity's real class, for tests.
    fn debug_block_entity_handler(&mut self, pos: [i32; 3], block: &str) -> Option<Value> {
        let entity = *self
            .interp
            .heap
            .kept_block_entities()
            .get(&(pos, block.to_string()))?;
        let class = match self.interp.heap.get(entity) {
            Some(JavaObject::Instance { class_name, .. }) => class_name.clone(),
            _ => return None,
        };
        self.interp
            .execute_method(
                &class,
                "getInventory",
                "()Lvectorwing/farmersdelight/refabricated/inventory/ItemStackHandler;",
                &[Value::Object(Some(entity))],
            )
            .ok()
            .filter(|value| !value.is_null())
    }

    /// Write one of a kept block entity's own fields, for tests.
    #[doc(hidden)]
    pub fn debug_set_block_entity_field(
        &mut self,
        pos: [i32; 3],
        block: &str,
        field: &str,
        value: i32,
    ) {
        let Some(id) = self
            .interp
            .heap
            .block_entities
            .get(&(pos, block.to_string()))
            .copied()
        else {
            return;
        };
        if let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get_mut(id) {
            fields.insert(field.to_string(), Value::Int(value));
        }
    }

    /// A field of a kept block entity, for tests that check what a tick did
    /// to the mod's own state (`cookTime`, `cookTimeTotal`).
    #[doc(hidden)]
    pub fn debug_block_entity_field(
        &mut self,
        pos: [i32; 3],
        block: &str,
        field: &str,
    ) -> Option<i32> {
        let id = *self
            .interp
            .heap
            .block_entities
            .get(&(pos, block.to_string()))?;
        match self.interp.heap.get(id) {
            Some(JavaObject::Instance { fields, .. }) => fields.get(field).map(Value::as_int),
            _ => None,
        }
    }

    /// The `ItemStack` in a kept block entity's own container slot, for tests.
    #[doc(hidden)]
    pub fn debug_block_entity_item(
        &mut self,
        pos: [i32; 3],
        block: &str,
        slot: i32,
    ) -> Option<(String, i32)> {
        let id = *self
            .interp
            .heap
            .block_entities
            .get(&(pos, block.to_string()))?;
        let stack = self
            .interp
            .execute_method(
                "net/minecraft/world/Container",
                "getItem",
                "(I)Lnet/minecraft/world/item/ItemStack;",
                &[Value::Object(Some(id)), Value::Int(slot)],
            )
            .ok()?;
        let stack = self
            .interp
            .heap
            .get_native::<crate::minecraft::ItemStack>(stack.as_obj()?)?;
        Some((stack.item.id.clone(), stack.count))
    }

    /// Put an `ItemStack` in a kept block entity's own container slot, for
    /// tests.
    #[doc(hidden)]
    pub fn debug_set_block_entity_item(
        &mut self,
        pos: [i32; 3],
        block: &str,
        slot: i32,
        item: &str,
        count: i32,
    ) {
        let Some(id) = self
            .interp
            .heap
            .block_entities
            .get(&(pos, block.to_string()))
            .copied()
        else {
            return;
        };
        let stack = self
            .interp
            .heap
            .alloc_item_stack(crate::minecraft::ItemStack::new(item, count));
        let _ = self.interp.execute_method(
            "net/minecraft/world/Container",
            "setItem",
            "(ILnet/minecraft/world/item/ItemStack;)V",
            &[
                Value::Object(Some(id)),
                Value::Int(slot),
                Value::Object(Some(stack)),
            ],
        );
    }

    pub fn debug_state_object(&mut self, id: &str, key: &str) -> Option<Value> {
        self.interp.mod_state_objects(id).ok()?.get(key).cloned()
    }
}

/// The host's blocks within the placement radius of `pos`.
fn world_around(
    host: &dyn super::HostBridge,
    pos: [i32; 3],
) -> crate::place_builtins::WorldSnapshot {
    let mut blocks = Vec::new();
    let r = crate::place_builtins::WORLD_RADIUS;
    for dx in -r..=r {
        for dy in -r..=r {
            for dz in -r..=r {
                let at = [pos[0] + dx, pos[1] + dy, pos[2] + dz];
                let id = host.block(at);
                if id.is_empty() || id == "minecraft:air" {
                    continue;
                }
                blocks.push((at, id, host.block_properties(at)));
            }
        }
    }
    crate::place_builtins::WorldSnapshot { blocks }
}

/// A block a mod's placement code set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockChange {
    pub pos: [i32; 3],
    /// `namespace:path`.
    pub block: String,
    /// The state, `property -> value`.
    pub properties: BTreeMap<String, String>,
}

/// What a placed block's own `onPlace` and `setPlacedBy` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacedBy {
    /// The block overrides neither (or is not a mod's block).
    NotOverridden,
    /// The blocks the code set, in order, the placed block's own position
    /// left out.
    Changes(Vec<BlockChange>),
    Failed(String),
}

impl super::FabricBridge {
    /// Run `onPlace` and `setPlacedBy` of the registered block
    /// `request.block`, now standing in the state `placed` at `request.pos`,
    /// over the host's blocks around it, and report the other blocks they set
    /// (a bed's foot, a piece of furniture's extra cells). The caller applies
    /// them through the host.
    pub fn block_placed(
        &mut self,
        host: &dyn super::HostBridge,
        request: &super::PlacementRequest,
        placed: &BTreeMap<String, String>,
    ) -> PlacedBy {
        let Some(fact) = self
            .interp
            .heap
            .block_facts
            .iter()
            .find(|b| b.id == request.block)
        else {
            return PlacedBy::NotOverridden;
        };
        let on_place = fact.overrides.iter().any(|o| o.ends_with(".onPlace"));
        let set_placed_by = fact.overrides.iter().any(|o| o.ends_with(".setPlacedBy"));
        if !on_place && !set_placed_by {
            return PlacedBy::NotOverridden;
        }
        let key = placed
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",");
        let started = std::time::Instant::now();
        let world = world_around(host, request.pos);
        let previous = self.interp.step_budget;
        self.interp.step_budget = Some(crate::place_builtins::PLACEMENT_BUDGET);
        let result = self
            .interp
            .placed_by(request, &world, &key, on_place, set_placed_by);
        let used = crate::place_builtins::PLACEMENT_BUDGET - self.interp.step_budget.unwrap_or(0);
        self.interp.step_budget = previous;
        let took = started.elapsed();
        if took.as_millis() > 50 {
            log::warn!(
                "setPlacedBy of {} took {took:?} ({used} instructions)",
                request.block
            );
        }
        match result {
            Ok(changes) => PlacedBy::Changes(
                changes
                    .into_iter()
                    .filter(|(at, ..)| *at != request.pos)
                    .map(|(pos, block, key)| BlockChange {
                        pos,
                        block,
                        properties: crate::place_builtins::parse_state_key(&key),
                    })
                    .collect(),
            ),
            Err(error) => PlacedBy::Failed(error.to_string()),
        }
    }
}

/// A block tick a mod's code scheduled (`Level.scheduleTick`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSchedule {
    pub pos: [i32; 3],
    /// The registered block whose tick is due, `namespace:path`.
    pub block: String,
    /// In game ticks.
    pub delay: u32,
}

/// A particle burst a mod's code asked the level to send
/// (`ServerLevel.sendParticles`).
#[derive(Debug, Clone, PartialEq)]
pub struct ParticleBurst {
    /// `minecraft:falling_water`.
    pub id: String,
    pub at: [f64; 3],
    pub count: u32,
    /// The random spread of each particle from `at`.
    pub spread: [f64; 3],
    pub speed: f64,
}

/// An item a mod's block code dropped (`Level.popResource`, `dropStacks`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemDrop {
    pub pos: [i32; 3],
    pub item: String,
    pub count: u32,
}

/// A block a mod's code removed (`Level.removeBlock`, `destroyBlock`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRemoval {
    pub pos: [i32; 3],
    /// `destroyBlock`'s `dropBlock`: the block's loot drops when true.
    pub drop: bool,
}

/// What a mod's block code changed, for the host to apply.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlockEffects {
    /// The states the code set, in first-write order.
    pub changes: Vec<BlockChange>,
    /// The block ticks it scheduled.
    pub schedules: Vec<BlockSchedule>,
    /// The particles it sent.
    pub particles: Vec<ParticleBurst>,
    /// The items it dropped.
    pub drops: Vec<ItemDrop>,
    /// The blocks it removed.
    pub removals: Vec<BlockRemoval>,
    /// The menu its code opened (`Player.openMenu`): the slots and title the
    /// host should show, read from the provider's own `createMenu`.
    pub menu: Option<OpenedMenu>,
}

/// A menu a mod's block entity (or another `MenuProvider`) opened, as its own
/// `createMenu` built it.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenedMenu {
    /// The title `getDisplayName` answered, as plain text.
    pub title: String,
    /// The menu's slot count (`AbstractContainerMenu.slots.size()`).
    pub slots: usize,
    /// What each slot holds, by index.
    pub items: Vec<Option<(String, i32)>>,
    /// The slots a click may put a stack into, from the menu's own
    /// `mayPlace`/`canPlaceItem` answers where they are readable.
    pub input: Vec<usize>,
}

/// What using a registered block (right-click) did.
#[derive(Debug, Clone, PartialEq)]
pub enum BlockUse {
    /// The block overrides neither use method (or is not a mod's block): the
    /// host handles the click as it would anyway.
    NotOverridden,
    /// The block's code passed the click on (`InteractionResult.PASS`).
    Passed,
    /// The block consumed the click: what its code changed, scheduled and
    /// sent, for the host to apply.
    Handled(BlockEffects),
    /// The block's code could not be run to an answer; nothing was invented.
    Failed(String),
}

impl super::FabricBridge {
    /// Run the registered block's own `useItemOn`/`useWithoutItem` for the
    /// click `event`, over the host's blocks around it, at the state the host
    /// has the block in. A block whose block entity is a container keeps the
    /// host's container path for an empty or non-acting hand (its
    /// `useWithoutItem` is usually just `openMenu`, which the host does
    /// itself); an overridden `useItemOn` still runs, so a container that
    /// reacts to an item works.
    pub fn block_used(
        &mut self,
        host: &dyn super::HostBridge,
        event: &super::RightClickBlockEvent,
    ) -> BlockUse {
        let block_id = host.block(event.pos);
        let Some(fact) = self
            .interp
            .heap
            .block_facts
            .iter()
            .find(|b| b.id == block_id)
        else {
            return BlockUse::NotOverridden;
        };
        let (main_item, off_item) = (host.held_item(), host.off_hand_item());
        // `ServerPlayerGameMode.useItemOn`: sneaking with something in either
        // hand keeps the block's own use from running, so the item's use (a
        // block placed against it) can.
        if use_suppressed(event.sneaking, main_item.as_deref(), off_item.as_deref()) {
            return BlockUse::NotOverridden;
        }
        let use_item = fact.overrides.iter().any(|o| o.ends_with(".useItemOn"));
        let mut use_without_item = fact
            .overrides
            .iter()
            .any(|o| o.ends_with(".useWithoutItem"));
        if !use_item && !use_without_item {
            return BlockUse::NotOverridden;
        }
        if matches!(fact.container, ContainerRead::Container(_)) {
            use_without_item = false;
            if !use_item {
                return BlockUse::NotOverridden;
            }
        }
        let request = super::PlacementRequest {
            block: block_id.clone(),
            pos: event.pos,
            hit_pos: event.pos,
            face: event.face,
            hit: event.hit,
            inside: false,
            yaw: 0.0,
            pitch: 0.0,
            player_pos: [0.0; 3],
            sneaking: event.sneaking,
            off_hand: event.off_hand,
            item: if event.off_hand { off_item } else { main_item }.unwrap_or_default(),
            replace_clicked: false,
        };
        let started = std::time::Instant::now();
        let world = world_around(host, event.pos);
        let previous = self.interp.step_budget;
        self.interp.step_budget = Some(crate::place_builtins::PLACEMENT_BUDGET);
        let result = self
            .interp
            .block_used(&request, &world, use_item, use_without_item);
        let used = crate::place_builtins::PLACEMENT_BUDGET - self.interp.step_budget.unwrap_or(0);
        self.interp.step_budget = previous;
        let took = started.elapsed();
        if took.as_millis() > 50 {
            log::warn!(
                "use of {} took {took:?} ({used} instructions)",
                request.block
            );
        }
        match result {
            Ok(None) => BlockUse::Passed,
            Ok(Some(level)) => match self.effects_of(level) {
                Ok(effects) => {
                    // Do not consume a click whose removal the host cannot
                    // carry out: its own path stays in charge.
                    if !effects.removals.is_empty() && !host.can_remove_blocks() {
                        BlockUse::Failed(
                            "block code removed a block, which this host cannot do".into(),
                        )
                    } else {
                        BlockUse::Handled(effects)
                    }
                }
                Err(error) => BlockUse::Failed(error.to_string()),
            },
            Err(error) => BlockUse::Failed(error.to_string()),
        }
    }

    /// The click location a registered block's `BlockHitResult` answers, read
    /// through the real `HitResult.getLocation()`; `None` when the runtime
    /// could not build it. Tests use it; nothing in the host calls it.
    #[doc(hidden)]
    pub fn debug_hit_location(
        &mut self,
        request: &super::PlacementRequest,
        host: &dyn super::HostBridge,
    ) -> Option<[f64; 3]> {
        let world = world_around(host, request.pos);
        let prepared = self.interp.prepare_placement(request, &world).ok()?;
        let value = self
            .interp
            .execute_method(
                "net/minecraft/world/phys/HitResult",
                "getLocation",
                "()Lnet/minecraft/world/phys/Vec3;",
                &[Value::Object(Some(prepared.hit_result))],
            )
            .ok()?;
        let id = value.as_obj()?;
        let field = |name: &str| match self.interp.heap.get(id)? {
            JavaObject::Instance { fields, .. } => fields.get(name).map(Value::as_double),
            _ => None,
        };
        Some([field("x")?, field("y")?, field("z")?])
    }

    /// Run the registered block's own `tick` at `pos` (`Level.scheduleTick`
    /// made it due), over the host's blocks around it, at the state the host
    /// has it in. `None` when the block overrides no tick or is gone.
    pub fn tick_block(
        &mut self,
        host: &dyn super::HostBridge,
        pos: [i32; 3],
        block_id: &str,
    ) -> Option<BlockEffects> {
        let fact = self
            .interp
            .heap
            .block_facts
            .iter()
            .find(|b| b.id == block_id)?;
        if !fact.overrides.iter().any(|o| o.ends_with(".tick")) {
            return None;
        }
        let started = std::time::Instant::now();
        let world = world_around(host, pos);
        // Warm the state-object cache the snapshot will use: it holds heap ids
        // across calls, so it must not be filled inside the scope.
        for (_, id, _) in &world.blocks {
            if self
                .interp
                .heap
                .registered_blocks
                .iter()
                .any(|block| &block.id == id)
            {
                let _ = self.interp.mod_state_objects(id);
            }
        }
        self.interp.heap.begin_scope();
        let previous = self.interp.step_budget;
        self.interp.step_budget = Some(crate::place_builtins::TICK_BUDGET);
        let result = self.interp.block_tick(block_id, pos, &world);
        let used = crate::place_builtins::TICK_BUDGET - self.interp.step_budget.unwrap_or(0);
        self.interp.step_budget = previous;
        let took = started.elapsed();
        if took.as_millis() > 20 {
            log::warn!("tick of {block_id} took {took:?} ({used} instructions)");
        }
        let effects = match result {
            Ok(level) => self.effects_of(level).ok(),
            Err(error) => {
                log::warn!("Fabric: {block_id}'s tick failed: {error}");
                None
            }
        };
        // The snapshot and whatever the tick allocated are freed unless a
        // scoped object escaped into a static, a cache or an older object.
        let mark = self.interp.heap.scope_mark().unwrap_or(0);
        let kept = self.interp.heap.scope_escaped()
            || self.interp.heap.refs_into(mark)
            || self.interp.static_refs_into(mark)
            || self.bridge_refs_into(mark);
        if !self.interp.heap.end_scope(kept) && self.scope_kept_logged.insert(block_id.to_string())
        {
            log::debug!(
                "block {block_id}'s tick kept heap objects (a class initializer, a static or a cache); its scope was not released"
            );
        }
        effects
    }

    /// Whether a heap id the bridge keeps between calls is in the open scope.
    fn bridge_refs_into(&self, mark: usize) -> bool {
        let id = |id: Option<usize>| id.is_some_and(|id| id >= mark);
        id(self.screen_id)
            || id(self.menu_screen)
            || self
                .hovered_tooltip
                .as_ref()
                .is_some_and(|(_, stack, screen)| *stack >= mark || id(*screen))
    }

    /// The number of heap slots, for the tick-scope test.
    #[doc(hidden)]
    pub fn heap_len(&self) -> usize {
        self.interp.heap.object_count()
    }

    /// How many objects the last [`crate::heap::Heap::refs_into`] examined: the
    /// objects written since the current scope opened, not the whole heap.
    #[doc(hidden)]
    pub fn scope_scan_len(&self) -> usize {
        self.interp.heap.refs_scanned()
    }

    /// The changes, block ticks and particles a level a mod's code wrote is
    /// holding, drained.
    fn effects_of(&mut self, level: usize) -> hfc_core::Result<BlockEffects> {
        let (particles, scheduled, dropped, broken, menus) =
            match self.interp.heap.get_native_mut::<Level>(level) {
                Some(level) => (
                    std::mem::take(&mut level.particles),
                    std::mem::take(&mut level.scheduled),
                    std::mem::take(&mut level.dropped_stacks),
                    std::mem::take(&mut level.broken),
                    std::mem::take(&mut level.menus),
                ),
                None => (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()),
            };
        let schedules = scheduled
            .into_iter()
            .filter_map(|(pos, object, delay)| {
                let block = self
                    .interp
                    .heap
                    .registered_blocks
                    .iter()
                    .find(|b| b.object == object)?
                    .id
                    .clone();
                Some(BlockSchedule {
                    pos,
                    block,
                    delay: delay.max(1) as u32,
                })
            })
            .collect();
        let particles = particles
            .into_iter()
            .filter(|call| call.count > 0)
            .map(|call| ParticleBurst {
                id: call.id,
                at: call.at,
                count: call.count as u32,
                spread: call.spread,
                speed: call.speed,
            })
            .collect();
        let changes: Vec<BlockChange> = self
            .interp
            .level_writes(level)?
            .into_iter()
            .map(|(pos, block, key)| BlockChange {
                pos,
                block,
                properties: crate::place_builtins::parse_state_key(&key),
            })
            .collect();
        let removals: Vec<BlockRemoval> = broken
            .into_iter()
            .map(|(pos, drop)| BlockRemoval {
                pos: [pos.x, pos.y, pos.z],
                drop,
            })
            .collect();
        // `level_writes` keeps the last write per position, and a removal has
        // no sequence beside it: if both touched a position, the order is not
        // known, so nothing is applied rather than guessing.
        if let Some(change) = changes
            .iter()
            .find(|change| removals.iter().any(|removal| removal.pos == change.pos))
        {
            return Err(hfc_core::Error::new(
                "interpreter",
                format!("block code both set and removed {:?}", change.pos),
            ));
        }
        let drops = dropped
            .into_iter()
            .filter(|(_, item, count)| *count > 0 && !item.is_empty() && item != "minecraft:air")
            .map(|(pos, item, count)| ItemDrop {
                pos: [pos.x, pos.y, pos.z],
                item,
                count: count as u32,
            })
            .collect();
        // A menu the code opened (`Player.openMenu`): ask its provider for the
        // menu itself, so the slots and title are the mod's own.
        let menu = menus.last().copied().and_then(|provider| {
            let _ = level;
            self.interp.menu_of_provider(provider)
        });
        Ok(BlockEffects {
            changes,
            schedules,
            particles,
            drops,
            removals,
            menu,
        })
    }

    /// Apply the effects of a mod block's code through the host: property
    /// writes (a write that replaces the block with another is reported), the
    /// block ticks it scheduled, and the particles it sent.
    /// The menu a block's use opened, and the block it is on, taken once: the
    /// host shows it after the use's other effects are applied.
    pub fn take_opened_menu(&mut self) -> Option<(String, OpenedMenu)> {
        let block = self.pending_menu_block.take()?;
        let menu = self.pending_menu.take()?;
        Some((block, menu))
    }

    /// Remember the menu a use opened, for [`FabricBridge::take_opened_menu`].
    pub(super) fn remember_opened_menu(&mut self, block: &str, menu: &BlockEffects) {
        if let Some(opened) = &menu.menu {
            self.pending_menu = Some(opened.clone());
            self.pending_menu_block = Some(block.to_string());
        }
    }

    pub fn apply_block_effects(&self, host: &mut dyn super::HostBridge, effects: &BlockEffects) {
        for change in &effects.changes {
            if host.block(change.pos) != change.block {
                log::warn!(
                    "Fabric: block code set {:?} to {}, which a click cannot do here",
                    change.pos,
                    change.block
                );
                continue;
            }
            for (key, value) in &change.properties {
                // The state the mod wrote carries every property, not only
                // the ones it changed.
                if host.block_property(change.pos, key).as_deref() == Some(value.as_str()) {
                    continue;
                }
                if !host.set_block_property(change.pos, key, value) {
                    log::warn!(
                        "Fabric: block code set {:?}.{key} to {value}, which the host refused",
                        change.pos
                    );
                }
            }
        }
        for removal in &effects.removals {
            if !host.remove_block(removal.pos, removal.drop) {
                log::warn!(
                    "Fabric: block code removed {:?}, which the host refused",
                    removal.pos
                );
            }
        }
        for drop in &effects.drops {
            host.drop_item(drop.pos, &drop.item, drop.count);
        }
        for schedule in &effects.schedules {
            host.schedule_block_tick(schedule.pos, &schedule.block, schedule.delay);
        }
        for particle in &effects.particles {
            host.particle(
                &particle.id,
                particle.at,
                particle.count,
                particle.spread,
                particle.speed,
            );
        }
    }
}

/// `ServerPlayerGameMode.useItemOn`'s suppression: with an item in either
/// hand, sneaking skips the block's own use so the item's use (placing a
/// block, eating) runs instead.
fn use_suppressed(sneaking: bool, main: Option<&str>, off: Option<&str>) -> bool {
    fn holds(item: Option<&str>) -> bool {
        item.is_some_and(|id| !id.is_empty() && id != "minecraft:air")
    }
    sneaking && (holds(main) || holds(off))
}

#[cfg(test)]
mod tests {
    use super::{ItemDrop, use_suppressed};
    use crate::minecraft::{BlockPos, Level};

    #[test]
    fn sneaking_with_an_item_in_either_hand_suppresses_the_blocks_use() {
        assert!(use_suppressed(true, Some("minecraft:stone"), None));
        assert!(use_suppressed(true, None, Some("minecraft:stone")));
        assert!(use_suppressed(
            true,
            Some("minecraft:stone"),
            Some("minecraft:torch")
        ));
        // An empty hand is `None`, an empty id or air.
        assert!(!use_suppressed(true, None, None));
        assert!(!use_suppressed(true, Some(""), None));
        assert!(!use_suppressed(true, Some("minecraft:air"), None));
        assert!(!use_suppressed(true, None, Some("minecraft:air")));
        // Not sneaking never suppresses.
        assert!(!use_suppressed(false, Some("minecraft:stone"), None));
    }

    /// `popResource`/`dropStacks` record into the level; the bridge must drain
    /// them for the host, and skip what no stack can hold.
    #[test]
    fn the_drops_a_level_holds_are_drained_for_the_host() {
        let mut bridge = super::super::FabricBridge::new();
        let mut level = Level::new(false);
        let at = |y| BlockPos { x: 1, y, z: 3 };
        level
            .dropped_stacks
            .push((at(2), "minecraft:wheat".into(), 2));
        // Air and an empty count are not items a host can give.
        level
            .dropped_stacks
            .push((at(1), "minecraft:air".into(), 3));
        level
            .dropped_stacks
            .push((at(3), "minecraft:wheat_seeds".into(), 0));
        let level_id = bridge.interp.heap.alloc_level(level);
        let effects = bridge.effects_of(level_id).unwrap();
        assert_eq!(
            effects.drops,
            [ItemDrop {
                pos: [1, 2, 3],
                item: "minecraft:wheat".into(),
                count: 2,
            }]
        );
        // Drained, not left for the next call to apply twice.
        assert!(bridge.effects_of(level_id).unwrap().drops.is_empty());
    }

    /// The level holds a mod's own state for a position; a removal must find
    /// it, not only the native block map.
    #[test]
    fn removing_a_mod_state_leaves_air_and_records_the_removal() {
        let mut level = Level::new(false);
        let pos = BlockPos { x: 4, y: 1, z: 4 };
        level.mod_states.insert([4, 1, 4], 1234);
        assert!(level.destroy_block(&pos, false));
        assert_eq!(level.get_block_state(&pos).block.id, "minecraft:air");
        assert!(level.mod_states.get(&[4, 1, 4]).is_none());
        assert_eq!(level.broken, [(pos.clone(), false)]);
        // Nothing is there now: no removal is recorded.
        assert!(!level.destroy_block(&pos, true));
        assert_eq!(level.broken.len(), 1);
    }
}

/// A block entity as the host keeps it: the block it belongs to, its position,
/// and the mod's own object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockEntityKeeper {
    pub block: String,
    pub pos: [i32; 3],
}

impl super::FabricBridge {
    /// Make the block entity of the registered block at `pos` if there is none
    /// yet, and answer whether the block has one. The entity is the mod's own
    /// (`EntityBlock.newBlockEntity`), kept for the position: its fields, its
    /// container and its ticker are its own code's.
    pub fn ensure_block_entity(
        &mut self,
        host: &dyn super::HostBridge,
        pos: [i32; 3],
        block_id: &str,
    ) -> bool {
        let Some(fact) = self
            .interp
            .heap
            .block_facts
            .iter()
            .find(|b| b.id == block_id)
        else {
            return false;
        };
        if !fact.has_block_entity {
            return false;
        }
        if self
            .interp
            .heap
            .kept_block_entities()
            .contains_key(&(pos, block_id.to_string()))
        {
            return true;
        }
        let world = world_around(host, pos);
        let properties = host.block_properties(pos);
        let key = crate::place_builtins::state_key_of(&properties);
        let block = Value::Object(
            self.interp
                .heap
                .registered_blocks
                .iter()
                .find(|b| b.id == block_id)
                .map(|b| b.object),
        );
        let previous = self.interp.step_budget;
        self.interp.step_budget = Some(crate::place_builtins::PLACEMENT_BUDGET);
        let level = self.interp.snapshot_level(&world).ok();
        let state = self
            .interp
            .mod_state_objects(block_id)
            .ok()
            .and_then(|states| {
                states
                    .get(&key)
                    .cloned()
                    .or_else(|| states.values().next().cloned())
            });
        let made = match (level, state) {
            (Some(level), Some(state)) => self
                .interp
                .block_entity_at(&block, block_id, pos, &state, level)
                .ok()
                .flatten()
                .is_some(),
            _ => false,
        };
        self.interp.step_budget = previous;
        if !made
            && self
                .interp
                .heap
                .block_entity_failures
                .insert(block_id.to_string())
        {
            log::warn!("Fabric: {block_id}'s newBlockEntity made no block entity");
        }
        made
    }

    /// Whether the registered block `id` makes a block entity (`EntityBlock`).
    pub fn block_entity_declared(&self, id: &str) -> bool {
        self.interp
            .heap
            .block_facts
            .iter()
            .find(|b| b.id == id)
            .is_some_and(|b| b.has_block_entity)
    }

    /// The position and block of every block entity the runtime keeps.
    pub fn kept_block_entity_positions(&self) -> Vec<([i32; 3], String)> {
        self.interp
            .heap
            .kept_block_entities()
            .keys()
            .map(|(pos, block)| (*pos, block.clone()))
            .collect()
    }

    /// How many block entities the runtime keeps for Fabric blocks.
    pub fn kept_block_entity_count(&self) -> usize {
        self.interp.heap.kept_block_entities().len()
    }

    /// The state of a kept block entity, saved through its own
    /// `saveAdditional` into a `ValueOutput` the runtime records: the tree the
    /// host can store in the block entity's NBT and hand back through
    /// [`FabricBridge::load_block_entity`].
    pub fn save_block_entity(
        &mut self,
        pos: [i32; 3],
        block_id: &str,
    ) -> Option<Vec<(String, crate::value_io_builtins::StoredValue)>> {
        let entity = *self
            .interp
            .heap
            .kept_block_entities()
            .get(&(pos, block_id.to_string()))?;
        let output = crate::value_io_builtins::alloc_output(&mut self.interp.heap);
        self.interp
            .execute_method(
                "net/minecraft/world/level/block/entity/BlockEntity",
                "saveAdditional",
                "(Lnet/minecraft/world/level/storage/ValueOutput;)V",
                &[Value::Object(Some(entity)), Value::Object(Some(output))],
            )
            .ok()?;
        Some(crate::value_io_builtins::flat_values(
            &self.interp.heap,
            output,
        ))
    }

    /// Make the block entity at `pos` if there is none, then load `values` into
    /// it through its own `loadAdditional`.
    pub fn load_block_entity(
        &mut self,
        host: &dyn super::HostBridge,
        pos: [i32; 3],
        block_id: &str,
        values: Vec<(String, crate::value_io_builtins::StoredValue)>,
    ) -> bool {
        if !self.ensure_block_entity(host, pos, block_id) {
            return false;
        }
        let Some(entity) = self
            .interp
            .heap
            .kept_block_entities()
            .get(&(pos, block_id.to_string()))
            .copied()
        else {
            return false;
        };
        let input = crate::value_io_builtins::input_from_flat(&mut self.interp.heap, &values);
        self.interp
            .execute_method(
                "net/minecraft/world/level/block/entity/BlockEntity",
                "loadAdditional",
                "(Lnet/minecraft/world/level/storage/ValueInput;)V",
                &[Value::Object(Some(entity)), Value::Object(Some(input))],
            )
            .is_ok()
    }

    /// Make the mod's own inventory match `items` (the stacks the host's menu
    /// shows), one slot at a time, so the block's code reads what the player
    /// put in. Slots the menu does not show are left as they are.
    pub fn sync_menu_items(
        &mut self,
        pos: [i32; 3],
        block_id: &str,
        items: &[Option<(String, i32)>],
    ) -> usize {
        let mut written = 0;
        for (slot, stack) in items.iter().enumerate() {
            if self.set_menu_slot(
                pos,
                block_id,
                slot,
                stack.as_ref().map(|(i, c)| (i.as_str(), *c)),
            ) {
                written += 1;
            }
        }
        written
    }

    /// The slots a mod block's menu shows and what they hold, read from the
    /// block entity the container's `slots` come from: the host keeps the
    /// shown stacks in step with the mod's own inventory, so a cooking tick
    /// reads what the player put in.
    pub fn menu_slot(
        &mut self,
        pos: [i32; 3],
        block_id: &str,
        slot: usize,
    ) -> Option<(String, i32)> {
        let entity = *self
            .interp
            .heap
            .kept_block_entities()
            .get(&(pos, block_id.to_string()))?;
        let class = match self.interp.heap.get(entity) {
            Some(JavaObject::Instance { class_name, .. }) => class_name.clone(),
            _ => return None,
        };
        // The menu's slots read a `Container` the entity answers; a mod's own
        // inventory (`ItemStackHandler`) is read through the entity's own
        // accessor, which `read_mod_container` asked when the host declared it.
        let container = self
            .interp
            .execute_method(
                &class,
                "getInventory",
                "()Lvectorwing/farmersdelight/refabricated/inventory/ItemStackHandler;",
                &[Value::Object(Some(entity))],
            )
            .ok()
            .filter(|value| !value.is_null());
        let stack = match container {
            Some(handler) => self.interp.execute_method(
                "vectorwing/farmersdelight/refabricated/inventory/ItemStackHandler",
                "getStackInSlot",
                "(I)Lnet/minecraft/world/item/ItemStack;",
                &[handler, Value::Int(slot as i32)],
            ),
            None => self.interp.execute_method(
                "net/minecraft/world/Container",
                "getItem",
                "(I)Lnet/minecraft/world/item/ItemStack;",
                &[Value::Object(Some(entity)), Value::Int(slot as i32)],
            ),
        }
        .ok()?;
        let stack = self
            .interp
            .heap
            .get_native::<crate::minecraft::ItemStack>(stack.as_obj()?)?;
        (stack.count > 0 && stack.item.id != "minecraft:air")
            .then(|| (stack.item.id.clone(), stack.count))
    }

    /// Set the stack in one of a mod block's menu slots, through the same
    /// `Container`/inventory the block's own code reads.
    pub fn set_menu_slot(
        &mut self,
        pos: [i32; 3],
        block_id: &str,
        slot: usize,
        stack: Option<(&str, i32)>,
    ) -> bool {
        let Some(entity) = self
            .interp
            .heap
            .kept_block_entities()
            .get(&(pos, block_id.to_string()))
            .copied()
        else {
            return false;
        };
        let class = match self.interp.heap.get(entity) {
            Some(JavaObject::Instance { class_name, .. }) => class_name.clone(),
            _ => return false,
        };
        let stack = stack
            .map(|(item, count)| {
                Value::Object(Some(
                    self.interp
                        .heap
                        .alloc_item_stack(crate::minecraft::ItemStack::new(item, count)),
                ))
            })
            .unwrap_or(Value::NULL);
        let handler = self
            .interp
            .execute_method(
                &class,
                "getInventory",
                "()Lvectorwing/farmersdelight/refabricated/inventory/ItemStackHandler;",
                &[Value::Object(Some(entity))],
            )
            .ok()
            .filter(|value| !value.is_null());
        let result = match handler {
            Some(handler) => self.interp.execute_method(
                "vectorwing/farmersdelight/refabricated/inventory/ItemStackHandler",
                "setStackInSlot",
                "(ILnet/minecraft/world/item/ItemStack;)V",
                &[handler, Value::Int(slot as i32), stack],
            ),
            None => self.interp.execute_method(
                "net/minecraft/world/Container",
                "setItem",
                "(ILnet/minecraft/world/item/ItemStack;)V",
                &[Value::Object(Some(entity)), Value::Int(slot as i32), stack],
            ),
        };
        result.is_ok()
    }

    /// Every kept block entity whose block declares a ticker: run its tick
    /// over the host's world. The host calls this once a game tick; what each
    /// tick changed is applied through the host. Answers how many ran.
    pub fn tick_block_entities(&mut self, host: &mut dyn super::HostBridge) -> usize {
        let kept: Vec<([i32; 3], String)> = self
            .interp
            .heap
            .kept_block_entities()
            .keys()
            .cloned()
            .collect();
        let mut ran = 0;
        for (pos, block) in kept {
            // A block that was broken or replaced no longer has its entity.
            if host.block(pos) != block {
                self.interp.heap.block_entities.remove(&(pos, block));
                continue;
            }
            if let Some(effects) = self.tick_block_entity(host, pos, &block) {
                self.apply_block_effects(host, &effects);
                ran += 1;
            }
        }
        ran
    }

    /// Run the block entity's own ticker at `pos` (the block's `getTicker`
    /// answered one), over the host's blocks around it, and report what the
    /// tick changed: the same effects a block tick's are.
    pub fn tick_block_entity(
        &mut self,
        host: &dyn super::HostBridge,
        pos: [i32; 3],
        block_id: &str,
    ) -> Option<BlockEffects> {
        let entity = *self
            .interp
            .heap
            .kept_block_entities()
            .get(&(pos, block_id.to_string()))?;
        let block = Value::Object(
            self.interp
                .heap
                .registered_blocks
                .iter()
                .find(|b| b.id == block_id)
                .map(|b| b.object),
        );
        let world = world_around(host, pos);
        let properties = host.block_properties(pos);
        let key = crate::place_builtins::state_key_of(&properties);
        let started = std::time::Instant::now();
        self.interp.heap.begin_scope();
        let previous = self.interp.step_budget;
        self.interp.step_budget = Some(crate::place_builtins::TICK_BUDGET);
        let result = (|| -> std::result::Result<usize, String> {
            let level = self
                .interp
                .snapshot_level(&world)
                .map_err(|e| e.to_string())?;
            let state = self
                .interp
                .mod_state_objects(block_id)
                .map_err(|e| e.to_string())?
                .get(&key)
                .cloned()
                .ok_or_else(|| format!("{block_id} has no state {key}"))?;
            let ticker = self
                .interp
                .block_entity_ticker(&block, level, &state, entity)
                .ok_or_else(|| format!("{block_id}'s getTicker answered none"))?;
            self.interp
                .tick_block_entity(&ticker, level, pos, &state, entity)
                .map_err(|e| e.to_string())?;
            Ok(level)
        })();
        let used = crate::place_builtins::TICK_BUDGET - self.interp.step_budget.unwrap_or(0);
        self.interp.step_budget = previous;
        let took = started.elapsed();
        if took.as_millis() > 20 {
            log::warn!("tick of {block_id}'s block entity took {took:?} ({used} instructions)");
        }
        let effects = match result {
            Ok(level) => self.effects_of(level).ok(),
            Err(error) => {
                if self
                    .interp
                    .heap
                    .block_entity_failures
                    .insert(block_id.to_string())
                {
                    log::warn!("Fabric: {block_id}'s block entity tick failed: {error}");
                }
                None
            }
        };
        // A block entity's tick runs on the object kept between calls, so its
        // scope must be kept whenever the entity itself was touched.
        let mark = self.interp.heap.scope_mark().unwrap_or(0);
        let kept = self.interp.heap.scope_escaped()
            || self.interp.heap.refs_into(mark)
            || self.interp.static_refs_into(mark)
            || entity >= mark;
        if !self.interp.heap.end_scope(kept) && self.scope_kept_logged.insert(block_id.to_string())
        {
            log::debug!(
                "block entity {block_id} kept heap objects (its fields hold them); its scope was not released"
            );
        }
        effects
    }
}
