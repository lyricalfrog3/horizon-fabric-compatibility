# Handoff: blocks stage C3 — custom block behaviour

You are continuing code-registered block support in **Horizon Fabric Compatibility** at
`/home/daniel/Desktop/horizon-rust-fabric`. Read this whole file, then the root `AGENTS.md`,
`workspace/horizon/AGENTS.md`, and these sections of
[`docs/architecture/fabric-runtime.md`](../architecture/fabric-runtime.md): "Registered blocks",
"Blocks in the client (stage B: decoration)", "Placing a registered block (stage C1)" and
"Containers (stage C2)".

Stage D (load time) has its own handoff: [blocks-d-load-time.md](blocks-d-load-time.md). The two
touch different code; if both run at once, coordinate on `bridge/blocks.rs` and
`fabric_bridge/content.rs`.

## Where the work stands

Everything below is committed and pushed, and the user verified placing, facing and storage
live with Modern Decorations.

| Repository | Branch | Commit |
|---|---|---|
| Root (`lyricalfrog3/horizon-fabric-compatibility`) | `main` | `ed2018c` "Free what a block tick allocates, with an escape check" (C3 and the review fixes: `33d06b5`, `63415a5`, `2ab6ef3`, `e4d43d6`, `05a4527`, `9eccad6`, `6583c67`, `477cda4`, `b316f30`, `ed2018c`) |
| Client submodule `workspace/horizon` (`lyricalfrog3/horizon`, remote `fork`) | `hfc` | `5398c852e` "Remove blocks for Fabric block code" (C3 and the review fixes: `b905398a6`, `5398c852e`) |

What exists:

- **Stage A, facts.** `FabricBridge::registered_blocks()` (`crates/hfc-runtime/src/bridge/blocks.rs`,
  `src/block_builtins.rs`) lists every block a mod registers with facts read by running its real
  code: the class chain, `overrides` (the vanilla methods the mod overrides, `"<class>.<method>"`),
  `has_block_entity`, state properties and default, per-state hardness/light/sound/map colour,
  outline and collision shapes (`read_block_shapes`), its BlockItem and `item_components`.
- **Stage B, decoration.** The client (`workspace/horizon/crates/horizon-client/src/game/fabric_bridge/content.rs`
  and `content/blocks.rs`) writes a Horizon block declaration per block in the generated content
  mod: a vanilla template of the same behaviour (a plain block when the mod overrides that
  behaviour), `state_properties`, `default_state`, `shapes` + `collision`, `can_occlude`.
- **Stage C1, placement.** `FabricBridge::state_for_placement` and `block_placed`
  (`src/place_builtins.rs`) run the mod's `getStateForPlacement`, `setPlacedBy` and `onPlace`
  over a snapshot of the host's real blocks (radius 2). Client hooks: `fabric_state_for_placement`
  (from `native_state_for_placement` in `game/native_hooks.rs`) and `fabric_block_placed` (after
  `native_place_block` in `game.rs`).
- **Stage C2, containers.** `src/container_builtins.rs` runs the real `newBlockEntity`,
  `getContainerSize`, `getDisplayName`, `getMaxStackSize` and `getSlotsForFace`;
  `RegisteredBlock::container` is
  `Container{slots, menu_type, title, title_key, max_stack_size, faces, notes}`,
  `NotVanillaContainer(class)`, `Unreadable` or `NoBlockEntity`. Horizon content blocks gained a
  `container {slots, menu, title|title_key, max_stack_size?, faces?}` key
  (`horizon-mods/src/content.rs`, documented in `workspace/horizon/docs/mods.md`), backed by
  `Class::ModContainer` in `horizon-world` and `register_block_containers` in
  `horizon-client-render/src/containers.rs`.
