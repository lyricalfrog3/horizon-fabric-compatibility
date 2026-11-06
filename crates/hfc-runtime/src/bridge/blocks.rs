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
        let mut use_item = fact.overrides.iter().any(|o| o.ends_with(".useItemOn"));
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
        // An empty hand has nothing for `useItemOn` to do.
        if use_item && (request.item.is_empty() || request.item == "minecraft:air") {
            use_item = false;
            if !use_without_item {
                return BlockUse::NotOverridden;
            }
        }
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
                Ok(effects) => BlockUse::Handled(effects),
                Err(error) => BlockUse::Failed(error.to_string()),
            },
            Err(error) => BlockUse::Failed(error.to_string()),
        }
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
        let previous = self.interp.step_budget;
        self.interp.step_budget = Some(crate::place_builtins::TICK_BUDGET);
        let result = self.interp.block_tick(block_id, pos, &world);
        let used = crate::place_builtins::TICK_BUDGET - self.interp.step_budget.unwrap_or(0);
        self.interp.step_budget = previous;
        let took = started.elapsed();
        if took.as_millis() > 20 {
            log::warn!("tick of {block_id} took {took:?} ({used} instructions)");
        }
        match result {
            Ok(level) => self.effects_of(level).ok(),
            Err(error) => {
                log::warn!("Fabric: {block_id}'s tick failed: {error}");
                None
            }
        }
    }

    /// The changes, block ticks and particles a level a mod's code wrote is
    /// holding, drained.
    fn effects_of(&mut self, level: usize) -> hfc_core::Result<BlockEffects> {
        let (particles, scheduled, dropped) = match self.interp.heap.get_native_mut::<Level>(level)
        {
            Some(level) => (
                std::mem::take(&mut level.particles),
                std::mem::take(&mut level.scheduled),
                std::mem::take(&mut level.dropped_stacks),
            ),
            None => (Vec::new(), Vec::new(), Vec::new()),
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
        let changes = self
            .interp
            .level_writes(level)?
            .into_iter()
            .map(|(pos, block, key)| BlockChange {
                pos,
                block,
                properties: crate::place_builtins::parse_state_key(&key),
            })
            .collect();
        let drops = dropped
            .into_iter()
            .filter(|(_, item, count)| *count > 0 && !item.is_empty() && item != "minecraft:air")
            .map(|(pos, item, count)| ItemDrop {
                pos: [pos.x, pos.y, pos.z],
                item,
                count: count as u32,
            })
            .collect();
        Ok(BlockEffects {
            changes,
            schedules,
            particles,
            drops,
        })
    }

    /// Apply the effects of a mod block's code through the host: property
    /// writes (a write that replaces the block with another is reported), the
    /// block ticks it scheduled, and the particles it sent.
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
}
