# Experimental Fabric runtime (`hfc-runtime`)

**Status: experimental, not part of the HFC content pipeline.** This runtime lives in
`crates/hfc-runtime` and is wired into the Horizon client under `workspace/horizon`. It is
separate from the host-side HFC installer described in the [README](../../README.md): HFC
still converts content jars without running their Java code; this runtime is an in-client
interpreter for selected Fabric entrypoints.

It does not claim general Fabric compatibility. A mod initializing is not evidence that its
gameplay, screens, rendering or networking work.

## What runs where

| Piece | Path | Role |
|---|---|---|
| Interpreter and class loader | `crates/hfc-runtime/src/interp.rs`, `loader.rs`, `class.rs`, `jimage.rs` | Parses class files, interprets bytecode, loads the JDK image and Minecraft jars on demand |
| Fabric bridge | `crates/hfc-runtime/src/bridge.rs` | Reads `fabric.mod.json`, runs entrypoints, dispatches events, owns the instruction budget |
| Runtime builtins | `crates/hfc-runtime/src/builtins.rs` | Java/JDK, Fabric API and Minecraft shims |
| Client adapter | `workspace/horizon/crates/horizon-client/src/game/fabric_bridge.rs` | Loads jars, supplies the real Minecraft classpath, bridges host blocks/items |
| Client hooks | `.../game/native_hooks.rs`, `.../game/stations.rs` | Right-click dispatch, tooltip text and tooltip colors |

The interpreter is **not a JVM and not a sandbox**. It executes mod bytecode in the client
process with a custom heap and builtins. Only run mods you trust.

## Entrypoint instruction budget

`FabricBridge::init` runs `onInitialize` entrypoints first, then `onInitializeClient`. Each
entrypoint gets `ENTRYPOINT_BUDGET` instructions (currently **10,000,000**).

A JVM has no instruction cap. The cap exists only to stop a mis-decoded branch from hanging
the render thread. The previous 200,000 cap aborted legitimate Fabric API initialization and
left mods half-registered. Measured with the real Minecraft jar, libraries and JDK image:

| Entrypoint | Approximate cost | 200k | 10M |
|---|---:|---|---|
| `ModMenu.onInitializeClient` | ~367k | fail | pass |
| Fabric `RecipeSyncImpl.onInitialize` | ~590k | fail | pass |
| Fabric `CustomIngredientInit.onInitialize` | ~575k | fail | pass |
| Fabric `AttachmentSync.onInitialize` | ~4.9M | fail | pass |
| `CustomUnbakedBlockStateModelInit.onInitializeClient` | >50M | fail | **intentionally still fails** |

The last entrypoint only registers custom block-state model codecs. It is unrelated to the
mods tested here and is deliberately not given a larger budget. A failure is logged as
`Mod entrypoint <class>.<method><descriptor> failed to initialize: ...`; it does not abort
the other entrypoints.

`Interpreter::trace_calls` and `call_stack` are opt-in diagnostics. When enabled, a budget
error names the Java frames it occurred under. They are off by default because they allocate
a string per call.

## Cold-start fix: synthetic statics

A mod reading `Items.WHEAT`, `Blocks.WHEAT` or a vanilla tag does not need Minecraft's real
registry to be built. The runtime already supplies synthetic values for those fields. The
problem was ordering: `Getstatic` ran the class's real `<clinit>` first, so the interpreter
built the entire item/block/tag registry (and `Stats`, `DataComponents`, `Registries`) before
replacing the result with the synthetic value.

`has_synthetic_statics` in `interp.rs` now lists the registry-sized classes whose `<clinit>`
is skipped:

- `net/minecraft/world/item/Items`
- `net/minecraft/world/level/block/Blocks`
- `net/minecraft/tags/ItemTags`
- `net/minecraft/tags/BlockTags`
- `net/minecraft/core/component/DataComponents`
- `net/minecraft/core/registries/Registries`
- `net/minecraft/stats/Stats`

Cheap enums and singletons (`InteractionHand`, `InteractionResult`, `Direction`,
`EquipmentSlot`, `SoundEvents`) still run their real initializers, so code that relies on
their `$VALUES` or other static state keeps working.

Measured effect with the real Minecraft/JDK classpath and Right Click Harvest loaded:

