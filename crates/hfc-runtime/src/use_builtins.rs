//! Using a block a mod registered: its own `useItemOn`, `useWithoutItem` and
//! `tick`.
//!
//! Both use methods are mod bytecode (or Minecraft's real `BlockBehaviour`
//! bytecode under it). The runtime runs them with the same `Level` snapshot,
//! placer and `UseOnContext` a placement gets (see `place_builtins.rs`), at the
//! state the host's block actually stands in, and maps the
//! `InteractionResult` back to the host: `SUCCESS`/`FAIL` consume the click,
//! `PASS` and `TRY_WITH_EMPTY_HAND` let the empty hand's turn run and, when
//! that passes too, the host's own handling.
//!
//! `tick` is the block's scheduled tick (`Level.scheduleTick`), which the host
//! runs when it is due. Both return the level they ran on: the bridge drains
//! its property writes, scheduled ticks and particles for the host.

use crate::interp::Interpreter;
use crate::place_builtins::{PlacementRequest, WorldSnapshot, state_key_of};
use crate::value::{JavaObject, Value};
use hfc_core::{Error, Result};
use std::collections::HashMap;

const BLOCK: &str = "net/minecraft/world/level/block/Block";

const USE_ITEM_ON: &str = "(Lnet/minecraft/world/item/ItemStack;Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/Level;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/world/InteractionHand;Lnet/minecraft/world/phys/BlockHitResult;)Lnet/minecraft/world/InteractionResult;";

const USE_WITHOUT_ITEM: &str = "(Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/Level;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/world/phys/BlockHitResult;)Lnet/minecraft/world/InteractionResult;";

const TICK: &str = "(Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/server/level/ServerLevel;Lnet/minecraft/core/BlockPos;Lnet/minecraft/util/RandomSource;)V";

/// What one of the block's use methods answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// `InteractionResult.PASS`: the click goes on.
    Pass,
    /// `InteractionResult.TRY_WITH_EMPTY_HAND`: the empty hand's turn runs.
    TryEmptyHand,
    /// `InteractionResult.SUCCESS`: the block did it.
    Success,
    /// `InteractionResult.FAIL`: the block refused; the click is spent.
    Fail,
    /// An `InteractionResult` this runtime cannot name: nothing was invented.
    Unknown,
}

impl Outcome {
    /// `InteractionResult.consumesAction`.
    fn consumes(self) -> bool {
        matches!(self, Self::Success | Self::Fail)
    }
}

fn failure(what: impl Into<String>) -> Error {
    Error::new("interpreter", what.into())
}

/// Which `InteractionResult` record `value` is. Without Minecraft's classes
/// the runtime has no result to name, so the value is not guessed at.
fn outcome_of(heap: &crate::heap::Heap, value: &Value) -> Outcome {
    let Some(id) = value.as_obj() else {
        return Outcome::Unknown;
    };
    let Some(JavaObject::Instance { class_name, .. }) = heap.get(id) else {
        return Outcome::Unknown;
    };
    match class_name.rsplit('$').next().unwrap_or(class_name) {
        "Pass" | "PASS" => Outcome::Pass,
        "TryEmptyHandInteraction" | "TRY_WITH_EMPTY_HAND" => Outcome::TryEmptyHand,
        "Success" | "SUCCESS" | "SUCCESS_SERVER" | "CONSUME" => Outcome::Success,
        "Fail" | "FAIL" => Outcome::Fail,
        _ => Outcome::Unknown,
    }
}

impl Interpreter {
    /// Run the block's `useItemOn` (when it overrides it and the hand holds an
    /// item) and then, if that did not consume the click, its `useWithoutItem`
    /// (when it overrides it). `Ok(None)` is the block passing the click on;
    /// `Ok(Some(level))` the click it consumed, the level it ran on holding
    /// what it changed.
    pub(crate) fn block_used(
        &mut self,
        request: &PlacementRequest,
        world: &WorldSnapshot,
        use_item: bool,
        use_without_item: bool,
    ) -> Result<Option<usize>> {
        let block = self
            .heap
            .registered_blocks
            .iter()
            .find(|b| b.id == request.block)
            .map(|b| b.object)
            .ok_or_else(|| failure(format!("{} is not a registered block", request.block)))?;
        let properties = world
            .blocks
            .iter()
            .find(|(at, ..)| *at == request.pos)
            .map(|(_, _, properties)| properties.clone())
            .unwrap_or_default();
        let key = state_key_of(&properties);
        let state = self
            .mod_state_objects(&request.block)?
            .get(&key)
            .cloned()
            .ok_or_else(|| failure(format!("{} has no state {key}", request.block)))?;
        let prepared = self.prepare_placement(request, world)?;
        let level = Value::Object(Some(prepared.level));
        let pos = Value::Object(Some(prepared.pos));
        let player = Value::Object(Some(prepared.player));
        let hit = Value::Object(Some(prepared.hit_result));

        let mut handled = false;
        if use_item && !request.item.is_empty() && request.item != "minecraft:air" {
            let result = self.execute_method(
                BLOCK,
                "useItemOn",
                USE_ITEM_ON,
                &[
                    Value::Object(Some(block)),
                    Value::Object(Some(prepared.stack)),
                    state.clone(),
                    level.clone(),
                    pos.clone(),
                    player.clone(),
                    prepared.hand.clone(),
                    hit.clone(),
                ],
            )?;
            handled = outcome_of(&self.heap, &result).consumes();
        }
        // `TRY_WITH_EMPTY_HAND`: only the main hand's turn uses the block
        // itself, as the host's own `use_item_on_branch` does.
        if !handled && use_without_item && !request.off_hand {
            let result = self.execute_method(
                BLOCK,
                "useWithoutItem",
                USE_WITHOUT_ITEM,
                &[
                    Value::Object(Some(block)),
                    state,
                    level.clone(),
                    pos,
                    player,
                    hit,
                ],
            )?;
            handled = outcome_of(&self.heap, &result).consumes();
        }
        if !handled {
            return Ok(None);
        }
        Ok(Some(prepared.level))
    }

    /// Run the block's own `tick` at `pos`, now that its scheduled tick is
    /// due, over the world around it. `Ok(level)` is the level it ran on, the
    /// bridge draining what it changed.
    pub(crate) fn block_tick(
        &mut self,
        id: &str,
        pos: [i32; 3],
        world: &WorldSnapshot,
    ) -> Result<usize> {
        let block = self
            .heap
            .registered_blocks
            .iter()
            .find(|b| b.id == id)
            .map(|b| b.object)
            .ok_or_else(|| failure(format!("{id} is not a registered block")))?;
        let properties = world
            .blocks
            .iter()
            .find(|(at, ..)| *at == pos)
            .map(|(_, _, properties)| properties.clone())
            .unwrap_or_default();
        let key = state_key_of(&properties);
        let state = self
            .mod_state_objects(id)?
            .get(&key)
            .cloned()
            .ok_or_else(|| failure(format!("{id} has no state {key}")))?;
        let level = self.snapshot_level(world)?;
        let block_pos = self.heap.alloc_block_pos(pos[0], pos[1], pos[2]);
        // The runtime's `RandomSource`: a tick that draws from it is reported,
        // not given made-up numbers.
        let random = self.heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/util/RandomSource".into(),
            fields: HashMap::new(),
        });
        self.execute_method(
            BLOCK,
            "tick",
            TICK,
            &[
                Value::Object(Some(block)),
                state,
                Value::Object(Some(level)),
                Value::Object(Some(block_pos)),
                Value::Object(Some(random)),
            ],
        )?;
        Ok(level)
    }
}
