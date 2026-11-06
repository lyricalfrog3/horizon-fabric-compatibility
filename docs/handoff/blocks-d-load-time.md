# Handoff: blocks stage D — load time

You are continuing code-registered block support in **Horizon Fabric Compatibility** at
`/home/daniel/Desktop/horizon-rust-fabric`. Read this whole file, then the root `AGENTS.md`,
`workspace/horizon/AGENTS.md`, and in
[`docs/architecture/fabric-runtime.md`](../architecture/fabric-runtime.md) the entrypoint budget
table and the "Registered blocks" / "Blocks in the client (stage B: decoration)" sections.

The custom-behaviour work has its own handoff: [blocks-c3-custom-use.md](blocks-c3-custom-use.md).
Read its "Where the work stands" for what stages A–C2 built; the commits are the same
(root `main` `4fd9d8c`, client fork `hfc` `464372de1`).

**Where step 2 stands now** (the rest of this file is the original plan): the shape pass is
memoized per `VoxelShape` object, bounded per block (not per state) and reads one state for a
block whose classes do not replace `getShape`/`getCollisionShape`; Farmer's Delight's 2,580
states went from 0.64 s to 0.25 s and every state now has a shape. The null shapes it used to
read were one bug: the JDK's `JavaLangAccess` (which `EnumMap`/`EnumSet` read enum constants
through) is now installed when `SharedSecrets` initializes, so Minecraft's `Direction`-keyed
shape maps answer again. mdm is 13.0 s (from 13.8 s); what remains is its own `Shapes.join`
calls inside `electric_guitar_*` (~39M instructions each), which needs a native fast path or a
lazier shape read, not another cache. Items 1 (the cache) and 3 (duplicate work) are still
open.

## The problem

With Modern Decorations (`test-mods/mdm-3.1-fabric-26.3.jar`, 344 blocks) installed, startup is
about **20 s slower**. Measured (release build of the runtime; debug is about 9x slower):

| Phase | Cost | Where |
|---|---|---|
| `MdmMod.onInitialize` | 46.1M instructions, ~5.3 s | entrypoint; mostly building `StateDefinition`s. `mdm:furniture_part` has 2,916 states and each of the three `electric_guitar_*` blocks 3,468 |
| Whole `init()` incl. facts for 15,360 states | ~5.8 s | `FabricBridge::init` → `collect_block_facts` (`crates/hfc-runtime/src/block_builtins.rs`) |
| Reading every block's shapes | ~168M instructions, ~13 s live (13.8 s before the memo, 13.0 s after) | `FabricBridge::read_block_shapes` (`bridge/blocks.rs`), called once behind a `Once` in `workspace/horizon/crates/horizon-client/src/game/fabric_bridge/content.rs` `registered_blocks`. Each `electric_guitar_*` ~40M, white bathtub 11.4M, black modern fridge 7.5M, most blocks under 1M. `Shapes.or` over many fine boxes dominates |
| Writing the 344 declarations | ~150 ms | `content/blocks.rs` `declare` |

Also suspected, not measured: `content_root` is called twice per start, so the export may run
twice (only one build was logged in one run); `Vanilla::load` in `content/blocks.rs` parses
gameplay-facts again. `ENTRYPOINT_BUDGET` was raised from 10M to 100M for this mod, plus
`BLOCK_SHAPE_BUDGET` 100M per block (`crates/hfc-runtime/src/bridge/mod.rs`).

## What D should do

In order of payoff:

