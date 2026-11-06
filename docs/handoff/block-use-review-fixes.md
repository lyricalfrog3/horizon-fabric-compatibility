# Handoff: fix the review findings on block use, ticks and the block-facts cache

You are fixing ten findings that a code review raised against root commit `33d06b5`
("Run block use code and ticks, export container rules, cache block facts") in
**Horizon Fabric Compatibility** at `/home/daniel/Desktop/horizon-rust-fabric`.

Before you touch code, read the root `AGENTS.md`, [`docs/testing.md`](../testing.md), the
"Registered blocks" sections of [`docs/architecture/fabric-runtime.md`](../architecture/fabric-runtime.md)
(stages C1, C2 and C3), and [blocks-c3-custom-use.md](blocks-c3-custom-use.md). If you change client
code, also read `workspace/horizon/AGENTS.md`.

Every finding below was checked against the code at `33d06b5`. Line numbers are from that commit
and may drift by a few lines, so search for the quoted function names.

---

## 0. Rules that apply to every fix

- **Never fake compatibility.** If the runtime cannot carry out something a mod's use or tick code
  did, it must not report `BlockUse::Handled`, and so must not cancel the host's click. Report
  `BlockUse::Failed(reason)` or apply the effect for real. Do not leave it silently half-done.
- **No mod-specific code.** Do not branch on `mdm`, a block id, a class name or a jar name. Every
  fix goes in the shared path: `use_builtins.rs`, `bridge/blocks.rs`, `builtins.rs`, `minecraft.rs`
  or the `HostBridge` trait.
- **Do not weaken tests.** The tests in `crates/hfc-runtime/tests/registered_blocks.rs` load the
  real Modern Decorations jar from `test-mods/` and the machine's Minecraft. If those are missing,
  the tests return early. Do not replace them with fabricated Minecraft classes.
- **Vanilla is the spec.** The client is Minecraft 26.3. The vanilla behaviour quoted here is
  `ServerPlayerGameMode.useItemOn` and `BlockBehaviour`. Confirm a detail from the decompiled
  sources (`workspace/horizon/.local/minecraft/26.3/sources/`) or with
  `python3 workspace/horizon/tools/rea_vanilla.py method <class> <method>` before relying on it.
- Commands (run from the repo root):
  ```sh
  cargo fmt --check -p hfc-runtime
  cargo test -p hfc-runtime --test registered_blocks
  cargo test -p hfc-runtime --test block_facts_cache
  cargo test -p hfc-runtime --lib
  # only if workspace/horizon changes:
  (cd workspace/horizon && cargo build --profile fast -p horizon-client --no-default-features)
  ```
  Run the test file for the area you changed, not the whole workspace, after each fix.
- Pure decision logic (which method runs, whether a click is consumed) should go in small
  free functions with unit tests in the same file (`#[cfg(test)] mod tests`). That logic can then
  be tested without Minecraft on the machine. Those unit tests must not build fake
  `net/minecraft` objects.

### The code path in one picture

```
host: Game::use_hand (workspace/horizon/crates/horizon-client/src/game.rs ~6128)
  -> native_right_click_block -> fabric_right_click_block (game/fabric_bridge/mod.rs:399)
     -> FabricBridge::right_click_block (crates/hfc-runtime/src/bridge/events.rs:514)
        1-2. crop/age snapshot
        3.   UseBlockCallback listeners      (always passed MAIN_HAND)      <- finding 7
        4.   JamLib InteractionEvent.USE_BLOCK (always passed MAIN_HAND)    <- finding 7
        5.   FabricBridge::block_used (bridge/blocks.rs:381)                <- findings 1-6
               -> Interpreter::block_used (use_builtins.rs:79)
                    prepare_placement (place_builtins.rs:289): Level snapshot, Player,
                      count-1 ItemStack, BlockHitResult (no location)
                    useItemOn, then useWithoutItem
               -> FabricBridge::effects_of (bridge/blocks.rs ~490): drains ONLY
                    particles, scheduled, and placed/replanted via level_writes
             Handled(effects) -> apply_block_effects -> Flow::Cancel
host: scheduled tick due (game/fabric_bridge/mod.rs:570)
  -> FabricBridge::tick_block (bridge/blocks.rs ~465) -> Interpreter::block_tick
     -> effects_of -> apply_block_effects                                   <- findings 2, 3, 10
```

Key data types:

- `minecraft::Level` (`crates/hfc-runtime/src/minecraft.rs:572`): `placed`, `replanted`,
  `dropped_stacks: Vec<(BlockPos, String, i32)>`, `broken: Vec<BlockPos>`, `particles`,
  `scheduled`, `blocks` (native states), `mod_states` (heap ids of a mod's own state objects).
- `BlockEffects` (`bridge/blocks.rs:349`): only `changes`, `schedules`, `particles`.
- `HostBridge` (`bridge/mod.rs:34`): `block`, `block_property`, `set_block_property`, `give`,
  `break_block` (default `false`), `held_item`, `off_hand_item`, `drop_item`
  (default calls `give`), `block_properties`, `schedule_block_tick`, `particle`.
  The client implements it in `workspace/horizon/crates/horizon-client/src/game/fabric_bridge/`
  (`HostBridgeAdapter`).

---

## Suggested order

| # | Finding | Difficulty | Touches host? |
|---|---|---|---|
| 4 | Sneak-click runs block use | easy | no |
| 5 | PASS falls through to `useWithoutItem` | easy | no |
| 7 | Listeners always get `MAIN_HAND` | easy | no |
| 9 | Cache hit not checked against registered ids | easy | no |
| 8 | `sendParticles` overloads misread | easy-medium | no |
| 3 | Item drops lost | easy-medium | no (uses existing `drop_item`) |
| 2 | Block removals lost | medium | yes (new hook) |
| 1 | Held-item changes lost (item duplication) | medium-hard | stage 1 no, stage 2 yes |
| 6 | `BlockHitResult` has no click location | medium | no |
| 10 | Heap grows without limit on ticking blocks | hard | no |

Commit each fix (or each small group) on its own, with a message that says what behaviour
changed. Fixes 1 and 2 need client changes: commit those inside `workspace/horizon`, push to
remote `fork` branch `hfc`, then commit the submodule pointer at the root (see root `AGENTS.md`,
"Git"). Never push to upstream `horizon-rust/horizon`.

---

## Finding 4: sneaking with an item in hand must skip the block's use methods

**Where:** `FabricBridge::block_used`, `crates/hfc-runtime/src/bridge/blocks.rs:381-458`.

**Problem:** `event.sneaking` is only copied into the `PlacementRequest` (`sneaking: event.sneaking`).
It is never used to decide whether to run the block's code. Vanilla
`ServerPlayerGameMode.useItemOn` does this:

```java
boolean hasItems = !player.getMainHandItem().isEmpty() || !player.getOffhandItem().isEmpty();
boolean suppress = player.isSecondaryUseActive() && hasItems;
if (!suppress) { /* state.useItemOn, then maybe state.useWithoutItem */ }
// then the item's own useOn (placement)
```

The host already follows this in its own path (`game.rs` `use_hand`:
`if !(sneaking && hands_full)`), but `right_click_block` runs before that check and returns
`Flow::Cancel`. As a result, sneak-placing a block against a mod block (for example the shower
head) toggles the mod block and places nothing.

**Fix:**
- At the top of `block_used`, after the block-fact lookup, read both hands:
  `host.held_item()` and `host.off_hand_item()`. An item is empty when it is `None`, `""` or
  `"minecraft:air"`. If `event.sneaking` is true and either hand holds an item, return
  `BlockUse::NotOverridden`.
- Put the decision in a free function, for example
  `fn use_suppressed(sneaking: bool, main: Option<&str>, off: Option<&str>) -> bool`, and unit
  test it: sneaking with an empty main hand and an item in the off hand is suppressed; sneaking
  with both hands empty is not; not sneaking is never suppressed.

**Test (real classpath, `tests/registered_blocks.rs`):** next to the shower-head test, add a host
`World` whose `held_item()` returns `Some("minecraft:stone")`, send an event with
`sneaking: true`, and assert `BlockUse::NotOverridden`. Check how the test's `World` implements
`HostBridge` (it currently uses the defaults for `held_item`). Add a field if needed.

---

## Finding 5: only `TRY_WITH_EMPTY_HAND` may fall through to `useWithoutItem`

**Where:** `Interpreter::block_used`, `crates/hfc-runtime/src/use_builtins.rs:111-150`.

**Problem:** `handled = outcome_of(..).consumes()` is false for `Pass`, `TryEmptyHand` and
`Unknown`. `useWithoutItem` then runs in all three cases (`if !handled && use_without_item && !request.off_hand`).
In vanilla:

```java
InteractionResult r = state.useItemOn(stack, level, player, hand, hit);
if (r.consumesAction()) return r;
if (r instanceof InteractionResult.TryEmptyHandInteraction && hand == InteractionHand.MAIN_HAND) {
    InteractionResult r2 = state.useWithoutItem(level, player, hit);
    if (r2.consumesAction()) return r2;
}
// PASS (or TRY_WITH_EMPTY_HAND that did not consume) -> the item's own use
```

**Fix:**
- Keep the `Outcome` of `useItemOn`. Run `useWithoutItem` only when `useItemOn` was **not run**
  (an empty hand, or the block does not override it; the inherited default answers
  `TRY_WITH_EMPTY_HAND`) or when it returned `Outcome::TryEmptyHand`.
- `Outcome::Unknown` must not be treated as PASS. The runtime cannot name the result, so
  `block_used` should return an error, which becomes `BlockUse::Failed`, so nothing is invented.
  Return something like
  `Err(failure("useItemOn returned an InteractionResult this runtime cannot name: <class>"))`.
  Do the same for an `Unknown` from `useWithoutItem`.
- Factor the decision into a free function, for example
  `fn runs_empty_hand(item_outcome: Option<Outcome>, off_hand: bool) -> bool`, and unit test the
  table: `None` -> true (main hand), `Some(TryEmptyHand)` -> true, `Some(Pass)` -> false,
  `Some(Success | Fail)` -> false, and any off-hand case -> false.

**Related note (not a review finding, but on the same line):** `block_used` skips `useItemOn`
entirely when the hand is empty. Vanilla calls `useItemOn` with `ItemStack.EMPTY` too, and the
default answers `TRY_WITH_EMPTY_HAND`. Leave it skipped, which matches the default, but write in
a comment that this assumes the mod's override does nothing for an empty stack. Do not change it
in this fix.

---

## Finding 7: the Fabric/JamLib listeners must get the hand that is acting

**Where:** `FabricBridge::right_click_block`, `crates/hfc-runtime/src/bridge/events.rs:559-561`
(`main_hand` is resolved once), used at ~588 (`UseBlockCallback.interact`) and ~613-622
(JamLib `InteractionEvent$UseBlock.onUseBlock`).

**Problem:** This is not conditional. The host already calls `right_click_block` once per hand.
`Game::use_hand` passes `self.hand == Hand::Off` as `off_hand`, and `startUseItem` runs it for
both hands. So every listener runs twice per click, both times with `MAIN_HAND`. A listener that
does `if (hand != InteractionHand.MAIN_HAND) return PASS;` acts twice.

**Fix:** Resolve the hand from the event:

```rust
let hand = self.interp.resolve_static_field(
    "net/minecraft/world/InteractionHand",
    if event.off_hand { "OFF_HAND" } else { "MAIN_HAND" },
);
```

Pass `hand` to both listener dispatches. Rename the variable so no `main_hand` is left. Also set
the snapshot `Player`'s hand item to the acting hand's item if `Player` has a field for it
(`Player::hand_item`, `minecraft.rs:666`). Right now step 2 builds `Player::new(level_id)` with
no item. Keep that change small: only fill it in if a test shows a listener reading it.

**Test:** There is no listener fixture for this in `test-mods/`, so check `test-mods/README.md`
for one that registers `UseBlockCallback`. If none exists, do not invent a jar. Say in the commit
message that the change is untested against a real listener, and keep the diff obviously correct.

---

## Finding 9: a cache hit must match this start's registered blocks

**Where:** `FabricBridge::load_block_facts_cache`, `crates/hfc-runtime/src/bridge/block_cache.rs:162-201`.
It is called from `bridge/mod.rs:817`, **after** the entrypoints ran, so
`self.interp.heap.registered_blocks` is already filled.

**Problem:** The key covers the jars, the jimage and the versions. It does not cover the game
dir's `config/` files, which can change which blocks a mod registers. After a config edit, a hit
installs a stale list. New blocks then have no facts (they are not exported, and use and tick
return `NotOverridden`), and removed blocks are still exported.

**Fix:**
- After the version and key checks, compare the ids:
  `cached.blocks.iter().map(|b| &b.id)` must equal
  `self.interp.heap.registered_blocks.iter().map(|b| &b.id)`, **same order, same length**.
  On a mismatch, log at `info` ("block facts cache: registered blocks changed since it was
  written; reading them again") and return `false`. Leave the heap untouched; the function's
  contract says a miss leaves it untouched.
- Check that the `id` field names match: the cached element type is the block fact struct in
  `bridge/blocks.rs` (field `id`), and `registered_blocks` elements also have `id` (they are used
  as `b.id` in `use_builtins.rs`).
- The ids can match while other facts differ (a config that changes a block's hardness). That is
  still a gap. Do not hash config files into the key in this fix. Add one sentence to the cache
  section of `docs/architecture/fabric-runtime.md` saying the cache trusts facts other than the id
  list to depend only on the jars.

**Test (`crates/hfc-runtime/tests/block_facts_cache.rs`):** that file already writes a cache and
checks hits and misses. Add a case that writes a cache file, edits its JSON so one block id
differs (or drops one block), keeps the same key, then starts a bridge and asserts it was **not**
loaded from the cache (`block_facts_cache.from_cache` or whatever accessor that file already
uses) and that the facts equal a fresh read. Editing the cache file is fine; fabricating Minecraft
classes is not.

---

## Finding 8: `sendParticles` must dispatch on the descriptor

**Where:** `crates/hfc-runtime/src/builtins.rs:6077-6100`
(`} else if method_name == "sendParticles" && args.len() >= 10 {`).

**Problem:** Every overload with 10 or more args is read as
`(ParticleOptions, x, y, z, count, dx, dy, dz, speed)`. 26.3 also has the 11-parameter
`(ParticleOptions, boolean overrideLimiter, boolean alwaysShow, x, y, z, count, dx, dy, dz, speed)`,
and there is a `ServerPlayer`-first overload. For those, booleans are read as coordinates and the
counts are garbage.

**Fix:**
- List the real overloads first:
  `python3 workspace/horizon/tools/rea_vanilla.py method net.minecraft.server.level.ServerLevel sendParticles`
  (or `javap` on the 26.3 server classes). Write the exact descriptors down in a comment.
- Find out how the descriptor reaches this builtin: look at the enclosing function's signature
  (it gets `method_name`, and probably `descriptor` or `desc`). Match on it instead of `args.len()`.
- Map each overload's arguments by index:
  - `(T, DDD I DDD D)I`: `args[1]` type, `args[2..5]` x/y/z, `args[5]` count, `args[6..9]`
    spread, `args[9]` speed.
  - `(T, Z Z DDD I DDD D)I`: `args[1]` type, `args[2]`/`args[3]` booleans (ignore, or carry
    `alwaysShow` if `ParticleCall` grows a field; not needed), `args[4..7]` x/y/z, `args[7]`
    count, `args[8..11]` spread, `args[11]` speed.
  - The `ServerPlayer`-first overload sends to one player only. Record it the same way (the host
    shows particles to the local player anyway), with indices shifted by one. If its descriptor
    has the booleans too, shift by three.
- An unknown descriptor falls through (do not return `Handled`), so the call shows up as an
  unhandled method instead of being guessed.
- The return value is the count for the matched overload. Vanilla returns how many players were
  sent to; the current code returns the count. Keep returning the count and leave the comment
  that says so.

**Test:** add a unit test in `builtins.rs`'s test module only if that module can call the builtin
with native `Level` and `ParticleType` objects. Those are the runtime's own native types, not
fabricated Minecraft classes. Otherwise rely on the existing shower-head tick test (9-parameter
overload) and say in the commit that the 11-parameter overload has no fixture.

---

## Finding 3: item drops from block code must reach the host

**Where:** `FabricBridge::effects_of`, `bridge/blocks.rs` (~490-540); `BlockEffects` (`:349`);
`apply_block_effects`. The drops are recorded by `builtins.rs:5960-5982` (`popResource`) and
`:5983-6002` (`dropStacks`) into `level.dropped_stacks: Vec<(BlockPos, String, i32)>`.

**Problem:** `effects_of` never drains `dropped_stacks`, so a berry-bush-style harvest
(`popResource` then `setBlock(age=1)`) resets the age and gives nothing. The listener path
already applies drops (`sync_level_to_host`, `events.rs:7-20`, calls `host.drop_item`).

**Fix:**
- Add `pub drops: Vec<ItemDrop>` to `BlockEffects`, with
  `pub struct ItemDrop { pub pos: [i32; 3], pub item: String, pub count: u32 }`
  (`Debug, Clone, PartialEq`). Export it next to `BlockSchedule`/`ParticleBurst` from
  `bridge/mod.rs` the same way those are.
- In `effects_of`, `std::mem::take(&mut level.dropped_stacks)` together with the particles and
  schedules. Skip `count <= 0` and empty or `minecraft:air` items.
- In `apply_block_effects`, call `host.drop_item(drop.pos, &drop.item, drop.count)` for each one,
  **after** applying `changes`.
- `BlockEffects` derives `Default`, so the existing struct literals only need the new field. Grep
  for `BlockEffects {` in `crates/` and `workspace/horizon` and add it where needed.

**Test:** none of mdm's blocks pop resources as far as is known, so check with
`javap -c -p` on its block classes for `popResource`. If none do, a unit test of `effects_of`
draining a native `Level` with a `dropped_stacks` entry is acceptable, because `Level` is the
runtime's own type. Allocate it with `heap.alloc_level` on a bare interpreter if the test module
can build one.

---

## Finding 2: block removals from block code must reach the host

**Where:** same functions as finding 3. Removals are recorded by `builtins.rs:6145-6150`
(`removeBlock`/`destroyBlock` on a `Level`) via `Level::destroy_block` (`minecraft.rs:617-628`),
which pushes onto `level.broken`.

**Problem:** `effects_of` and `level_writes` (`place_builtins.rs:507`) never read `broken`, so the
removal is dropped while the click is still cancelled.

**There is a second bug in the same function:** `Level::destroy_block` only looks in
`self.blocks`, not `self.mod_states`. A mod block removing **itself or another mod block** returns
`false` and records nothing. Fix that too: if `mod_states` holds the position, remove it from
`mod_states`, insert air into `blocks`, push onto `broken` and return `true`. Then
`get_block_state` at that position answers air afterwards. Check `get_block_state` and the
`getBlockState` builtin to see how `mod_states` is consulted first.

**Fix:**
- Record whether the removal drops items. In the builtin at `builtins.rs:6145`, `removeBlock(pos, isMoving)`
  never drops, and `destroyBlock(pos, dropBlock)` (plus the overloads with an entity or a
  recursion limit) drops when `dropBlock` is true. Change `broken` to
  `Vec<(BlockPos, bool /* drop */)>`, or add a parallel field. Update the other readers of
  `broken`: grep `\.broken` in `crates/hfc-runtime/src`; `block_break_before` in `events.rs` uses
  it for `host.break_block`.
- Add `pub removals: Vec<BlockRemoval>` to `BlockEffects`, with
  `pub struct BlockRemoval { pub pos: [i32; 3], pub drop: bool }`. Drain it in `effects_of`.
- **Order matters.** A use can `setBlock` a position and later remove it, or the other way round.
  `level_writes` collapses `placed` to the last write per position. Before reporting, drop any
  `changes` entry whose position appears in `removals` **after** it, and drop any removal followed
  by a later `setBlock` at the same position. The simplest correct way is to add a monotonically
  increasing sequence number to each `placed` and `broken` entry when recording, compare, and keep
  only the last action per position. If that is too invasive, return `BlockUse::Failed("block code
  both set and removed <pos>")` for that case rather than guessing.
- Host hook: add to `HostBridge`
  ```rust
  /// Remove the block at `pos` as `Level.removeBlock`/`destroyBlock` would:
  /// set it to air, dropping its loot when `drop`. Hosts that cannot leave
  /// the default, which reports nothing was removed.
  fn remove_block(&mut self, pos: [i32; 3], drop: bool) -> bool { let _ = (pos, drop); false }
  ```
  In `apply_block_effects`, call it. When it returns `false`, `log::warn!` that the host refused.
- **Do not cancel what cannot be applied.** `block_used` runs `effects_of` before anything is
  applied. Add a `FabricBridge` capability check: if `effects.removals` is non-empty and the host
  cannot remove blocks, return `BlockUse::Failed("block code removed a block, which this host cannot
  do")`. One way: add `fn can_remove_blocks(&self) -> bool { false }` to `HostBridge`, `true` in the
  client adapter. This keeps the host's click path running instead of eating the click.
- Client side (`workspace/horizon/crates/horizon-client/src/game/fabric_bridge/`): implement
  `remove_block` (and `can_remove_blocks`) on `HostBridgeAdapter`. Find how `break_block` is
  implemented there and reuse the game's own block-removal path. `drop == true` should behave like
  vanilla `destroyBlock(pos, true)` (loot and the break event/particles); `drop == false` should
  only set air with update flags 3. Look for an existing `set_block`/`destroy_block` helper on
  `Game` in `game.rs` or `game/` before writing a new one, and make it go through the server
  level so LAN and integrated clients see it.

**Test:** a runtime unit test that a native `Level` with a mod-state position records the removal
(this is the `mod_states` bug). A real-classpath test only if a fixture mod's block removes
something.

---

## Finding 1: changes the block's use makes to the held item are lost (item duplication)

**Where:** `prepare_placement` (`place_builtins.rs:289-316`) builds
`ItemStack::new(&request.item, 1)`. It is allocated once as `stack_id` (passed to `useItemOn`)
and cloned again into `player.hand_item`. The `getItemInHand` builtin (`builtins.rs:6261-6268`)
allocates a **new clone** on each call. `place_builtins.rs:119` answers `getItemInHand` from the
`UseOnContext` with the context stack. Nothing reads the stack back after the call, and there is
no `HostBridge` method to change the hand.

**Problem:** A block that does `stack.shrink(1)`, `stack.consume(1, player)`,
`player.setItemInHand(hand, new ItemStack(Items.BUCKET))`, `ItemUtils.createFilledResult(...)` or
`player.addItem(...)` and returns `SUCCESS` gets its block-state change applied and the click
cancelled, but the player keeps the item. The action can be repeated without limit.

Do this in two stages. **Stage 1 alone is an acceptable stopping point.** It makes the bug
impossible, at the cost of not supporting those blocks yet.

### Stage 1: detect and refuse (runtime only)

- Make the hand item one identity. Store the heap id of the hand stack on the native `Player`, for
  example `pub hand_stack: Option<usize>` next to `hand_item` in `minecraft.rs:666`.
  `prepare_placement` sets it to `stack_id`. `getItemInHand` (builtins.rs) returns that id for the
  acting hand when set, falling back to today's behaviour otherwise. Check whether
  `getMainHandItem`, `getOffhandItem` and `getItemBySlot` builtins exist and route them the same
  way. Make sure `UseOnContext.getItemInHand` (`place_builtins.rs:119`) is the same id.
- Record hand writes: give `Player` a `hand_replaced: Option<usize>` that the `setItemInHand`
  builtin sets (add the builtin if missing; it must also update `hand_stack`), and an
  `inventory_added: Vec<usize>` for `addItem`/`getInventory().add`. Grep the builtins for
  `setItemInHand`, `addItem` and `getInventory` first. Some may already exist under a different
  receiver class (`Player`, `ServerPlayer`, `Inventory`).
- After the use methods return a consuming result, compare the final state with the initial
  `(item, count = 1)`. The hand counts as changed if the stack at `hand_stack` has a different item
  id or count (shrink/consume/split mutate it in place), if `hand_replaced` is set, or if
  `inventory_added` is non-empty. Find how `ItemStack` count is stored and mutated: grep
  `"shrink"`, `"setCount"`, `"split"`, `"consume"` across `crates/hfc-runtime/src`. If `shrink`
  is not a builtin, the real bytecode runs on the native stack and the `count` field must be
  read the same way. Make sure what you compare is what the bytecode writes.
- If anything changed: return `Err(failure("<block>'s use changed the held item (<what>), which the
  host cannot apply"))` from `Interpreter::block_used`. That becomes `BlockUse::Failed`, so the
  host keeps its own handling and nothing is half-applied. Log it once per block id (keep a
  `HashSet<String>` on the bridge) so a player clicking repeatedly does not flood the log.
- Unit test the comparison as a free function over `(initial_item, initial_count, final_item,
  final_count, replaced, added)`.

### Stage 2: apply it for real (runtime + host)

- Have the host report the real count: add `fn held_count(&self, off_hand: bool) -> Option<u32>`
  to `HostBridge` (default `None`). `prepare_placement` uses it instead of `1` when known. When
  unknown, keep `1` and treat any shrink as a delta.
- Add to `BlockEffects`: `pub hand: Option<HandChange>` with
  `enum HandChange { Shrunk(u32), Replaced { item: String, count: u32 } }` and
  `pub given: Vec<(String, u32)>`.
  Report a shrink as the delta (`initial - final`), not the absolute count, because the runtime's
  initial count may be 1 while the host's real stack has more.
- Add `fn set_hand_item(&mut self, off_hand: bool, change: &HandChange) -> bool` (default `false`)
  and `fn can_change_hand(&self) -> bool` (default `false`). Use `give` for `given`, since
  `HostBridge::give` already exists. Keep the stage-1 refusal for hosts where
  `can_change_hand()` is false.
- Creative mode: vanilla's `consume`/`shrink` call sites often skip the shrink when
  `player.getAbilities().instabuild` (`hasInfiniteMaterials`). Find what the runtime's `Player`
  answers for `getAbilities`/`hasInfiniteMaterials` and make sure it matches the host's game
  mode. Otherwise a creative player loses items they would not lose in vanilla.
- Client: implement both on `HostBridgeAdapter`, editing the acting hand's real stack through the
  same code the game's own `use_hand` uses to consume items, so LAN guests (`as_guest`) get it too.

**Test:** stage 1 needs a real mod block that consumes an item. Check with `javap -c -p` whether
any block class in the `test-mods/` jars calls `shrink`/`consume`/`setItemInHand` in
`useItemOn`. If one does, add a real-classpath test asserting `BlockUse::Failed` (stage 1), then
`Handled` with the right `HandChange` (stage 2). If none does, say so in the commit and rely on
the unit tests.

---

## Finding 6: `BlockHitResult` has no click location

**Where:** `minecraft::BlockHitResult` (`minecraft.rs:301-306`: only `pos` and `direction`);
`Heap::alloc_hit_result` (`minecraft.rs:763`); `prepare_placement` (`place_builtins.rs:348`:
`alloc_hit_result(hit_pos, face)`, then it separately builds a real `Vec3` from `request.hit` via
`VEC3.<init>(DDD)V`, ~`place_builtins.rs:356-367`, which only reaches the `UseOnContext`); the
`BlockHitResult` builtins in `builtins.rs:5731-5780` (`<init>`, `getBlockPos`, `getDirection`,
`withPosition`; no `getLocation`, no `isInside`). `<init>` ignores the `Vec3` argument too.

**Problem:** A block that reads `hit.getLocation()` (upper/lower slot, chiseled-bookshelf-style
`getRelativeHitCoordinatesForBlockFace`) gets `null` or zeros, so it acts on the wrong slot or
fails.

**Fix:**
- Add `pub location: Option<usize>` (heap id of a real `Vec3` object) and `pub inside: bool` to
  `BlockHitResult`. Storing the id of the `Vec3` that `prepare_placement` already builds with
  Minecraft's real constructor avoids fabricating a `Vec3`.
- In `prepare_placement`, build the `Vec3` **before** the hit result, then allocate the hit result
  with that location and `request.inside`. Keep `alloc_hit_result(pos, dir)` working for the
  other callers (`events.rs` step 2) by giving them `location: None`, or have them build a `Vec3`
  from `event.hit` the same way. For events.rs, prefer that, since the event carries `hit`.
- Builtins: add `getLocation` (return `Value::Object(location)`; when `None`, fall through instead
  of returning null) and `isInside`. **Dispatch on both class names.** Bytecode calls
  `getLocation` as `invokevirtual net/minecraft/world/phys/HitResult.getLocation`, the superclass,
  so the builtin must match `HitResult` when the receiver is a native `BlockHitResult`. Check how
  the dispatch keys on `class_name` here: it may be the call site's owner, not the object's
  class.
- `<init>` (`builtins.rs:5733`): capture an argument that is a `Vec3` instance (a
  `JavaObject::Instance` whose class is `net/minecraft/world/phys/Vec3`) into `location`, and a
  trailing boolean into `inside`. Check the 26.3 constructor descriptors with `rea_vanilla.py`.
  `withPosition`/`withDirection` must copy `location` and `inside`.
- Also check `getType` (`HitResult.Type.BLOCK`) and `isInside` are answered, since vanilla code
  around use often calls them.

**Test (`tests/registered_blocks.rs`):** with the shower head (or any mdm block whose use reads
the hit; check with `javap`), assert that use still works. A direct test can run
`execute_method("net/minecraft/world/phys/HitResult", "getLocation", "()Lnet/minecraft/world/phys/Vec3;", ..)`
on a hit result from `prepare_placement` and check the real `Vec3`'s `x/y/z` equal
`event.hit`. That uses real classes on the real classpath.

---

## Finding 10: the heap grows without limit while a block keeps ticking

**Where:** `Heap::alloc` (`crates/hfc-runtime/src/heap.rs:127-130`) only pushes, and nothing ever
frees a slot. Each `tick_block`/`block_used` allocates a `Level` snapshot with one `BlockState`
per non-air block in the 5x5x5 area (`snapshot_level`, `place_builtins.rs:265-286`), a
`BlockPos`, a `RandomSource` instance (`use_builtins.rs:186-189`), and whatever the mod's
bytecode allocates. A running shower head reschedules every 2 game ticks, which is 10 calls/s per
block. The reviewer estimates 0.2-1.4 GB/hour per running block.

**This is the hard one. Do it last, measure before and after, and do not break heap identity.**

**Approach: a per-call allocation scope with an escape check, truncating only when it is safe.**

1. **Warm everything that is cached across calls before the scope opens.** These are allocated
   lazily the first time and must survive:
   - `mod_state_objects(id)` (state objects cached in `heap.block_state_objects`). `block_used` and
     `block_tick` call it before `snapshot_level`, but `snapshot_level` calls it for every
     **neighbouring** mod block too. Pre-warm for every registered id that appears in the
     `WorldSnapshot` before opening the scope.
   - `ensure_initialized(DIRECTION)`, `ensure_initialized(VEC3)` and any class the mod's code
     initializes for the first time. Class initialization stores objects in statics, so you
     cannot pre-warm it in general. Handle it with the escape check below.
   - interned strings, the synthetic statics (`has_synthetic_statics` / `create_static_field_val`),
     and the `InteractionHand`/`Direction` constants.
2. **Open the scope:** `let mark = heap.objects.len();` and set `heap.scope = Some(mark)`.
3. **Escape tracking (write barrier):** while a scope is open, any store of a reference `>= mark`
   into an object `< mark` (putfield, array store, `aastore`), into a static field (putstatic,
   `static_fields` map) or into any Rust-side cache on `Heap`/`Interpreter` (`block_state_objects`,
   intern tables, `registered_blocks`, the class-init table) sets `heap.escaped = true`. Find
   every such store site: grep `Putfield`, `Putstatic`, `Aastore` in `interp.rs`, and every
   `heap.get_mut` in the builtins that writes a `Value`. Missing one is a use-after-free bug
   (a stale id pointing at a reused slot), so be thorough. When unsure about a site, set `escaped`
   conservatively.
   - Any class initialized during the scope must also set `escaped`. Find where `<clinit>` runs
     (`ensure_initialized`) and set it there.
4. **Close the scope** after `effects_of` has turned everything into plain Rust data
   (`BlockEffects` has no heap ids; check that holds after your other fixes). If `!escaped`,
   `heap.objects.truncate(mark)`. If `escaped`, keep everything and `log::debug!` once per block id
   that its call escaped. The first tick after a class init will escape; the later ticks should
   not.
5. Always reset `scope`/`escaped`, including on the error path. Use a guard struct with `Drop`, or
   close the scope in both match arms of `block_used`/`tick_block`.
6. Apply it to `tick_block` first (the unbounded case). Apply it to `block_used` too only if it
   comes for free. Clicks are human-paced, so they matter much less.

**Why not cheaper options:**
- Only freeing the `Level` slot (`objects[level] = None`) leaves the `BlockState`s inside it
  owned by the `Level` (they are Rust values in its `HashMap`, so they do go away). It also leaves
  the `BlockPos`, `RandomSource` and mod allocations. It is a reasonable **first step** if the
  scope work is too big: set the snapshot `Level` slot and the per-call `BlockPos`/`RandomSource`
  slots to `None` after `effects_of`, but only if nothing could hold them. The mod's code could
  have stored `level` in a static field. Even this partial fix needs the escape check for those
  ids.
- Reusing one `RandomSource` across ticks is fine. Allocate it once in a `OnceCell`-like field on
  the bridge outside any scope.

**Test:**
- A runtime unit test: open a scope, allocate, close, assert `objects.len()` is back to `mark`.
  Store a new object into a pre-existing instance's field, close, and assert nothing was
  truncated.
- Real classpath (`registered_blocks.rs`): run `tick_block` on the running shower head 1,000 times
  and assert `heap` object count after the run is at most the count after the 2nd run plus a small
  constant. Expose a `#[doc(hidden)] pub fn heap_len(&self)` on `FabricBridge` if no accessor
  exists. Keep the existing assertions on particles and the reschedule on every tick. This also
  catches a use-after-free that would break later ticks.
- Record the measured heap growth before and after in `docs/architecture/fabric-runtime.md`
  (C3 section).

---

## Docs to update when you are done

- `docs/architecture/fabric-runtime.md`, the stage C3 section: what a block's use and tick code can
  now change on the host (states, schedules, particles, drops, removals, hand item if stage 2 is
  done), what is refused with `Failed` (hand-item changes on hosts without the hook, anything
  else not carried), the sneak and PASS rules, and the heap measurement.
- `README.md` and `workspace/horizon/docs/mods.md` if they list what C3 reproduces. Do not claim
  more than the tests show (root `AGENTS.md`, "Never fake compatibility").
- Update [blocks-c3-custom-use.md](blocks-c3-custom-use.md)'s "Where the work stands" table with
  the new commit hashes.

## Checklist

- [ ] 4 sneak suppression + unit test + real-classpath test
- [ ] 5 empty-hand fallthrough only on `TRY_WITH_EMPTY_HAND`; `Unknown` -> `Failed`; unit test
- [ ] 7 acting hand passed to `UseBlockCallback` and JamLib listeners
- [ ] 9 cache hit requires identical registered id list; test in `block_facts_cache.rs`
- [ ] 8 `sendParticles` matched by descriptor, all 26.3 overloads mapped
- [ ] 3 `drops` in `BlockEffects`, applied via `drop_item`
- [ ] 2 `removals` in `BlockEffects` (+ drop flag, ordering), `mod_states` bug in `destroy_block`,
      `remove_block` host hook, `Failed` when the host cannot remove
- [ ] 1 stage 1: hand-item change detected -> `Failed`; (stage 2: applied through host)
- [ ] 6 hit location stored and answered via `HitResult.getLocation` / `isInside`
- [ ] 10 per-call heap scope with escape barrier on `tick_block`; growth test; measurement in docs
- [ ] `cargo fmt --check -p hfc-runtime`, the touched test files pass, client builds if touched
- [ ] Docs updated; commits per fix with the agent's `Co-Authored-By:` footer
