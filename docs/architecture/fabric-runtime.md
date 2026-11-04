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
| Fabric screens | `.../game/fabric_bridge.rs`, `Screen::Fabric` in `game.rs` | Replays the recorded GUI calls and routes mouse/key/char input |

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
| Fabric `ScreenEvents.AFTER_INIT` | dispatched | runs for every initialized mod screen |
| Fabric `ClientTickEvents` | dispatched | runs once per host tick, before the screen tick |
| Fabric key mappings | supported | `KeyMapping`, `KeyMappingHelper` and `consumeClick`; the host binds by translation key or physical key |
| Mixins / access wideners | **partially applied, menu widgets only** | `@Inject` callbacks that add widgets to a host menu are run with synthesized screen/layout locals; all other mixins and access wideners are ignored |

### Mod Menu

`ModMenu.onInitializeClient` completes, registers a Fabric key mapping and a client-tick
listener. Pressing that mapping through the host opens `ModsScreen`; the host initializes it
and replays its `GuiGraphicsExtractor` display list. The list itself is real: it builds an
`AbstractSelectionList`, filters all loaded mods and renders their names. A pinned test
(`tests/modmenu_screen.rs`) opens the screen from the key mapping, asserts the list and its
widgets, checks that `Minecraft`, `Java`, `Mod Menu` and the other root mods are present, and
closes it with Escape. Two more tests drive the widgets: click the search field, type, see the
filtered count and the typed character, press Backspace to clear it, and click Done; and
scroll the list with both the wheel and a scrollbar drag at a height where it overflows.

Fabric API's nested modules are libraries, and Mod Menu hides them by default. That depends on
Fabric Loader's custom metadata (`fabric-api:module-lifecycle` under `custom` in
`fabric.mod.json`) and on `ModMenuConfig.SHOW_LIBRARIES` defaulting to `WITH_CONFIG`; both are
answered from the real mod metadata, so the list shows the root mods (11 with the pinned test
set) rather than every Fabric API module.