| Call | Before | After |
|---|---:|---:|
| First `right_click_block` | 34.3 s | 30-34 ms |
| Second `right_click_block` | 0.33 ms | 0.33 ms |

In the running client, the first harvest after joining changed the block and delivered drops
by tick 42 when the click was issued at tick 40 (about 100 ms). Before the fix the same first
click took roughly 4.8 s to take effect; the user-visible delay was reported as about 10 s.
The control command's immediate `changed:false` reply is a race: it reads the block before
the server applies the harvest.

## Event coverage today

| Path | Status | Notes |
|---|---|---|
| Right Click Harvest | **works** | JamLib `InteractionEvent.USE_BLOCK`; the mod's replant and drops are replayed into the host |
| Fabric `ItemTooltipCallback` | dispatched | text lines can be added/changed |
| Iceberg `RenderTooltipEvents.COLOREXT` | dispatched | background/border colors and gradient flags |
| Iceberg `GATHER` / `POSTEXT` | **not dispatched** | Legendary Tooltips needs these for its frame |
| Fabric `ScreenEvents.AFTER_INIT` | **not dispatched** | Mod Menu cannot add its title-screen button or open `ModsScreen` |
| Fabric `ClientTickEvents` | **not dispatched** | keybinds and tick-driven UI do not run |
| Mixins / access wideners | **not applied** | Java patches are outside this runtime |

### Mod Menu

`ModMenu.onInitializeClient` now completes with the 10M budget. Its UI still cannot appear:
`ModsScreen` extends the real `Screen`, builds widgets and renders through
`GuiGraphicsExtractor`. The runtime has no `Minecraft.setScreen`, `Screen`, widget-list or
GUI renderer, so Horizon's native Mods page remains the visible screen. This is a missing
host feature, not a budget failure.

### Legendary Tooltips

Initialization and `COLOREXT` dispatch work, but the callback depends on
`Minecraft.getInstance().level` and registry access; the runtime does not provide a live
client level, so the callback returns the incoming colors. The visible frame is also a
nine-slice texture produced through `GATHER`/`POSTEXT`, and Horizon's tooltip renderer draws
only a solid background plus top/bottom border colors. Mod tooltip textures are not loaded.
Until a textured tooltip renderer, those two events and a live client level exist, the mod's
frame will not be visible even though its listener runs.

## Tests

Runtime tests use third-party jars from `test-mods/` and, for classpath tests, a local
Minecraft installation. The jars are **not committed** (see `test-mods/README.md`). Run:

```sh
cargo test -p hfc-runtime
cargo fmt --check -p hfc-runtime
```

The suite covers bridge loading, RCH harvesting, real JDK/Minecraft bytecode, JImage loading
and missing-class diagnostics. A test that needs the real game classpath skips or fails
clearly when the installation is absent; it must not silently pass with fabricated classes.

## Adding a builtin or event

1. **Never add per-mod code.** Fix the shared path a mod exposes: an opcode, a Fabric API
   event, a Minecraft class/method builtin, a config/resource format, or a host boundary.
   Do not check a mod id, a mod's package/class name, a config field, a jar name or a version.
   A test may load one mod to reproduce a bug; the implementation must remain universal.
   Library adapters for APIs shared by many mods are allowed, but adapters for one mod's own
   classes are not. Existing per-mod shims are debt and must be generalized when touched.
2. Prefer executing the mod's real bytecode. Add a builtin only for a host boundary (I/O,
   registry data, rendering) or a class the runtime intentionally replaces.
3. If a class has synthetic static fields, add it to `has_synthetic_statics` and to
   `create_static_field_val`; otherwise the first `Getstatic` rebuilds the real registry.
4. Dispatch an event only after its listener signature and result semantics are understood.
   A callback that runs but returns defaults is not compatibility; document it as partial.
5. Add a focused test with the real mod jar and, when possible, a live-client check through
   `horizon-ctl`.
6. Record measured costs here when changing `ENTRYPOINT_BUDGET`.

## Repository note

The root repository does not track `workspace/horizon`, `workspace/horizon-mods` or
`research/horizon-mixins`; each is a separate Git checkout with its own history. Client-side
adapter changes must be committed in `workspace/horizon`. Build output, generated mods,
world saves and downloaded jars are ignored by the root repository.
