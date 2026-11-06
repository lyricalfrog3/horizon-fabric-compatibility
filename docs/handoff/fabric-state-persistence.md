# Handoff: keep Fabric block state across a world reload

You are making state that Fabric mod code creates during play survive saving and reloading the
world. The main gap is scheduled block ticks. The work is in **Horizon Fabric Compatibility** at
`/home/daniel/Desktop/horizon-rust-fabric`, and most of it is in the client submodule
`workspace/horizon`. Read the root `AGENTS.md`, `workspace/horizon/AGENTS.md` and
`workspace/horizon/CLAUDE.md`, and the "Scheduled block ticks (stage C3, ticking)" section of
[`docs/architecture/fabric-runtime.md`](../architecture/fabric-runtime.md).

## Why

[`fabric-runtime.md`](../architecture/fabric-runtime.md) says it plainly: "The schedule lives in
memory: it is not saved with the world, so a block that ticks itself (a running shower) stops
when the world is reloaded. Vanilla saves its scheduled ticks." Any mod block that drives itself
with `Level.scheduleTick` (timers, pulsing blocks, growth, machines) silently stops on reload,
and only a click restarts it. The block state says "running" while nothing runs.

## Where things are today

- The runtime asks the host to schedule: `HostBridge::schedule_block_tick(pos, block, delay)`
  (`crates/hfc-runtime/src/bridge/mod.rs`).
- The client keeps its own in-memory list:
  `fabric_block_ticks: Vec<(u64 /* due game tick */, String /* dimension */, [i32; 3], String /* block id */)>`
  (`workspace/horizon/crates/horizon-client/src/game.rs` ~2896, initialized ~18817).
  `HostBridgeAdapter::schedule_block_tick`
  (`workspace/horizon/crates/horizon-client/src/game/fabric_bridge/mod.rs` ~371) pushes onto it,
  capped at 4096 entries with no deduplication. `Game::tick_fabric_blocks` (same file, ~550)
  runs up to 64 due entries per game tick, skips entries whose block changed, and calls
  `FabricBridge::tick_block` and `apply_block_effects`.
- Vanilla scheduled ticks already have a full, saved implementation in the client:
  - per-dimension `block_ticks: Ticks` (`game/levels.rs` ~43, ~190, swapped per dimension ~246),
    defined in `crates/horizon-world/src/level/ticks.rs`;
  - `Game::native_schedule_block_tick(ticks, pos, block, delay)` (`game/native_hooks.rs` ~7516),
    which native (Rust) mods already use: `registry.default_state(block)` →
    `behaviours.block(state)` → `ticks.schedule_block(time, at, block, delay, TickPriority::Normal)`.
    It deduplicates like vanilla ("a tick of that block already queued there is kept");
  - saving: `persist.rs` `attach_save_data` (~175) packs `block_ticks` into the chunk's extras,
    `save.rs` (~1560) writes the vanilla `block_ticks` NBT list, `save.rs` (~1769) reads it back
    through the block-id table, and `persist.rs` (~230-262) re-registers the saved ticks when a
    chunk loads;
  - running: when a vanilla tick is due, the game calls the `before_tick_block` hook
    (`game.rs` ~1338). That hook already lets native mods handle a tick for their blocks
    (`native_tick_block(.., handled)`).

## The fix: put Fabric ticks on the vanilla scheduler

Do not invent a second save format. Route Fabric block ticks through the vanilla `Ticks`
scheduler, which already saves, loads, handles unloaded chunks, follows the dimension and
deduplicates. Then delete the parallel list.

Steps (each one compiles on its own; commit in the submodule):

1. **Confirm Fabric blocks are registry blocks.** Blocks a Fabric mod registers in code are
   loaded as content-mod blocks (stage B, `fabric_bridge/content.rs`). Check that
   `registry.default_state("mdm:shower_head")` (or any registered Fabric block id) resolves and
   that `behaviours.block(state)` returns a behaviour id usable in `Ticks`. If content blocks
   share one generic behaviour, find what `Ticks` stores per entry (a `Block` id from the
   registry, or a behaviour). The tick must identify **the block id**, because that is what
   `tick_block` needs and what the saved NBT `i` field holds. If the stored key loses the block
   id, write down what you found and choose the smallest change that keeps it (for example
   storing the registry block index).
2. **Schedule through it.** In `HostBridgeAdapter::schedule_block_tick`, call the same path as
   `native_schedule_block_tick` for the current level's `block_ticks`, with
   `delay.max(1) as i32`. Find how the adapter reaches the current dimension's `Ticks` from
   `Game`: it is the `ticks: &mut Ticks` the native hooks receive, or `self.block_ticks` on the
   level being served. Read `game/levels.rs` to see which one is live while the server thread
   ticks. If it reports `MissingChunk`, log at debug and drop it, as vanilla does.
