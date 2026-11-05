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

use std::collections::BTreeMap;

/// A block's `SoundType`, as the oracle writes it.
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
pub struct BlockContainer {
    /// `getContainerSize()`.
    pub slots: usize,
    /// The id of the vanilla `MenuType` of the menu `createMenu` builds
    /// (`minecraft:generic_9x3`).
    pub menu_type: String,
    /// The plain text of `getDisplayName()`: a literal, or a translation the
    /// mod's language files resolved (the key itself when they did not).
    pub title: Option<String>,
}

/// What reading a block's container found.
#[derive(Debug, Clone, PartialEq, Default)]
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
#[derive(Debug, Clone, PartialEq)]
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