1. **Cache the block facts and the generated declarations per jar set.** Key: the hashes of the
   loaded jars (all of them; a library change can change a mod's facts), the runtime/HFC version
   and Horizon's content format version. Store under the game's `.horizon-cache/` (next to
   `fabric-content/`), written atomically (see `hfc-core`'s atomic writes). On a hit, skip
   `collect_block_facts` and `read_block_shapes` and reuse the declarations. Mind what still
   needs live objects: C1 placement and C2 container reads call the mod's code at play time, so
   the interpreter's registered block objects must still exist. Measure whether `onInitialize`
   itself (the 5.3 s) must still run on a cache hit; if it must, say so plainly and focus on the
   shapes.
2. **Make shape reading cheaper where it's spent**: profile `Shapes.or` / `VoxelShape` paths in
   the interpreter (likely `BitSet`/`DiscreteVoxelShape` loops and collection copies) and add a
   builtin only at a real hot spot, keeping results identical (assert equal boxes before/after on
   the mdm blocks). Shapes could also be read lazily per block on first need, if Horizon's content
   loader allows declaring a block before its shape is known; it probably doesn't, since shapes
   go into the content root's tables.
3. **Remove duplicate work**: confirm whether the export runs twice per start and whether
   gameplay-facts is parsed twice; fix both if so.
4. **Keep the game responsive**: the work must not stall the render thread; log a progress line
   during a cold start so a long first launch doesn't look frozen.

Record before/after numbers (cold start, warm start, with and without mdm) in
`fabric-runtime.md`'s budget table, as `AGENTS.md` requires when budgets change, and update the
README's "Known gaps" line about the ~20 s startup.

## Rules that bite here

- **Correctness over speed.** A cache must never serve facts for a different jar set; any doubt is
  a miss. Assert that cached and fresh facts are identical in a test.
- **No mod-specific code** (no branching on mod ids, class or jar names); **never fake**
  compatibility; **don't weaken tests**.
- **Tests:** never run a full suite (it froze the user's PC). One file or filter at a time with
  `nice -n 19 … -j2 … --test-threads=1`, e.g.
  `nice -n 19 cargo test -j2 --release -p hfc-runtime --test registered_blocks -- --test-threads=1`.
  Client tests from `workspace/horizon`, filtered (`--lib fabric_bridge::content`).
- **Don't touch `Cargo.toml`/`Cargo.lock`.** Don't commit unless the user says so.
- **Never list, read or copy anything in `~/Downloads`** or other personal folders; test jars are
  in `test-mods/`. **Never write** to `~/.local/share/horizon*` or `~/.local/bin`.
- Cheap Agent (cheapag) **cannot build this repo** (empty submodule in its worktrees, no access to
  the Minecraft classes, so runtime tests skip and look green). Use your own tools or a Sonnet
  subagent.
- Commits: client changes inside `workspace/horizon`, pushed with `git push fork HEAD:hfc` (never
  `origin`); then the submodule pointer at the root. Root commits end with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Measuring live

Build: `cd workspace/horizon && nice -n 19 cargo build -j2 --profile fast -p horizon-client --no-default-features`.

```sh
T=$(mktemp -d /tmp/hfcd.XXXX); mkdir -p $T/game/horizon-mods
cp test-mods/fabric-api-0.165.0+26.3.jar test-mods/mdm-3.1-fabric-26.3.jar $T/game/horizon-mods/
export HORIZON_HOME=$T/home XDG_DATA_HOME=$T/xd XDG_CONFIG_HOME=$T/xc HORIZON_NO_AUDIO=1 HORIZON_LAUNCHER_NO_SELF_UPDATE=1
C="workspace/horizon/target/fast/horizon-ctl --socket $T/c.sock"
time $C launch --world W --seed 42 --creative --wait --offscreen \
  --resources /home/daniel/.local/share/horizon-launcher/versions/nightly-2026-11-01-c9d474d/.local/horizon-client/26.3 \
  --game-dir $T/game --home $T/home      # cold
$C quit
time $C launch …same arguments…        # warm: the cache should hit
$C quit; rm -rf $T
```

The game log is `$T/c.log`; the bridge logs the shape-reading time and slow entrypoints. Check
that blocks still place, face, store and drop after a warm start (give a cabinet, place it,
`interact`).

## Done means

A warm start with mdm is within a few seconds of a start without it, a cold start is no slower
than today, cached and fresh facts are proven identical by a test, the numbers are recorded in
`fabric-runtime.md`, and placement and containers still work after a warm start.