- **Stage C3, use.** `use_builtins.rs` runs `useItemOn`/`useWithoutItem` with vanilla's sneak and
  `TRY_WITH_EMPTY_HAND` rules; `BlockEffects` carries the property writes, drops, removals,
  scheduled ticks and particles it produced, and `HostBridge` applies them. A use that changes
  the held stack is refused. `BlockHitResult` carries the real click `Vec3`.
- **Stage C3, ticks.** `Level.scheduleTick` is handed to the host, which runs the block's real
  `tick` per dimension with a 2M budget, at most 64 per game tick, in a heap scope that frees
  what nothing kept (an escape scan covers statics, caches and older objects).
- **Stage D, load time.** `bridge/block_cache.rs` caches block facts and shapes under the game
  folder's `.horizon-cache/fabric-blocks`, keyed on every loaded jar's SHA-256, the JDK image and
  versions, and checked against the ids this start registered. Measured: cold 23.6 s, warm 8.9 s,
  no block mod 2.6 s.

Modern Decorations (`test-mods/mdm-3.1-fabric-26.3.jar`, 344 blocks) today: 130 containers
(`generic_9x3`, 27 slots), 211 blocks with `DecorationPlacementBlockEntity`, 2 with
`WallClockBlockEntity`, 1 (`mdm:furniture_part`) with no block entity.

## What C3 should do

The goal is that a mod block **behaves** as its own code says, in the general case, not only as
a container or decoration. In priority order, each one a shippable step with tests:

1. **Honour container rules from the mod's real code** (small, visible, the user noticed the
   gap): the container's `getMaxStackSize()` (the mdm cabinet's is **8**; Horizon allows 64 today),
   `canPlaceItem(slot, stack)`, and for `WorldlyContainer` the per-face `getSlotsForFace`,
   `canPlaceItemThroughFace`, `canTakeItemThroughFace` (hoppers). Read what can be read as facts
   (a fixed max stack size) and export it in the `container` declaration; rules that depend on the
   stack need a runtime call or must be reported as not reproduced. Extend Horizon's `container`
   declaration in a documented, general way. Make the title translatable if the mod's
   `getDisplayName` is a `Component.translatable` (today it is resolved to literal text).
2. **Run the mod's `useWithoutItem` / `useItemOn`** for blocks that override them and are not
   plain containers: toggles such as a lamp's `lit`, a block that cycles a property, a block that
   opens a vanilla menu. The right-click path is `right_click_block` in
   `crates/hfc-runtime/src/bridge/events.rs` (it already dispatches `UseBlockCallback` with a
   stand-in `Level`/`Player` and syncs changes back with `sync_level_to_host`). Use the same
   snapshot-of-real-blocks approach C1 uses in `place_builtins.rs`, call the block's own method
   only when `overrides` names it, map the returned `InteractionResult` (vanilla 26.3's real
   records: `InteractionResult$Pass`, `$Success`, `$Fail`, `$TryEmptyHandAndPass`; see
   `interaction_result_passes` in `events.rs`) and apply the block changes it made. A container
   block whose `useWithoutItem` only calls `openMenu` should keep using Horizon's container path.
3. **Block-entity data that isn't a container**: mdm's `DecorationPlacementBlockEntity` and
   `WallClockBlockEntity`. Find what they store (javap) and whether the data matters to the player
   (placement offsets, rendered state). Save/load through Horizon's block-entity NBT if it does;
   otherwise report it.
4. **Ticking** (`getTicker` / `BlockEntityTicker`) and **neighbour updates**
   (`updateShape`, `neighborChanged`): only if a test mod needs them; measure the per-tick cost
   first, because every call goes through the interpreter.

Also left from earlier stages (fix while in the area):

- C1 always passes the main hand; the hook doesn't carry the hand. World border is treated as
  unlimited and build height as the overworld's (-64..320).
- Block-entity data from the placed item's stack in `setPlacedBy` is not applied.
- `onPlace` is wired but untested (no mdm block uses it).
- The runtime still logs "item … extends BlockItem … not exported" for BlockItems, although stage
  B exports them as the block's item; fix the message.
