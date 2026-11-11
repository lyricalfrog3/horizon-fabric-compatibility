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
    /// A rectangle shading from `color_from` to `color_to`, as
    /// `GuiGraphicsExtractor.fillGradient`. When `horizontal` is false the
    /// shade runs top to bottom; when true, `color_from` is on the left and
    /// `color_to` on the right (the pose turned the vertical gradient).
    Gradient {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        color_from: i32,
        color_to: i32,
        horizontal: bool,
    },
    Text {
        text: String,
        x: i32,
        y: i32,
        color: i32,
    },
    /// An item model at a GUI position. `rotation` is degrees about the
    /// vertical axis; the host draws the model with its own item renderer.
    Item {
        item: String,
        x: i32,
        y: i32,
        size: i32,
        rotation: f32,
    },
    /// A texture rectangle. `u0..u1` and `v0..v1` are the part of the texture
    /// to sample, normalised to its size (`0..1` is the whole image), so a mod
    /// that draws a nine-slice frame from one texture keeps its corners and
    /// edges rather than stretching the sheet.
    Blit {
        texture: String,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        u0: f32,
        u1: f32,
        v0: f32,
        v1: f32,
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
    /// The `org/joml/Matrix3x2fStack` returned by `pose()`, created on first use.
    pub pose: Option<usize>,
}

/// A PNG a mod read through `NativeImage.read`. The runtime never decodes or
/// uploads it: the host decodes these bytes when it replays the blits of the
/// texture the mod registered, so a mod's own icon can be drawn for real.
#[derive(Debug, Clone)]
pub struct PngImage {
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

impl NativeObject for PngImage {
    fn class_name(&self) -> &str {
        "com/mojang/blaze3d/platform/NativeImage"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// A host menu a Fabric mixin may add widgets to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuKind {
    Title,
    Pause,
    Inventory,
    Container,
}

impl MenuKind {
    /// The Minecraft screen class whose layout a contribution pass runs.
    pub fn screen_class(self) -> &'static str {
        match self {
            MenuKind::Title => "net/minecraft/client/gui/screens/TitleScreen",
            MenuKind::Pause => "net/minecraft/client/gui/screens/PauseScreen",
            MenuKind::Inventory => "net/minecraft/client/gui/screens/inventory/InventoryScreen",
            MenuKind::Container => {
                "net/minecraft/client/gui/screens/inventory/AbstractContainerScreen"
            }
        }
    }

    /// The method the mixin adds widgets to, in the screen class.
    pub fn layout_method(self) -> &'static str {
        match self {
            MenuKind::Pause => "createPauseMenu",
            MenuKind::Title | MenuKind::Inventory | MenuKind::Container => "init",
        }
    }
}

/// A widget a mod's mixin added to a host menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuWidget {
    pub id: usize,
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub visible: bool,
    pub active: bool,
}

/// The callback object a Mixin injector receives. `cancel` ends the target
/// method early, as `CallbackInfo.cancel()` does in a real Mixin application.
///
/// A target method that returns `void` is handed a `CallbackInfo`; one that
/// returns a value is handed a `CallbackInfoReturnable`, which is the same
/// state plus the value the method answers with when the callback cancels.
/// `returnable` says which of the two classes this object presents as (Mixin
/// passes one or the other, and the callback's parameter type has to match).
#[derive(Debug, Clone)]
pub struct CallbackInfo {
    /// Whether the callback called `cancel()` (or `setReturnValue`, which
    /// cancels too, as Mixin's does).
    pub cancelled: bool,
    /// Whether the injection was declared `cancellable = true`. Mixin's
    /// `cancel()` throws `CancellationException` for one that was not, and a
    /// non-cancellable callback cannot end a non-cancellable injection.
    pub cancellable: bool,
    /// The injector's id, as `getId()` reports it (the target method name,
    /// unless the annotation named one).
    pub id: String,
    /// True for a `CallbackInfoReturnable`, false for a plain `CallbackInfo`.
    pub returnable: bool,
    /// The value `getReturnValue*` reports. At a `RETURN` injection point it
    /// starts as the value the target method is about to return; elsewhere it
    /// starts null (which the getters report as its type's zero).
    pub return_value: Value,
}

