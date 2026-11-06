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
use crate::minecraft::{ItemStack, Player};
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

/// Whether the block's empty-hand method runs: vanilla
/// `ServerPlayerGameMode.useItemOn` calls `useWithoutItem` only on the main
/// hand, and only when `useItemOn` was not run or answered
/// `TRY_WITH_EMPTY_HAND`; a `PASS` does not fall through.
fn runs_empty_hand(item_outcome: Option<Outcome>, off_hand: bool) -> bool {
    !off_hand && matches!(item_outcome, None | Some(Outcome::TryEmptyHand))
}

/// How a use changed the acting hand's stack, when it did: the host cannot
/// apply a hand-item change yet, so the use is refused rather than leaving
/// the player's item behind.
fn hand_change(
    initial_item: &str,
    initial_count: i32,
    final_item: &str,
    final_count: i32,
    replaced: bool,
) -> Option<String> {
    let changed = initial_item != final_item || initial_count != final_count;
    if !changed {
        return None;
    }
    Some(if replaced {
        format!("the hand was set to {final_item} x{final_count}")
    } else {
        format!("{initial_item} x{initial_count} -> {final_item} x{final_count}")
    })
}

/// The class of an `InteractionResult` the runtime cannot name, for the error.
fn result_class(heap: &crate::heap::Heap, value: &Value) -> String {
    value
        .as_obj()
        .and_then(|id| heap.get(id))
        .map(|object| object.class_name().to_string())
        .unwrap_or_else(|| "null".into())
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
    /// item) and then, when vanilla would, its `useWithoutItem`.
    /// `Ok(None)` is the block passing the click on; `Ok(Some(level))` the
    /// click it consumed, the level it ran on holding what it changed.
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
        let initial = self
            .heap
            .get_native::<ItemStack>(prepared.stack)
            .map(|stack| (stack.item.id.clone(), stack.count))
            .unwrap_or_else(|| ("minecraft:air".into(), 0));
        let level = Value::Object(Some(prepared.level));
        let pos = Value::Object(Some(prepared.pos));
        let player = Value::Object(Some(prepared.player));
        let hit = Value::Object(Some(prepared.hit_result));

        let mut consumed = false;
        let mut item_outcome = None;
        // An empty hand skips `useItemOn`: the inherited `BlockBehaviour`
        // default answers `TRY_WITH_EMPTY_HAND` for an empty stack, so a mod
        // override that does nothing for one is treated the same.
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
            let outcome = outcome_of(&self.heap, &result);
            if outcome == Outcome::Unknown {
                return Err(failure(format!(
                    "useItemOn returned an InteractionResult this runtime cannot name: {}",
                    result_class(&self.heap, &result)
                )));
            }
            if outcome.consumes() {
                consumed = true;
            } else {
                item_outcome = Some(outcome);
            }
        }
        if !consumed && use_without_item && runs_empty_hand(item_outcome, request.off_hand) {
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
            let outcome = outcome_of(&self.heap, &result);
            if outcome == Outcome::Unknown {
                return Err(failure(format!(
                    "useWithoutItem returned an InteractionResult this runtime cannot name: {}",
                    result_class(&self.heap, &result)
                )));
            }
            consumed = outcome.consumes();
        }
        if !consumed {
            return Ok(None);
        }
        let (hand_replaced, hand_stack) = match self.heap.get_native::<Player>(prepared.player) {
            Some(player) => (player.hand_replaced.is_some(), player.hand_stack),
            None => (false, None),
        };
        let (final_item, final_count) = hand_stack
            .and_then(|id| self.heap.get_native::<ItemStack>(id))
            .map(|stack| (stack.item.id.clone(), stack.count))
            .unwrap_or_else(|| ("minecraft:air".into(), 0));
        let replaced = hand_replaced && (initial.0 != final_item || initial.1 != final_count);
        if let Some(change) = hand_change(&initial.0, initial.1, &final_item, final_count, replaced)
        {
            return Err(failure(format!(
                "{}'s use changed the held item ({change}), which the host cannot apply",
                request.block
            )));
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

#[cfg(test)]
mod tests {
    use super::{Outcome, hand_change, runs_empty_hand};

    #[test]
    fn only_a_missing_or_try_empty_hand_item_result_runs_the_empty_hand() {
        // `useItemOn` was not run (an empty hand): the empty hand's turn runs.
        assert!(runs_empty_hand(None, false));
        // `TRY_WITH_EMPTY_HAND`: the empty hand's turn runs.
        assert!(runs_empty_hand(Some(Outcome::TryEmptyHand), false));
        // A `PASS` goes to the item's own use, not back to the block.
        assert!(!runs_empty_hand(Some(Outcome::Pass), false));
        // A consumed result ends the interaction.
        assert!(!runs_empty_hand(Some(Outcome::Success), false));
        assert!(!runs_empty_hand(Some(Outcome::Fail), false));
        // Only the main hand's turn runs the empty-hand method.
        for outcome in [
            None,
            Some(Outcome::TryEmptyHand),
            Some(Outcome::Pass),
            Some(Outcome::Success),
            Some(Outcome::Fail),
        ] {
            assert!(!runs_empty_hand(outcome, true), "{outcome:?}");
        }
    }

    #[test]
    fn a_use_that_leaves_the_hand_alone_is_not_a_change() {
        // The same item and count: the host's stack stands.
        assert_eq!(
            hand_change("minecraft:stone", 1, "minecraft:stone", 1, false),
            None
        );
        // A shrink (or a set to empty) is reported.
        assert_eq!(
            hand_change("minecraft:stone", 1, "minecraft:stone", 0, false).as_deref(),
            Some("minecraft:stone x1 -> minecraft:stone x0")
        );
        assert_eq!(
            hand_change("minecraft:stone", 1, "minecraft:air", 0, true).as_deref(),
            Some("the hand was set to minecraft:air x0")
        );
        // Another item, another count, or `setItemInHand` with a real change.
        assert!(hand_change("minecraft:stone", 1, "minecraft:dirt", 1, false).is_some());
        assert!(hand_change("minecraft:stone", 1, "minecraft:stone", 2, false).is_some());
        assert!(hand_change("minecraft:stone", 1, "minecraft:bucket", 1, true).is_some());
    }
}