3. **Run it from the vanilla tick.** In `before_tick_block` (or wherever `native_tick_block` is
   called for a due tick), when the state's block is a Fabric-registered block (ask the bridge:
   `FabricBridge` has the registered block ids via `registered_blocks()` or the facts list; use a
   cached `HashSet` of ids built at content load, not a scan per tick), call
   `FabricBridge::tick_block(&adapter, pos, &block_id)`, apply the effects, and set
   `*handled = true`. Keep the existing order relative to native mods: native-mod hook first, then
   Fabric, then vanilla behaviour. Keep "only on the server thread / dedicated"
   (`self.serving.is_some() || self.options.dedicated.is_some()`), as `tick_fabric_blocks` does
   now.
4. **Delete the old path:** the `fabric_block_ticks` field, its initializer, the 4096 cap, and
   `tick_fabric_blocks` plus its call site. Grep for each name to be sure nothing else uses them.
   The vanilla scheduler's own per-tick limit (`Ticks` caps how many run per tick, 65536 in
   vanilla) replaces the 64/tick limit. Check that `tick_block`'s budget (`TICK_BUDGET`, 2M
   instructions, about 1 ms per shower tick) is acceptable at that rate, and if not, add a
   per-game-tick time budget for Fabric ticks that defers the rest to the next tick (vanilla
   does not defer, so document it if you add it).
5. **Mod-block-entity data: verify only.** `fabric-runtime.md` notes mdm's block entities store
   nothing, and C2 container blocks use the host's own container block entities, which the host
   already saves. Check that a Fabric container block's items survive a reload in a live run. If
   they do not, that is a separate bug: write it up in this file's "Found" section and do not
   fix it here.
6. **Config files: verify only.** The runtime's VFS writes to disk under the game dir
   (`crates/hfc-runtime/src/vfs.rs` `write`, with the game dir set in
   `fabric_bridge/mod.rs` ~116). It truncates and rewrites in place, which is not crash-safe.
   Change truncating writes (`append == false`) to go through `hfc_core`'s atomic write (used by
   `bridge/block_cache.rs` as `atomic_write`), and keep appends as they are. Check that the
   existing `crates/hfc-runtime/tests/config_files.rs` still passes.

## Tests

- **Runtime:** nothing changes in `hfc-runtime` except the VFS atomic write, which
  `config_files.rs` covers. Run `cargo test -p hfc-runtime --test config_files`.
- **Client unit test:** find the existing tests for `Ticks` packing and loading
  (`horizon-world/src/level/ticks.rs`, `save.rs` tests) and add one that a tick scheduled for a
  **content block id** round-trips through `pack` → `save.rs` NBT → `read_ticks` → `from_saved`
  with the same id, position and remaining delay.
- **Live test (required before claiming it works).** Follow the scratch-run rules in
  `workspace/horizon/CLAUDE.md`: fresh `mktemp -d` for `HORIZON_HOME`,
  `HORIZON_LAUNCHER_HOME`, `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, plus
  `HORIZON_LAUNCHER_NO_SELF_UPDATE=1` and `HORIZON_NO_AUDIO=1`. Never touch the user's real
  worlds. With Modern Decorations from `test-mods/` installed, use `horizon-ctl`: place a shower
  head, right-click it on, check that particles appear (Particles option *All*), save and quit,
  relaunch the same scratch world, and check that the shower still emits particles **without a
  click**. Capture the `horizon-ctl` commands and screenshots or log lines as evidence in the
  commit message or PR.
- `cd workspace/horizon && cargo build --profile fast -p horizon-client --no-default-features`
  and the touched crates' tests (`python3 tools/pr_ready.py --quick` if the vanilla data is
  present).

## Docs

- `docs/architecture/fabric-runtime.md`: replace the "The schedule lives in memory..." paragraph
  with how it works now (saved in the chunk's vanilla `block_ticks`, deduplicated per position and
  block, run from the vanilla tick hook) and the measured cost.
- `workspace/horizon/docs/mods.md`, if it repeats the limitation.

## Git

Client changes: commit inside `workspace/horizon`, push to remote `fork` branch `hfc`
(`git@github.com:lyricalfrog3/horizon.git`), then commit the submodule pointer plus the root doc
and VFS changes at the root. Never push to upstream `horizon-rust/horizon`. Follow the
attribution rules in each repository's `AGENTS.md`.

## Found

(Write anything you found here that is out of scope: container contents not surviving a reload,
other mod state that is lost.)