impl Default for CallbackInfo {
    fn default() -> Self {
        Self {
            cancelled: false,
            cancellable: false,
            id: String::new(),
            returnable: false,
            return_value: Value::NULL,
        }
    }
}

impl CallbackInfo {
    /// A plain `CallbackInfo`, for a `void` target method.
    pub fn new(id: impl Into<String>, cancellable: bool) -> Self {
        Self {
            cancellable,
            id: id.into(),
            ..Self::default()
        }
    }

    /// A `CallbackInfoReturnable`, for a target method that returns a value.
    pub fn returnable(id: impl Into<String>, cancellable: bool, return_value: Value) -> Self {
        Self {
            cancellable,
            id: id.into(),
            returnable: true,
            return_value,
            cancelled: false,
        }
    }
}

impl NativeObject for CallbackInfo {
    fn class_name(&self) -> &str {
        if self.returnable {
            "org/spongepowered/asm/mixin/injection/callback/CallbackInfoReturnable"
        } else {
            "org/spongepowered/asm/mixin/injection/callback/CallbackInfo"
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// The bytes behind `Files.newInputStream` for a path inside a mod jar.
#[derive(Debug, Clone)]
pub struct ByteStream {
    pub data: Vec<u8>,
    pub pos: usize,
}

impl NativeObject for ByteStream {
    fn class_name(&self) -> &str {
        "java/io/ByteArrayInputStream"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl GuiGraphics {
    pub fn new(width: i32, height: i32) -> Self {
        Self {
            width,
            height,
            draws: Vec::new(),
            pose: None,
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
    /// Heap id of the real `Vec3` the hit landed at (`HitResult.location`),
    /// when the bridge built one from the click.
    pub location: Option<usize>,
    /// `HitResult.isInside()`.
    pub inside: bool,
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
    /// Enchantment id and level pairs, as the host reports them.
    pub enchantments: Vec<(String, i32)>,
    /// The item's rarity (`common`, `uncommon`, `rare`, `epic`) as the host's
    /// item data reports it. Mods read it through `ItemStack.getRarity()`.
    pub rarity: Option<String>,
    /// The item's maximum durability as the host's item data reports it; 0 for
    /// an item that cannot take damage.
    pub max_damage: i32,
}

impl ItemStack {
    pub fn new(id: impl Into<String>, count: i32) -> Self {
        Self {
            item: Item { id: id.into() },
            count,
            damage: 0,
            enchantments: Vec::new(),
            rarity: None,
            max_damage: 0,
        }
    }

    pub fn with_rarity(mut self, rarity: Option<&str>) -> Self {
        self.rarity = rarity.map(str::to_string);
        self
    }

    pub fn with_max_damage(mut self, max_damage: i32) -> Self {
        self.max_damage = max_damage.max(0);
        self
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

/// An EnumProperty for block state properties whose Java initializer did not
/// produce one (`BlockStateProperties.HALF`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumProperty {
    pub name: String,
    pub values: Vec<String>,
}

impl NativeObject for EnumProperty {
    fn class_name(&self) -> &str {
        "net/minecraft/world/level/block/state/properties/EnumProperty"
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

/// Classes the runtime answers with a native object instead of running their
/// bytecode. A call on one of these, if reached, runs against a Rust stand-in
/// that may not implement the method; the census reports those references for
/// a human to check. Maintained by hand next to the `NativeObject` impls.
pub fn native_classes() -> &'static [&'static str] {
    &[
        "net/minecraft/world/item/ItemStack",
        "net/minecraft/world/item/Item",
        "net/minecraft/world/item/Item$Properties",
        "net/minecraft/world/level/Level",
        "net/minecraft/server/level/ServerLevel",
        "net/minecraft/world/level/block/Block",
        "net/minecraft/world/level/block/CropBlock",
        "net/minecraft/world/level/block/NetherWartBlock",
        "net/minecraft/world/level/block/CocoaBlock",
        "net/minecraft/world/level/block/SugarCaneBlock",
        "net/minecraft/world/level/block/CactusBlock",
        "net/minecraft/world/level/block/state/BlockState",
        "net/minecraft/world/level/block/state/properties/IntegerProperty",
        "net/minecraft/world/level/block/state/properties/EnumProperty",
        "net/minecraft/core/BlockPos",
        "net/minecraft/core/Direction",
        "net/minecraft/world/phys/BlockHitResult",
        "net/minecraft/core/particles/SimpleParticleType",
        "net/minecraft/tags/TagKey",
        "net/minecraft/world/entity/player/Player",
        "net/minecraft/server/level/ServerPlayer",
        "net/minecraft/client/gui/GuiGraphicsExtractor",
        "com/mojang/blaze3d/platform/NativeImage",
        "io/github/jamalam360/jamlib/api/events/core/EventResult",
        "org/spongepowered/asm/mixin/injection/callback/CallbackInfo",
        "java/io/ByteArrayInputStream",
        "java/io/OutputStream",
    ]
}

/// A particle a mod's code asked the level to send (`ServerLevel.sendParticles`).
#[derive(Debug, Clone, PartialEq)]
pub struct ParticleCall {
    /// `minecraft:falling_water`.
    pub id: String,
    pub at: [f64; 3],
    pub count: i32,
    pub spread: [f64; 3],
    pub speed: f64,
}

/// A `ParticleType` constant (`ParticleTypes.FALLING_WATER`), by the id it is
/// registered under. Synthetic: the runtime does not build the particle
/// registry, so the constant is made from its field name.
#[derive(Debug)]
pub struct ParticleType {
    pub id: String,
}

impl NativeObject for ParticleType {
    fn class_name(&self) -> &str {
        "net/minecraft/core/particles/SimpleParticleType"
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
    /// Positions holding a block a mod registered in code: the heap id of the
    /// mod's own `BlockState` object, which `getBlockState` answers instead of
    /// a native state.
    pub mod_states: HashMap<[i32; 3], usize>,
    /// `setBlock`/`setBlockAndUpdate` calls, in order: position and the heap
    /// id of the state written (a native `BlockState` or a mod's own).
    pub placed: Vec<([i32; 3], usize)>,
    /// The build height: `isOutsideBuildHeight` is `y < min_y || y >= max_y`.
    /// The overworld's, as the host does not report its dimension's.
    pub min_y: i32,
    pub max_y: i32,
    pub dropped_stacks: Vec<(BlockPos, String, i32)>,
    pub replanted: Vec<(BlockPos, BlockState)>,
    /// Positions a mod removed with `destroyBlock`/`removeBlock`, in order,
    /// and whether the removal drops the block's items.
    pub broken: Vec<(BlockPos, bool)>,
    /// `ServerLevel.sendParticles` calls, in order, for the host to show.
    pub particles: Vec<ParticleCall>,
    /// `Level.scheduleTick` calls, in order: position, the block's heap id and
    /// the delay in ticks.
    pub scheduled: Vec<([i32; 3], usize, i32)>,
    /// The block entities standing in this snapshot, by position: the heap id
    /// of the mod's own `BlockEntity` object, made by `EntityBlock.newBlockEntity`
    /// (`Level.getBlockEntity` answers it).
    pub block_entities: HashMap<[i32; 3], usize>,
    /// Block entities made during this call, for the bridge to keep for the
    /// host's position (`newBlockEntity` on first use or tick).
    pub made_block_entities: Vec<([i32; 3], usize)>,
    /// `Player.openMenu(MenuProvider)` calls, in order: the provider object.
    pub menus: Vec<usize>,
}

impl Level {
    pub fn new(is_client: bool) -> Self {
        Self {
            is_client,
            dimension: "minecraft:overworld".into(),
            blocks: HashMap::new(),
            mod_states: HashMap::new(),
            placed: Vec::new(),
            min_y: -64,
            max_y: 320,
            dropped_stacks: Vec::new(),
            replanted: Vec::new(),
            broken: Vec::new(),
            particles: Vec::new(),
            scheduled: Vec::new(),
            block_entities: HashMap::new(),
            made_block_entities: Vec::new(),
            menus: Vec::new(),
        }
    }

    /// Remove the block at `pos`, a mod's own state or a native one; false
    /// when there was nothing to remove. `drop` is `destroyBlock`'s
    /// `dropBlock`.
    pub fn destroy_block(&mut self, pos: &BlockPos, drop: bool) -> bool {
        let key = [pos.x, pos.y, pos.z];
        let existed = self.mod_states.remove(&key).is_some()
            || self
                .blocks
                .get(&key)
                .is_some_and(|state| state.block.id != "minecraft:air");
        if existed {
            self.blocks.insert(key, BlockState::new("minecraft:air"));
            self.broken.push((pos.clone(), drop));
        }
        existed
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
    /// The player's `java.util.UUID` object, made on first use.
    pub uuid: Option<usize>,
    /// A server-side player: its class is `ServerPlayer`, as seen by mods.
    pub server_side: bool,
    /// Rotation in degrees (`getYRot`, `getXRot`), as the host reports it.
    pub yaw: f32,
    pub pitch: f32,
    /// `Entity.position()`.
    pub position: [f64; 3],
    /// `Entity.getDirection()`: the real `Direction` constant, set by the
    /// bridge when it knows the player's rotation.
    pub direction: Option<Value>,
    /// The heap id of the acting hand's stack, when the bridge built one:
    /// `getItemInHand` hands the mod the same object the use methods got, so
    /// a change it makes can be seen.
    pub hand_stack: Option<usize>,
    /// The acting hand is the off hand.
    pub hand_is_off: bool,
    /// `setItemInHand` put another stack (or none, `Some(None)`) in the
    /// acting hand.
    pub hand_replaced: Option<Option<usize>>,
    /// What `Player.getInventory().add(stack)` put in the player's inventory:
    /// `(item id, count)` per call, for the host to give.
    pub added: Vec<(String, i32)>,
    /// `LivingEntity.equipment`: the heap id of the player's real
    /// `EntityEquipment`, made the first time mod code reads it.
    pub equipment: Option<usize>,
    /// `Player.containerMenu`: the menu this player has open, as a heap id.
    /// `AbstractContainerMenu.clicked` reads it back through
    /// `MultiPlayerGameMode.handleContainerInput`, which is what runs a slot
    /// click's rules; without it a click had no menu to apply to.
    pub container_menu: Option<usize>,
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
            uuid: None,
            server_side: false,
            yaw: 0.0,
            pitch: 0.0,
            position: [0.0; 3],
            direction: None,
            hand_stack: None,
            hand_is_off: false,
            hand_replaced: None,
            added: Vec::new(),
            equipment: None,
            container_menu: None,
        }
    }
}

impl NativeObject for Player {
    fn class_name(&self) -> &str {
        if self.server_side {
            "net/minecraft/server/level/ServerPlayer"
        } else {
            "net/minecraft/world/entity/player/Player"
        }
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
        self.alloc_hit_result_with(pos, dir, None, false)
    }

    pub fn alloc_hit_result_with(
        &mut self,
        pos: BlockPos,
        dir: Direction,
        location: Option<usize>,
        inside: bool,
    ) -> usize {
        self.alloc_native(BlockHitResult {
            pos,
            direction: dir,
            location,
            inside,
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
