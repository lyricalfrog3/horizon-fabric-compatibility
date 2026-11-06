//! Placing a block a mod registered in code.
//!
//! `Block.getStateForPlacement(BlockPlaceContext)` is mod bytecode (or
//! Minecraft's real `Block` bytecode under it). The runtime runs it with a
//! context built from the host's real click: the clicked face, hit point and
//! position, the placer's rotation, position and sneaking, the hand and the
//! item in it. The context is a host boundary, so it is a native object
//! ([`PlaceContext`]) answering `BlockPlaceContext`'s getters, not a copy of
//! Minecraft's entity and level classes. The `Level` the mod reads is the
//! host's real blocks around the position (see [`PlacementRequest`]).

use crate::builtins::BuiltinResult;
use crate::heap::Heap;
use crate::interp::Interpreter;
use crate::minecraft::{BlockPos, BlockState, Direction, ItemStack, Level, Player};
use crate::value::{JavaObject, NativeObject, Value};
use hfc_core::{Error, Result};
use std::any::Any;
use std::collections::{BTreeMap, HashMap};

const BLOCK: &str = "net/minecraft/world/level/block/Block";
const DIRECTION: &str = "net/minecraft/core/Direction";
const VEC3: &str = "net/minecraft/world/phys/Vec3";
const PLACE_CONTEXT: &str = "net/minecraft/world/item/context/BlockPlaceContext";
const USE_ON_CONTEXT: &str = "net/minecraft/world/item/context/UseOnContext";
const STATE_FOR_PLACEMENT: &str = "(Lnet/minecraft/world/item/context/BlockPlaceContext;)Lnet/minecraft/world/level/block/state/BlockState;";

/// Blocks around the placement position the mod can read, in each direction.
pub(crate) const WORLD_RADIUS: i32 = 2;

/// The instruction budget of one `getStateForPlacement` call.
pub(crate) const PLACEMENT_BUDGET: u64 = 20_000_000;

/// The instruction budget of one scheduled block `tick` call. A tick should
/// cost thousands, not millions; the cap stops a mis-decoded loop.
pub(crate) const TICK_BUDGET: u64 = 2_000_000;

/// What `getStateForPlacement` is told about the click.
#[derive(Debug, Clone)]
pub struct PlacementRequest {
    /// The block's registry id (`namespace:path`).
    pub block: String,
    /// Where the block would go (`getClickedPos`).
    pub pos: [i32; 3],
    /// The block that was clicked.
    pub hit_pos: [i32; 3],
    /// The clicked face's normal.
    pub face: [i32; 3],
    /// Where the click landed (`getClickLocation`).
    pub hit: [f64; 3],
    pub inside: bool,
    /// The placer's rotation in degrees.
    pub yaw: f32,
    pub pitch: f32,
    /// The placer's feet (`Entity.position()`).
    pub player_pos: [f64; 3],
    /// `isSecondaryUseActive`.
    pub sneaking: bool,
    pub off_hand: bool,
    /// The item in the placing hand, `namespace:path`.
    pub item: String,
    /// Whether the clicked block is replaced (`replacingClickedOnBlock`).
    pub replace_clicked: bool,
}

/// The `BlockPlaceContext` a mod's `getStateForPlacement` is given.
#[derive(Debug, Clone)]
pub struct PlaceContext {
    pub level: Value,
    pub player: Value,
    pub stack: Value,
    pub hand: Value,
    pub pos: Value,
    pub hit_pos: Value,
    pub face: Value,
    pub click_location: Value,
    pub hit_result: Value,
    pub horizontal: Value,
    pub vertical: Value,
    /// `Direction.orderedByNearest(player)` with the clicked face first.
    pub nearest: Vec<Value>,
    pub yaw: f32,
    pub inside: bool,
    pub sneaking: bool,
    pub replace_clicked: bool,
}

