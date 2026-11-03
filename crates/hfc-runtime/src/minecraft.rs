//! Minecraft object models and types for the compatibility layer.

use crate::heap::Heap;
use crate::value::{NativeObject, Value};
use std::any::Any;
use std::collections::HashMap;

/// One recorded `GuiGraphics` operation. The runtime cannot issue GPU calls
/// itself, so a screen's extraction pass records what it would draw and the
/// host replays the list in its own draw batch.
#[derive(Debug, Clone, PartialEq)]
pub enum GuiDraw {
    Fill {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        color: i32,
    },
    Text {
        text: String,
        x: i32,
        y: i32,
        color: i32,
    },
    Blit {
        texture: String,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },
    Scissor {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
    },
    ScissorOff,
}

/// A `GuiGraphicsExtractor` display list, sized in GUI pixels.
#[derive(Debug, Clone)]
pub struct GuiGraphics {
    pub width: i32,
    pub height: i32,
    pub draws: Vec<GuiDraw>,
}

impl GuiGraphics {
    pub fn new(width: i32, height: i32) -> Self {
        Self {
            width,
            height,
            draws: Vec::new(),
        }
    }
}

impl NativeObject for GuiGraphics {
    fn class_name(&self) -> &str {
        "net/minecraft/client/gui/GuiGraphicsExtractor"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// A BlockPos (x, y, z).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl NativeObject for BlockPos {
    fn class_name(&self) -> &str {
        "net/minecraft/core/BlockPos"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// A Direction (DOWN, UP, NORTH, SOUTH, WEST, EAST).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Direction {
    pub name: &'static str,
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Direction {
    pub const DOWN: Self = Self {
        name: "DOWN",
        x: 0,
        y: -1,
        z: 0,
    };
    pub const UP: Self = Self {
        name: "UP",
        x: 0,
        y: 1,
        z: 0,
    };
    pub const NORTH: Self = Self {
        name: "NORTH",
        x: 0,
        y: 0,
        z: -1,
    };
    pub const SOUTH: Self = Self {
        name: "SOUTH",
        x: 0,
        y: 0,
        z: 1,
    };
    pub const WEST: Self = Self {
        name: "WEST",
        x: -1,
        y: 0,
        z: 0,
    };
    pub const EAST: Self = Self {
        name: "EAST",
        x: 1,
        y: 0,
        z: 0,
    };

    pub fn from_array(face: [i32; 3]) -> Self {
        match face {
            [0, -1, 0] => Self::DOWN,
            [0, 1, 0] => Self::UP,
            [0, 0, -1] => Self::NORTH,
            [0, 0, 1] => Self::SOUTH,
            [-1, 0, 0] => Self::WEST,
            [1, 0, 0] => Self::EAST,
            _ => Self::UP,
        }
    }
}

impl NativeObject for Direction {
    fn class_name(&self) -> &str {
        "net/minecraft/core/Direction"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// A BlockHitResult representing a raycast hit against a block.
#[derive(Debug, Clone)]
pub struct BlockHitResult {
    pub pos: BlockPos,
    pub direction: Direction,
}

impl NativeObject for BlockHitResult {
    fn class_name(&self) -> &str {
        "net/minecraft/world/phys/BlockHitResult"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// A Block representation.
#[derive(Debug, Clone)]
pub struct Block {
    pub id: String,
    pub is_crop: bool,
    pub max_age: i32,
}

impl NativeObject for Block {
    fn class_name(&self) -> &str {
        if self.is_crop {
            "net/minecraft/world/level/block/CropBlock"
        } else if self.id.contains("nether_wart") {
            "net/minecraft/world/level/block/NetherWartBlock"
        } else if self.id.contains("cocoa") {
            "net/minecraft/world/level/block/CocoaBlock"
        } else if self.id.contains("sugar_cane") {
            "net/minecraft/world/level/block/SugarCaneBlock"
        } else if self.id.contains("cactus") {
            "net/minecraft/world/level/block/CactusBlock"
        } else {
            "net/minecraft/world/level/block/Block"
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// A BlockState with properties (e.g. age = 7).
#[derive(Debug, Clone)]
pub struct BlockState {
    pub block: Block,
    pub properties: HashMap<String, String>,
}

impl BlockState {
    pub fn new(block_id: &str) -> Self {
        let is_crop = matches!(
            block_id,
            "minecraft:wheat"
                | "minecraft:carrots"
                | "minecraft:potatoes"
                | "minecraft:beetroots"
                | "minecraft:pitcher_crop"
                | "minecraft:torchflower_crop"
        );
        let max_age = match block_id {
            "minecraft:beetroots" | "minecraft:nether_wart" | "minecraft:cocoa" => 3,
            "minecraft:pitcher_crop" => 4,
            "minecraft:torchflower_crop" => 1,
            _ => 7,
        };
        let mut properties = HashMap::new();
        if is_crop || block_id.contains("nether_wart") || block_id.contains("cocoa") {
            properties.insert("age".into(), max_age.to_string());
        }
        Self {
            block: Block {
                id: block_id.to_string(),
                is_crop,
                max_age,
            },
            properties,
        }
    }

    pub fn with_property(mut self, key: &str, val: &str) -> Self {
        self.properties.insert(key.to_string(), val.to_string());
        self
    }

    pub fn get_age(&self) -> i32 {
        self.properties
            .get("age")
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(0)
    }

    pub fn is_max_age(&self) -> bool {
        self.get_age() >= self.block.max_age
    }
}

impl NativeObject for BlockState {
    fn class_name(&self) -> &str {
        "net/minecraft/world/level/block/state/BlockState"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// An Item representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: String,
}

impl NativeObject for Item {
    fn class_name(&self) -> &str {
        "net/minecraft/world/item/Item"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// An ItemStack in a hand or inventory.
#[derive(Debug, Clone)]
pub struct ItemStack {
    pub item: Item,
    pub count: i32,
    pub damage: i32,
}

impl ItemStack {
    pub fn new(id: impl Into<String>, count: i32) -> Self {
        Self {
            item: Item { id: id.into() },
            count,
            damage: 0,
        }
    }

    pub fn empty() -> Self {
        Self::new("minecraft:air", 0)
    }

    pub fn is_empty(&self) -> bool {
        self.count <= 0 || self.item.id == "minecraft:air"
    }

    pub fn is_hoe(&self) -> bool {
        self.item.id.ends_with("_hoe")
    }
}

impl NativeObject for ItemStack {
    fn class_name(&self) -> &str {
        "net/minecraft/world/item/ItemStack"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// A TagKey (e.g. ItemTags.HOES, BlockTags.CROPS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagKey {
    pub registry: String,
    pub id: String,
}

impl NativeObject for TagKey {
    fn class_name(&self) -> &str {
        "net/minecraft/tags/TagKey"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// An IntegerProperty for block state properties (e.g. AGE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegerProperty {
    pub name: String,
    pub min: i32,
    pub max: i32,
}

impl NativeObject for IntegerProperty {
    fn class_name(&self) -> &str {
        "net/minecraft/world/level/block/state/properties/IntegerProperty"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// A Level (or ServerLevel).
#[derive(Debug, Clone)]
pub struct Level {
    pub is_client: bool,
    pub dimension: String,
    pub blocks: HashMap<[i32; 3], BlockState>,
    pub dropped_stacks: Vec<(BlockPos, String, i32)>,
    pub replanted: Vec<(BlockPos, BlockState)>,
}

impl Level {
    pub fn new(is_client: bool) -> Self {
        Self {
            is_client,
            dimension: "minecraft:overworld".into(),
            blocks: HashMap::new(),
            dropped_stacks: Vec::new(),
            replanted: Vec::new(),
        }
    }

    pub fn get_block_state(&self, pos: &BlockPos) -> BlockState {
        self.blocks
            .get(&[pos.x, pos.y, pos.z])
            .cloned()
            .unwrap_or_else(|| BlockState::new("minecraft:air"))
    }

    pub fn set_block_and_update(&mut self, pos: &BlockPos, state: BlockState) -> bool {
        self.replanted.push((pos.clone(), state.clone()));
        self.blocks.insert([pos.x, pos.y, pos.z], state);
        true
    }
}

impl NativeObject for Level {
    fn class_name(&self) -> &str {
        if self.is_client {
            "net/minecraft/world/level/Level"
        } else {
            "net/minecraft/server/level/ServerLevel"
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// A Player interacting with the world.
#[derive(Debug, Clone)]
pub struct Player {
    pub is_crouching: bool,
    pub is_spectator: bool,
    pub level_id: usize,
    pub hand_item: Option<ItemStack>,
    pub experience_points: i32,
    pub food_exhaustion: f32,
}

impl Player {
    pub fn new(level_id: usize) -> Self {
        Self {
            is_crouching: false,
            is_spectator: false,
            level_id,
            hand_item: None,
            experience_points: 0,
            food_exhaustion: 0.0,
        }
    }
}

impl NativeObject for Player {
    fn class_name(&self) -> &str {
        "net/minecraft/world/entity/player/Player"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// EventResult from JamLib events.
#[derive(Debug, Clone)]
pub struct EventResult {
    pub cancelled: bool,
    pub value: Value,
}

impl EventResult {
    pub fn pass() -> Self {
        Self {
            cancelled: false,
            value: Value::NULL,
        }
    }

    pub fn cancel(value: Value) -> Self {
        Self {
            cancelled: true,
            value,
        }
    }
}

impl NativeObject for EventResult {
    fn class_name(&self) -> &str {
        "io/github/jamalam360/jamlib/api/events/core/EventResult"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Convenience helpers for allocating Minecraft objects on the heap.
impl Heap {
    pub fn alloc_block_pos(&mut self, x: i32, y: i32, z: i32) -> usize {
        self.alloc_native(BlockPos { x, y, z })
    }

    pub fn alloc_direction(&mut self, dir: Direction) -> usize {
        self.alloc_native(dir)
    }

    pub fn alloc_hit_result(&mut self, pos: BlockPos, dir: Direction) -> usize {
        self.alloc_native(BlockHitResult {
            pos,
            direction: dir,
        })
    }

    pub fn alloc_block_state(&mut self, state: BlockState) -> usize {
        self.alloc_native(state)
    }

    pub fn alloc_item_stack(&mut self, stack: ItemStack) -> usize {
        self.alloc_native(stack)
    }

    pub fn alloc_level(&mut self, level: Level) -> usize {
        self.alloc_native(level)
    }

    pub fn alloc_player(&mut self, player: Player) -> usize {
        self.alloc_native(player)
    }

    pub fn alloc_event_result(&mut self, res: EventResult) -> usize {
        self.alloc_native(res)
    }
}