Mod Menu's title and pause buttons come from its `MixinTitleScreen`/`MixinPauseScreen` mixins,
not from a Fabric API event. The runtime reads the mods' mixin configs and annotations and
applies only the widget-adding `@Inject` callbacks: it synthesizes the callback's target-screen
receiver, `CallbackInfo`, `@Local`/MixinExtras value refs and layout objects, then records the
widgets the callback adds through a layout's `addChild` or Fabric's
`Screens.getWidgets(screen).add(widget)`. The host draws those widgets on the native title and
pause menus (appended to each menu's icon row); clicking one opens the mod's screen over the
native menu, and Done/Escape returns to it. The pinned tests assert both menus produce a
`Mods` widget, that it renders, and that clicking it installs `ModsScreen`.

This is **not general Mixin support**. The vanilla layout method itself is not executed, so the
callback runs with synthesized locals and the widget is host-placed; `@ModifyArg`,
`@ModifyVariable`, `@Redirect`, `@WrapOperation`, MixinExtras expressions, accessors/invokers
and all gameplay/draw-code transforms are not applied. Menu buttons whose sprites are not in the HUD atlas (Mod Menu's own icon) are resolved by
`FabricBridge::gui_texture`: a `namespace:path` sprite is read from
`assets/<namespace>/textures/gui/sprites/<path>.png` in the loaded mods' jars, decoded by the host
and placed in the atlas's run-time region, the same way registered icons are. Title/pause widgets
and Fabric screens share that upload. Nine-slice `.mcmeta` scaling is not read; such a sprite is
stretched. An identifier no jar provides still renders as a labelled placeholder tile.

The rendering is partial, and the runtime does not claim Mod Menu is fully working:

- **There is no search placeholder because the mod never sets one.** Mod Menu 21.0.0 passes
  `ModMenuScreenTexts.SEARCH` ("Search for mods") as the `EditBox` constructor's last
  `Component`, and 26.3's constructor stores that as the widget's narration message. The mod
  never calls `setHint`, and `EditBox.extractWidgetRenderState` only draws a separate `hint`
  field. Vanilla 26.3 shows no placeholder here either, so the runtime does not invent one.
  The field is still operable: clicking it focuses it and typing filters the list.
- **Mod icons are real when the jar provides them.** `ModContainer.getPath` resolves a mod's
  icon resource inside its own jar (nested jars included), `Files.newInputStream` returns the
  bytes, and `NativeImage.read` keeps the PNG with the image. `DynamicTexture` carries it to
  `TextureManager.register`, where the runtime records the identifier and PNG bytes. The host
  decodes that PNG, places it in the HUD atlas's run-time region, and draws the `blit` with it
  (capped at 64 px per side). Mods with no readable icon, and synthetic containers with no jar
  (Fabric Loader in the pinned set), still get a labelled placeholder tile.
- **Description text is incomplete.** The description panel can render `null` and an empty
  component while fields it reads are still missing; names, titles, buttons, the list and
  search are the parts demonstrated by the tests.
- **Input uses 26.3's SDL field values.** `KeyEvent.key` is a scancode (`Escape` is 41) while
  `KeyEvent.keycode` is the SDL keycode `EditBox`/`Screen` switch on for Tab, Backspace, Delete
  and the arrows; `MouseButtonInfo.button` is SDL numbering (left is 1). A press makes the
  following moves dispatch `mouseDragged`, which is how scrollbar thumbs and sliders follow the
  pointer. The host maps winit keys and buttons to those conventions before dispatching.
- **Only the Fabric screen host exists.** There is no general widget/layout engine; the real
  Minecraft `Screen`, `AbstractSelectionList` and widget bytecode runs, and the host replays
  the recorded calls.

### Turning a mod off

Options > Mods lists every loaded Fabric jar as on until `horizon-mods.txt` says `mod.<id>:false`.
At startup the host reads that file and skips any jar whose `fabric.mod.json` id is switched off
(`FabricBridge::jar_summary` reads the id without loading the jar), so none of its entrypoints, key
mappings or menu widgets exist. Skipped jars are still listed so they can be turned back on. The
file is read once, so a change applies at the next launch. Dependencies are not checked: turning
off a library such as Fabric API leaves the mods that need it to fail.

### Legendary Tooltips

Initialization and `COLOREXT` dispatch work, but the callback depends on
`Minecraft.getInstance().level` and registry access; the runtime does not provide a live
client level, so the callback returns the incoming colors. The visible frame is also a
nine-slice texture produced through `GATHER`/`POSTEXT`, and Horizon's tooltip renderer draws
only a solid background plus top/bottom border colors. Mod tooltip textures are not loaded.
Until a textured tooltip renderer, those two events and a live client level exist, the mod's
frame will not be visible even though its listener runs.

## GUI replay

`GuiGraphicsExtractor` calls are recorded into a `GuiDraw` display list
(`crates/hfc-runtime/src/minecraft.rs`) instead of touching the GPU:
`fill`, `fillGradient`, `text`, `centeredText`, `blit`, `blitSprite`, and scissor bounds.
`FabricBridge::render_screen(width, height, partial_tick)` runs the real
`Screen.extractRenderStateWithTooltipAndSubtitles` and returns that list. The host
(`Game::draw_fabric_screen`) replays fills, text and scissor clipping through `HudBatch`; it
draws a blit from the HUD atlas when the sprite is packed there, or from a texture the mod
registered (`TextureManager.register`) after decoding its PNG into the atlas's run-time region.
`Screen::init` runs whenever the screen object or GUI size changes.

Input is a host boundary too. `FabricBridge` builds real `KeyEvent`, `CharacterEvent` and
`MouseButtonEvent` objects (with their 26.3 field layouts) and dispatches to the screen's
`keyPressed`/`charTyped`/`mouseClicked`/`mouseReleased`/`mouseScrolled`/`mouseMoved`/
`mouseDragged`. A held button turns movement into `mouseDragged`, as 26.3's `MouseHandler`
does, so a scrollbar thumb or slider follows the pointer. `Minecraft` and `Gui.setScreen` keep
the installed screen where mods read it back.

Key mappings are registered by `KeyMappingHelper.registerKeyMapping` and stored in the
runtime's mapping registry. `FabricBridge::press_key_mapping(name)` triggers one by its
translation key and then runs `ClientTickEvents.END_CLIENT_TICK`, which is how Mod Menu opens
its screen. `press_key_code` does the same for every mapping bound to a physical key. The
host exposes the translation-key form as:

```sh
horizon-ctl fabric-key key.modmenu.open_menu
```

This is a generic binding action: it looks up any Fabric mapping by its registered name and
does not know which mod owns it.

## Tests

Runtime tests use third-party jars from `test-mods/` and, for classpath tests, a local
Minecraft installation. The jars are **not committed** (see `test-mods/README.md`). Run:

```sh
cargo test -p hfc-runtime
cargo fmt --check -p hfc-runtime
```

The suite covers bridge loading, RCH harvesting, real JDK/Minecraft bytecode, JImage loading,
missing-class diagnostics and the Mod Menu screen path. A test that needs the real game
classpath skips or fails clearly when the installation is absent; it must not silently pass
with fabricated classes.

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