impl NativeObject for PlaceContext {
    fn class_name(&self) -> &str {
        PLACE_CONTEXT
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// `BlockPlaceContext` / `UseOnContext` getters on a [`PlaceContext`].
pub(crate) fn place_method(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    if class_name != PLACE_CONTEXT && class_name != USE_ON_CONTEXT {
        return None;
    }
    let ctx = heap.get_native::<PlaceContext>(args.first()?.as_obj()?)?;
    let value = match method_name {
        "getClickedPos" => ctx.pos.clone(),
        "getClickedFace" => ctx.face.clone(),
        "getHorizontalDirection" => ctx.horizontal.clone(),
        "getNearestLookingDirection" => ctx.nearest.first().cloned().unwrap_or(Value::NULL),
        "getNearestLookingVerticalDirection" => ctx.vertical.clone(),
        "getPlayer" => ctx.player.clone(),
        "getLevel" => ctx.level.clone(),
        "getItemInHand" => ctx.stack.clone(),
        "getHand" => ctx.hand.clone(),
        "getClickLocation" => ctx.click_location.clone(),
        "getHitResult" => ctx.hit_result.clone(),
        "isInside" => Value::from_bool(ctx.inside),
        "isSecondaryUseActive" => Value::from_bool(ctx.sneaking),
        "replacingClickedOnBlock" => Value::from_bool(ctx.replace_clicked),
        // The host decided the block goes here before it asked.
        "canPlace" => Value::from_bool(true),
        "getRotation" => Value::Float(ctx.yaw),
        "getNearestLookingDirections" => {
            let nearest = ctx.nearest.clone();
            Value::Object(Some(heap.alloc(JavaObject::Array {
                elem_type: DIRECTION.into(),
                elements: nearest,
            })))
        }
        _ => return None,
    };
    Some(BuiltinResult::Handled(value))
}

fn opposite(direction: &Direction) -> Direction {
    Direction::from_array([-direction.x, -direction.y, -direction.z])
}

/// `Direction.fromYRot`.
fn from_y_rot(yaw: f64) -> Direction {
    match ((yaw / 90.0 + 0.5).floor() as i64) & 3 {
        0 => Direction::SOUTH,
        1 => Direction::WEST,
        2 => Direction::NORTH,
        _ => Direction::EAST,
    }
}

/// `Direction.orderedByNearest(entity)`.
fn ordered_by_nearest(yaw: f32, pitch: f32) -> [Direction; 6] {
    let pitch_rad = f64::from(pitch) * (std::f64::consts::PI / 180.0);
    let yaw_rad = -f64::from(yaw) * (std::f64::consts::PI / 180.0);
    let (pitch_sin, pitch_cos) = (pitch_rad.sin(), pitch_rad.cos());
    let (yaw_sin, yaw_cos) = (yaw_rad.sin(), yaw_rad.cos());
    let (x_pos, y_pos, z_pos) = (yaw_sin > 0.0, pitch_sin < 0.0, yaw_cos > 0.0);
    let x_yaw = yaw_sin.abs();
    let y_mag = pitch_sin.abs();
    let z_yaw = yaw_cos.abs();
    let (x_mag, z_mag) = (x_yaw * pitch_cos, z_yaw * pitch_cos);
    let ax = if x_pos {
        Direction::EAST
    } else {
        Direction::WEST
    };
    let ay = if y_pos {
        Direction::UP
    } else {
        Direction::DOWN
    };
    let az = if z_pos {
        Direction::SOUTH
    } else {
        Direction::NORTH
    };
    let make = |a: Direction, b: Direction, c: Direction| {
        [
            a.clone(),
            b.clone(),
            c.clone(),
            opposite(&c),
            opposite(&b),
            opposite(&a),
        ]
    };
    if x_yaw > z_yaw {
        if y_mag > x_mag {
            make(ay, ax, az)
        } else if z_mag > y_mag {
            make(ax, az, ay)
        } else {
            make(ax, ay, az)
        }
    } else if y_mag > z_mag {
        make(ay, az, ax)
    } else if x_mag > y_mag {
        make(az, ax, ay)
    } else {
        make(az, ay, ax)
    }
}

fn failure(what: impl Into<String>) -> Error {
    Error::new("interpreter", what.into())
}

/// A state key (`a=b,c=d`, sorted by property name) as a map.
pub fn parse_state_key(key: &str) -> BTreeMap<String, String> {
    key.split(',')
        .filter_map(|part| part.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// A state key from properties.
pub fn state_key_of(properties: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = properties.iter().collect();
    sorted.sort();
    sorted
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// The world the mod reads: the host's blocks near the placement.
pub struct WorldSnapshot {
    /// `(position, block id, properties)` of every non-air block.
    pub blocks: Vec<([i32; 3], String, Vec<(String, String)>)>,
}

impl Interpreter {
    /// A real `Direction` constant.
    fn direction_value(&mut self, direction: &Direction) -> Value {
        self.resolve_static_field(DIRECTION, direction.name)
    }

    /// The `BlockState` objects of a registered block, by state key.
    pub(crate) fn mod_state_objects(&mut self, id: &str) -> Result<HashMap<String, Value>> {
        if let Some(cached) = self.heap.block_state_objects.get(id) {
            return Ok(cached.clone());
        }
        let object = self
            .heap
            .registered_blocks
            .iter()
            .find(|b| b.id == id)
            .map(|b| b.object)
            .ok_or_else(|| failure(format!("{id} is not a registered block")))?;
        let states = self.block_states(&Value::Object(Some(object)))?;
        let map: HashMap<String, Value> = states.into_iter().collect();
        self.heap
            .block_state_objects
            .insert(id.to_string(), map.clone());
        Ok(map)
    }

    /// The level mod code reads: the host's blocks, a mod's own as its state
    /// objects.
    pub(crate) fn snapshot_level(&mut self, world: &WorldSnapshot) -> Result<usize> {
        let mut level = Level::new(false);
        for (at, id, properties) in &world.blocks {
            if self.heap.registered_blocks.iter().any(|b| &b.id == id) {
                let key = state_key_of(properties);
                if let Some(state) = self
                    .mod_state_objects(id)?
                    .get(&key)
                    .and_then(Value::as_obj)
                {
                    level.mod_states.insert(*at, state);
                    continue;
                }
            }
            let mut state = BlockState::new(id);
            for (k, v) in properties {
                state.properties.insert(k.clone(), v.clone());
            }
            level.blocks.insert(*at, state);
        }
        Ok(self.heap.alloc_level(level))
    }

    /// The level, placer and context a block's placement methods are given,
    /// built from the host's click and the world around it.
    pub(crate) fn prepare_placement(
        &mut self,
        request: &PlacementRequest,
        world: &WorldSnapshot,
    ) -> Result<Prepared> {
        self.ensure_initialized(DIRECTION)?;

        // The level: the host's blocks, a mod's own as its state objects.
        let level_id = self.snapshot_level(world)?;

        // The placer.
        let horizontal = from_y_rot(f64::from(request.yaw));
        let horizontal_value = self.direction_value(&horizontal);
        let mut player = Player::new(level_id);
        player.server_side = true;
        player.is_crouching = request.sneaking;
        player.yaw = request.yaw;
        player.pitch = request.pitch;
        player.position = request.player_pos;
        player.direction = Some(horizontal_value.clone());
        let mut stack = ItemStack::new(&request.item, 1);
        if request.item.is_empty() || request.item == "minecraft:air" {
            stack = ItemStack::empty();
        }
        player.hand_item = Some(stack.clone());
        let stack_id = self.heap.alloc_item_stack(stack);
        // One object for the acting hand: `getItemInHand` and the use methods
        // see the same stack, so a change the mod makes is readable.
        player.hand_stack = Some(stack_id);
        player.hand_is_off = request.off_hand;
        let player_id = self.heap.alloc_player(player);

        // The click.
        let face = Direction::from_array(request.face);
        let mut nearest = ordered_by_nearest(request.yaw, request.pitch).to_vec();
        if !request.replace_clicked {
            let target = opposite(&face);
            if let Some(index) = nearest.iter().position(|d| *d == target) {
                nearest[..=index].rotate_right(1);
            }
        }
        let vertical = if request.pitch < 0.0 {
            Direction::UP
        } else {
            Direction::DOWN
        };
        let nearest_values: Vec<Value> = nearest.iter().map(|d| self.direction_value(d)).collect();
        let vertical_value = self.direction_value(&vertical);
        let face_value = self.direction_value(&face);
        let hand = self.resolve_static_field(
            "net/minecraft/world/InteractionHand",
            if request.off_hand {
                "OFF_HAND"
            } else {
                "MAIN_HAND"
            },
        );
        let hit_pos = BlockPos {
            x: request.hit_pos[0],
            y: request.hit_pos[1],
            z: request.hit_pos[2],
        };
        let hit_result = self.heap.alloc_hit_result(hit_pos.clone(), face.clone());
        let pos_id = self
            .heap
            .alloc_block_pos(request.pos[0], request.pos[1], request.pos[2]);
        let hit_pos_id = self.heap.alloc_block_pos(hit_pos.x, hit_pos.y, hit_pos.z);

        self.ensure_initialized(VEC3)?;
        let vec = self.alloc_uninitialized(VEC3);
        self.execute_special_method(
            VEC3,
            "<init>",
            "(DDD)V",
            &[
                Value::Object(Some(vec)),
                Value::Double(request.hit[0]),
                Value::Double(request.hit[1]),
                Value::Double(request.hit[2]),
            ],
        )?;

        let context = PlaceContext {
            level: Value::Object(Some(level_id)),
            player: Value::Object(Some(player_id)),
            stack: Value::Object(Some(stack_id)),
            hand: hand.clone(),
            pos: Value::Object(Some(pos_id)),
            hit_pos: Value::Object(Some(hit_pos_id)),
            face: face_value,
            click_location: Value::Object(Some(vec)),
            hit_result: Value::Object(Some(hit_result)),
            horizontal: horizontal_value,
            vertical: vertical_value,
            nearest: nearest_values,
            yaw: request.yaw,
            inside: request.inside,
            sneaking: request.sneaking,
            replace_clicked: request.replace_clicked,
        };
        let context_id = self.heap.alloc_native(context);
        Ok(Prepared {
            level: level_id,
            player: player_id,
            stack: stack_id,
            context: context_id,
            pos: pos_id,
            hit_result,
            hand,
        })
    }

    fn registered_block_object(&self, id: &str) -> Result<usize> {
        self.heap
            .registered_blocks
            .iter()
            .find(|b| b.id == id)
            .map(|b| b.object)
            .ok_or_else(|| failure(format!("{id} is not a registered block")))
    }

    /// Run the block's `getStateForPlacement` for `request`. `Ok(None)` is the
    /// mod refusing the placement (it returned null); `Ok(Some(key))` the state
    /// it chose, as a state key of the block.
    pub(crate) fn state_for_placement(
        &mut self,
        request: &PlacementRequest,
        world: &WorldSnapshot,
    ) -> Result<Option<String>> {
        let block = self.registered_block_object(&request.block)?;
        let prepared = self.prepare_placement(request, world)?;
        let state = self.execute_method(
            BLOCK,
            "getStateForPlacement",
            STATE_FOR_PLACEMENT,
            &[
                Value::Object(Some(block)),
                Value::Object(Some(prepared.context)),
            ],
        )?;
        if state.is_null() {
            return Ok(None);
        }
        let key = self.state_key(&state)?;
        let own = self.mod_state_objects(&request.block)?;
        match own.get(&key) {
            Some(known) if *known == state => Ok(Some(key)),
            _ => Err(failure(format!(
                "getStateForPlacement of {} returned a state that is not one of its own ({key})",
                request.block
            ))),
        }
    }

    /// Run the block's `onPlace` and `setPlacedBy` (the ones it overrides)
    /// for the block now standing in `state_key` at `request.pos`, and return
    /// the blocks they set, in order, as `(position, block id, state key)`.
    pub(crate) fn placed_by(
        &mut self,
        request: &PlacementRequest,
        world: &WorldSnapshot,
        state_key: &str,
        on_place: bool,
        set_placed_by: bool,
    ) -> Result<Vec<([i32; 3], String, String)>> {
        let block = self.registered_block_object(&request.block)?;
        let placed = self
            .mod_state_objects(&request.block)?
            .get(state_key)
            .cloned()
            .ok_or_else(|| failure(format!("{} has no state {state_key}", request.block)))?;
        // The block stands where it was placed.
        let mut world = WorldSnapshot {
            blocks: world.blocks.clone(),
        };
        world.blocks.retain(|(at, _, _)| *at != request.pos);
        world.blocks.push((
            request.pos,
            request.block.clone(),
            parse_state_key(state_key).into_iter().collect(),
        ));
        let prepared = self.prepare_placement(request, &world)?;
        if on_place {
            let air = self
                .heap
                .alloc_block_state(BlockState::new("minecraft:air"));
            self.execute_method(
                BLOCK,
                "onPlace",
                "(Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/Level;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;Z)V",
                &[
                    Value::Object(Some(block)),
                    placed.clone(),
                    Value::Object(Some(prepared.level)),
                    Value::Object(Some(prepared.pos)),
                    Value::Object(Some(air)),
                    Value::Int(0),
                ],
            )?;
        }
        if set_placed_by {
            self.execute_method(
                BLOCK,
                "setPlacedBy",
                "(Lnet/minecraft/world/level/Level;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/entity/LivingEntity;Lnet/minecraft/world/item/ItemStack;)V",
                &[
                    Value::Object(Some(block)),
                    Value::Object(Some(prepared.level)),
                    Value::Object(Some(prepared.pos)),
                    placed,
                    Value::Object(Some(prepared.player)),
                    Value::Object(Some(prepared.stack)),
                ],
            )?;
        }
        self.level_writes(prepared.level)
    }

    /// The states a level was changed to through `setBlock` or
    /// `setBlockAndUpdate`, in first-write order, the last write to a position
    /// winning. Each as `(position, block id, state key)`.
    pub(crate) fn level_writes(&mut self, level: usize) -> Result<Vec<([i32; 3], String, String)>> {
        let Some(native) = self.heap.get_native::<Level>(level) else {
            return Ok(Vec::new());
        };
        let writes: Vec<([i32; 3], Value)> = native
            .placed
            .iter()
            .map(|(at, id)| (*at, Value::Object(Some(*id))))
            .collect();
        let replanted = native.replanted.clone();
        let mut order: Vec<[i32; 3]> = Vec::new();
        let mut last: HashMap<[i32; 3], (String, String)> = HashMap::new();
        for (at, state) in writes {
            let change = self.state_change(&state)?;
            if !last.contains_key(&at) {
                order.push(at);
            }
            last.insert(at, change);
        }
        for (pos, state) in replanted {
            let at = [pos.x, pos.y, pos.z];
            let mut props: Vec<(String, String)> = state
                .properties
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            props.sort();
            if !last.contains_key(&at) {
                order.push(at);
            }
            last.insert(at, (state.block.id.clone(), state_key_of(&props)));
        }
        Ok(order
            .into_iter()
            .map(|at| {
                let (block, key) = last.remove(&at).unwrap_or_default();
                (at, block, key)
            })
            .collect())
    }

    /// The block id and state key of a state written to a level: a native
    /// `BlockState` or a mod block's own object.
    fn state_change(&mut self, state: &Value) -> Result<(String, String)> {
        if let Some(native) = state
            .as_obj()
            .and_then(|id| self.heap.get_native::<BlockState>(id))
        {
            let mut props: Vec<(String, String)> = native
                .properties
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            props.sort();
            return Ok((native.block.id.clone(), state_key_of(&props)));
        }
        let key = self.state_key(state)?;
        let owner = match state.as_obj().and_then(|id| self.heap.get(id)) {
            Some(JavaObject::Instance { fields, .. }) => {
                fields.get("owner").and_then(Value::as_obj)
            }
            _ => None,
        };
        let id = owner
            .and_then(|o| self.heap.registered_blocks.iter().find(|b| b.object == o))
            .map(|b| b.id.clone())
            .ok_or_else(|| {
                failure("a level write was a state of no registered block".to_string())
            })?;
        Ok((id, key))
    }
}

/// The objects a placement method is called with.
pub(crate) struct Prepared {
    pub level: usize,
    pub player: usize,
    pub stack: usize,
    pub context: usize,
    pub pos: usize,
    /// The `BlockHitResult` of the click (`UseOnContext.getHitResult`).
    pub hit_result: usize,
    /// The `InteractionHand` constant of the hand (`UseOnContext.getHand`).
    pub hand: Value,
}