- Beds whose class keeps vanilla `BedBlock` behaviour use a plain template when the mod overrides
  bed methods, so they can't be slept in.
- `/loot … loot <mod table>` and `/recipe give … <mod recipe>` fail at parse time (Horizon's
  resource arguments check against vanilla's command-registries list), and `/loot … mine` forces
  the `minecraft:` namespace (`terrain_cmds.rs` ~576). Horizon command bugs, not HFC.

## Rules that bite here

- **No mod-specific code.** Never branch on a mod id, namespace, class name, jar name or version.
  Fix the general path (a builtin, an event, a declaration form).
- **Never fake compatibility.** A behaviour you can't reproduce is reported (log + docs), not
  approximated silently. Never lose or duplicate a player's items.
- **Tests:** never run a full suite (it froze the user's PC). One file or a filter at a time:
  `nice -n 19 cargo test -j2 --release -p hfc-runtime --test registered_blocks -- --test-threads=1`
  (release: the debug profile takes about a minute for the mdm init). Client tests from
  `workspace/horizon`, filtered, e.g. `nice -n 19 cargo test -j2 -p horizon-client --no-default-features --lib fabric_bridge::content -- --test-threads=1`.
  Expected values come from `javap` of the mod's classes or vanilla's formulas, never from the
  code under test. javap (read-only):
  `/home/daniel/.local/share/horizon-launcher/minecraft/runtime/java-runtime-epsilon/linux/java-runtime-epsilon/bin/javap`.
- **Don't touch `Cargo.toml`/`Cargo.lock`.** Don't commit unless the user says so.
- **Never list, read or copy anything in `~/Downloads`** or other personal folders. Test jars are
  in `test-mods/` (git-ignored); ask the user for any other.
- **Never write** to `~/.local/share/horizon*` or `~/.local/bin`.
- Cheap Agent (cheapag) **cannot build this repo**: its worktrees have an empty
  `workspace/horizon` submodule and its sandbox can't read the Minecraft classes, so runtime tests
  would skip and look green. Use your own tools or a Sonnet subagent.
- Commits: client changes in `workspace/horizon`, pushed with `git push fork HEAD:hfc` (never to
  `origin`, which is upstream Horizon); then commit the submodule pointer at the root. Root
  commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Live check

Build: `cd workspace/horizon && nice -n 19 cargo build -j2 --profile fast -p horizon-client --no-default-features`.

Scratch game (the socket path must be short):

```sh
T=$(mktemp -d /tmp/hfc3.XXXX); mkdir -p $T/game/horizon-mods
cp test-mods/fabric-api-0.165.0+26.3.jar test-mods/mdm-3.1-fabric-26.3.jar $T/game/horizon-mods/
export HORIZON_HOME=$T/home XDG_DATA_HOME=$T/xd XDG_CONFIG_HOME=$T/xc HORIZON_NO_AUDIO=1 HORIZON_LAUNCHER_NO_SELF_UPDATE=1
C="workspace/horizon/target/fast/horizon-ctl --socket $T/c.sock"
$C launch --world W --seed 42 --creative --wait --offscreen \
  --resources /home/daniel/.local/share/horizon-launcher/versions/nightly-2026-11-01-c9d474d/.local/horizon-client/26.3 \
  --game-dir $T/game --home $T/home
# give, look-at-block, use-item / interact, block X Y Z, click-slot, screenshot, save, quit
$C quit; rm -rf $T
```

Saves live under `$T/game`, so use a fresh dir per run, or earlier blocks will be in the way.
The user tests in their own game at `~/horizon-fabric-test/` (`run.sh`); don't write there unless
asked.

## Done means

Each step: the behaviour runs from the mod's real code, a test pins it with independently
derived values, it works in a scratch game, and `fabric-runtime.md`, the README "Known gaps" and
(for any new declaration key) `workspace/horizon/docs/mods.md` say what is and isn't reproduced.
