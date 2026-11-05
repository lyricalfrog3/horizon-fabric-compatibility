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
| Fabric bridge | `crates/hfc-runtime/src/bridge/` | `mod.rs`: reads `fabric.mod.json`, runs entrypoints, owns the instruction budget. `screen.rs`: the open screen, ticks, key mappings and input. `menu.rs`: mixin-added menu widgets. `events.rs`: right-click and tooltip events. `textures.rs`: registered textures and jar sprites |
| Runtime builtins | `crates/hfc-runtime/src/builtins.rs` | Java/JDK, Fabric API and Minecraft shims |
| Client adapter | `workspace/horizon/crates/horizon-client/src/game/fabric_bridge/` | `mod.rs`: loads jars, supplies the real Minecraft classpath, bridges host blocks/items. `gui.rs`: display-list replay and screen input. `menu.rs`: host-placed menu widgets. `textures.rs`: atlas upload. `keys.rs`: SDL key numbering |
| Client hooks | `.../game/native_hooks.rs`, `.../game/stations.rs` | Right-click dispatch, tooltip text and tooltip colors |
| Fabric screens | `.../game/fabric_bridge/gui.rs`, `Screen::Fabric` in `game.rs` | Replays the recorded GUI calls and routes mouse/key/char input |

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
| Fabric `PlayerBlockBreakEvents.BEFORE` | dispatched by the runtime (`FabricBridge::block_break_before`) | Veinminer's listener runs end to end in a test: it breaks the connected ore through `ServerPlayerGameMode.destroyBlock`, and the bridge hands each broken position to `HostBridge::break_block`. The client calls it from `native_destroy_block`; a live run broke a 5-block coal row with one break and a pickaxe carrying the mod's enchantment (offscreen run, no durability or drops for the extra blocks beyond what `Game::break_block` does). |
| Fabric `ServerLifecycleEvents.SERVER_STARTING` | dispatched (`FabricBridge::server_starting`) | runs with an empty `MinecraftServer`; the client calls it once after `init` |
| Fabric `ItemTooltipCallback` | dispatched | text lines can be added/changed |
| Iceberg `RenderTooltipEvents.COLOREXT` | dispatched | background/border colors; the callback gets the real client level's registry access and the host's item rarity |
| Iceberg `RenderTooltipEvents.POSTEXT` | dispatched | returns a `GuiDraw` list the host replays after the tooltip text; this is where Legendary Tooltips draws its frame and shadow |
| Iceberg `RenderTooltipEvents.GATHER` | dispatched | the mod receives the raw text elements and can add image components (Legendary adds its item model and padding) |
| Iceberg/Prism `RenderTickEvents.START` | dispatched with each tooltip render | a `DeltaTracker` carries the real time since the previous tooltip frame, so shine and model timers animate |
| Fabric `ScreenEvents.AFTER_INIT` | dispatched | runs for every initialized mod screen |
| Fabric `ClientTickEvents` | dispatched | runs once per host tick, before the screen tick |
| Fabric key mappings | supported | `KeyMapping`, `KeyMappingHelper` and `consumeClick`; the host binds by translation key or physical key |
| Mixins / access wideners | **partially applied** | menu-widget `@Inject` callbacks run with synthesized screen/layout locals; non-cancellable `@Inject(at = HEAD)` callbacks on `GuiGraphicsExtractor.tooltip` run with the real receiver and arguments (title centring). Cancellable HEAD callbacks and draw-code transforms need the real target body and are ignored |

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
`@ModifyVariable`, `@Redirect`, `@WrapOperation`, MixinExtras expressions
and all gameplay/draw-code transforms are not applied. Menu buttons whose sprites are not in the HUD atlas (Mod Menu's own icon) are resolved by
`FabricBridge::gui_texture`: a `namespace:path` sprite is read from
`assets/<namespace>/textures/gui/sprites/<path>.png` in the loaded mods' jars, decoded by the host
and placed in the atlas's run-time region, the same way registered icons are. Title/pause widgets
and Fabric screens share that upload. Nine-slice `.mcmeta` scaling is not read; such a sprite is
stretched. An identifier no jar provides still renders as a labelled placeholder tile.

`@Accessor` and `@Invoker` interfaces are the one other mixin form applied. The runtime reads
them from every mixin class (`mixin::accessors_from_class`, name inference as Mixin does) and
registers them with the interpreter: a call naming the interface runs the target class's method
(invoker) or reads/writes its field (accessor). Right Click Harvest's `CropBlockAccessor` works
this way.

Reflection that constructs objects (`Class.getConstructor`/`getDeclaredConstructor` and
`Constructor.newInstance`) runs the class's real constructor. That is how JamLib's
`ConfigManager` builds a mod's config class, so Right Click Harvest's `Config` and its enums come
from the mod's own bytecode. Config files are not read or written yet: `Files.writeString`,
Jankson and `Path.getParent` are unresolved and return defaults, so every mod runs on its config
defaults. Unresolved calls returning defaults is also why a test can pass without a feature
working; check `Interpreter::missing` (set `trace_missing`) before claiming a path runs.

Exceptions are frame-local. The Code attribute's exception table is read, and `athrow`,
array index/null errors and integer division by zero run a handler of the *same method* that
covers them (`catch_locally`). An exception nothing in that method catches does not unwind into
the caller: the method ends, or the opcode takes its old lenient result. Full unwinding was tried
and reverted: the real JDK, Netty, joml and Minecraft code a mod pulls in throws dozens of
exceptions the runtime otherwise answers with defaults (`Pattern.compile`,
`Class.getDeclaredMethod`, `StackWalker`, Jankson in JamLib's config save, ...), and unwinding
them aborted static initializers, including Right Click Harvest's config. Those need real host
boundaries first; each is a place where the runtime is still wrong, not only silent.

Other known gaps found by running Veinminer (Kotlin): a null that would throw in a JVM still
loops (Kotlin's null-parameter message walks an empty stack trace), and there are no real threads,
so Kotlin coroutine dispatchers only get as far as a single-threaded interpreter allows.
`Atomic*FieldUpdater` is modelled as a plain read-modify-write over the instance field. A static
call is never treated as a lambda invocation, and `Language`'s initializer is skipped because
translations come from the host. Veinminer's `onInitialize` now returns without error; that says
nothing about its block-breaking behaviour, which is not exercised.

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

Minecraft's own tooltip body is built from the mods' real
`List<ClientTooltipComponent>`. The bridge wraps the host's text lines in
`FormattedText`, dispatches Iceberg's `GATHER` (an `Either<FormattedText,
TooltipComponent>` list), converts the result to client components, and applies the
non-cancellable `@Inject(at = HEAD)` callbacks on `GuiGraphicsExtractor.tooltip` with the
real receiver and arguments. That is where Legendary's `GuiGraphicsMixin` centres the title
and inserts its title-break component. The bridge then measures the components, records
their `extractText` and `extractImage` calls into a `GuiGraphicsExtractor` display list, and
appends `RenderTooltipEvents.POSTEXT` (frame, shadow, separator). The host replays the list
after its background box, so the centred title, separator, model panel and frame come from
the mod's own code.

What the path needed:

- `COLOREXT` reads `Minecraft.getInstance().level.registryAccess()`. The runtime answers
  `Level.registryAccess()` from the host-loaded registry snapshot, so the listener no longer
  returns the incoming colors.
- `Selectors.itemMatches` compares `ItemStack.getRarity()` with `Rarity.EPIC`; the runtime
  resolves the real enum field from the item's host-reported rarity, so `!epic` / `!rare`
  selectors work. Reference equality with `Rarity.EPIC` holds because both names resolve to
  the same enum constant.
- `TextureManager.getTexture(id).getTexture().getWidth()` reports the real size of a PNG the
  mod ships. `load_mod_jar` records the IHDR size of every `assets/*/textures/gui/*.png`.
- `GuiGraphicsExtractor.blit(texture, x, y, w, h, u0, u1, v0, v1)` records the sampled
  sub-rectangle, and the host applies it to the texture's atlas region. Without that, a
  nine-slice frame would stretch the whole sheet into every corner.
- The host keeps its own tooltip background when the mod returns a fully transparent one.
  Legendary uses transparent level-0 colors because Iceberg's background-cancelling mixin
  supplies the surface; that mixin is not applied here, so the host box stays.
- `ItemStack.getMaxDamage()`/`isDamageableItem()` answer from the host's item data, so
  Legendary's `render_item_model = "EQUIPMENT"` adds its model to equipment tooltips.
- `FormattedCharSequence.accept` feeds a `FormattedCharSink` (Iceberg's `StringRecomposer`
  reads a title through it), `new ArrayList(collection)` copies its source,
  `StringBuilder.append(char[])` appends characters, and `Language.getVisualOrder` returns a
  text-carrying sequence. Together these let the library recompose and centre the title.
- Item components record a `GuiDraw::Item`; `GuiGraphicsExtractor.item` does the same for
  Fabric screens. Iceberg's `CustomItemRenderer` is a host boundary: its request is recorded
  instead of running its offscreen GPU path.
- `RenderTickEvents.START` gets a `DeltaTracker` whose `getRealtimeDeltaTicks` is the host
  time since the previous tooltip frame, which is what `TooltipDecor.updateTimer` and
  `ItemModelComponent.updateTimer` read.

Verified live (frame): hovering an enchanted netherite pickaxe shows the gold frame with
readable text; a common item keeps the normal dark tooltip and no frame.
`tests/legendary_tooltips.rs` now also asserts that GATHER adds the item-model and padding
components, that Legendary's HEAD injector padded (centred) the title, that the name
separator fills were drawn, that the model component recorded a `GuiDraw::Item`, and that
the shine timer advances with real time.

Still partial: the model is drawn through the GUI item renderer, so its recorded Y rotation
is not applied (the HUD has no 3D model transform), and `model_rotation_speed = 0` (the
current config) keeps it still. The shine's horizontal gradients are recorded as vertical
ones because GUI pose rotation is not applied. A component type other than text or an image
component is only drawn if it implements `extractText`/`extractImage`.

## GUI replay

`GuiGraphicsExtractor` calls are recorded into a `GuiDraw` display list
(`crates/hfc-runtime/src/minecraft.rs`) instead of touching the GPU:
`fill`, `fillGradient`, `text`, `centeredText`, `item`, `blit`, `blitSprite`, and scissor
bounds. `item` is recorded as `GuiDraw::Item` and the host draws it with the game's own GUI
item renderer. `fillGradient` is recorded as `GuiDraw::Gradient` (both colours) and replayed with
`HudBatch::fill_gradient`. A blit carries the normalised `u0..u1`/`v0..v1` it samples, so a
nine-slice frame can blit one corner of a larger sheet; the host crops the texture's atlas
region to that sub-rectangle.

`FabricBridge::render_screen(width, height, partial_tick)` runs the real
`Screen.extractRenderStateWithTooltipAndSubtitles` and returns that list. The host
(`Game::draw_fabric_screen`) replays fills, gradients, text and scissor clipping through
`HudBatch`; it draws a blit from the HUD atlas when the sprite is packed there, or from a
texture the mod registered (`TextureManager.register`) or ships in its jar after decoding its
PNG into the atlas's run-time region. `Screen::init` runs whenever the screen object or GUI
size changes.

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

## Server-side boundaries (Veinminer path)

Kotlin server mods run through the same interpreter. What they needed, and where each piece
lives:

| Boundary | Where | Behaviour |
|---|---|---|
| Config files | `vfs.rs`, `fs_builtins.rs`, `text_io.rs` | `Files`, `Path`, readers and writers go to a root-restricted filesystem under the game directory (`FabricBridge::set_game_dir`); with no game directory they stay in memory. `getResourceAsStream` reads the mod's own jar. |
| Registries and tags | `registry.rs`, `FabricBridge::load_registries` | Block and item ids come from the client's `registries.json`, tags from `block-tags.json`, `data/*/tags` and the loaded mod jars. `Registry.containsKey`, `getTagOrEmpty`, `TagKey.create` and `Holder.value` answer from that snapshot. |
| Level | `events.rs` | A snapshot of the host blocks within a radius of the broken block. Breaks made by the mod are recorded and replayed through `HostBridge::break_block` when the event returns. |
| Held item | `HostBridge::held_item`, `held_enchantments` | The main-hand item and its enchantments. Durability and block hardness are **not** modelled: `getMaxDamage` is 0 and `requiresCorrectToolForDrops` is false. |
| Server tasks | `Interpreter::server_tasks` | `MinecraftServer.execute` queues; the dispatcher runs the queue after the listeners return, as a running server task defers new ones. |
| Coroutines | `interp.rs` | `Dispatchers.Default`/`IO` resolve to `Dispatchers.Unconfined`, so a launched coroutine runs on the dispatching thread until its first real suspension. Delays are not timed: `TimeUnit.convert` is not implemented, so tick-based cooldowns evaluate to 0. |
| Kotlin null checks | `builtins.rs` | `Intrinsics.check*` log a warning and continue rather than throw. |

Known gaps: no durability, hunger, drops, particles or sound (`Block.getId`, `levelEvent` are
unresolved no-ops), no real threads, and a mod's network handshake (`NetworkRouter`) never runs.

## Reflection and config files

Mods that write a config file usually do it by reflecting over a config class
(JamLib/Jankson for json5, Iceberg/Night Config for TOML). The loader's class
metadata drives this in `reflect.rs`, ahead of the builtins:

- `Class.getFields/getDeclaredFields/getField/getDeclaredField`, `Class.getMethods/
  getDeclaredMethods/getMethod/getDeclaredMethod`, `getEnumConstants`, and real
  `isAssignableFrom`/`isInstance` over the loaded hierarchy.
- `Field` (`get`, `set`, `getType`, `getModifiers`, `getAnnotation`, ...) and `Method`
  (`invoke`, `getParameterTypes`, annotations, ...). Field order is class-file order.
  Annotation instances answer their element methods from the class-file values.
- A `boolean` read through reflection is a `java/lang/Boolean` instance, since the
  value model has no boxed boolean (an `Int` would serialize as a number). Plain
  `Int/Long/Float/Double` values are instances of their wrapper for `instanceof`
  and `getClass`.
- `ServiceLoader` reads `META-INF/services/<service>` from every loaded jar and
  constructs the providers; there is no per-service table.
- `String.replaceAll/replaceFirst/split` use the `regex` crate with Java's `\R`
  translated; `System.lineSeparator` is `"\n"`.
- `String.matches/format/formatted/join/compareTo`, `Collectors` (`toList`, `toSet`,
  `joining`, `toCollection`, `toMap`), `IntStream.range`, and `Map`/`List` helpers
  (`computeIfAbsent`, `merge`, `putIfAbsent`, `subList`, ...) run real semantics.
- `Field.getGenericType` reads the class file's `Signature` attribute and answers a
  `ParameterizedType` whose `getActualTypeArguments` are class objects; `instanceof
  Class` holds for class objects. Mod Menu uses this to write its enum options.
- Gson is not stubbed: `Gson.toJson` runs Gson's own `JsonWriter`. A library
  `Writer` subclass keeps its own `write`/`append`, and a library `CharSequence`
  appended to a `StringBuilder` contributes its `toString()`.
- A mod's own static field that nothing assigned is `null`; only platform classes
  get synthesized statics.
- An inherited static field resolves to the class that declares it: `getstatic
  Subclass.field` and `putstatic Subclass.field` use the superclass's storage, as a JVM
  does. Iceberg's `LegendaryTooltipsConfig.getInstance()` reads the config map through a
  field inherited from `IcebergConfig`.
- `List.of`/`Set.of`/`Map.of` and their `copyOf` forms return the runtime's own
  list/set/map, not a JDK-internal class the heap cannot model. A class that extends a
  modeled collection (an anonymous `HashMap` subclass calling its inherited `put`) also
  dispatches to that storage.
- Mixin-added interfaces are applied generally (`mixin::interface_methods_from_class`).
  A mixin class that implements an interface (Prism's `TextColorMixin implements IColor`
  on `TextColor`) adds that interface to its target for `instanceof`, and a call to one
  of its methods on the target runs the mixin's own bytecode with the target as `this`,
  so `@Shadow` fields read the target's fields. A receiver whose own class implements the
  method (Prism's `DynamicColor`) keeps its implementation. Mixin-annotated members
  (`@Inject`, `@Shadow`, `@Overwrite`, accessors) are not added this way.
- The declaring class of a static field reached through a subclass is cached per
  (class, field), so `getstatic`/`putstatic` walk the hierarchy once.
- File watchers (`WatchService`, `Path.register`) are inert: nothing changes on disk
  while the game runs, so a config is not reloaded live.

Verified: `tests/config_files.rs` loads JamLib and Right Click Harvest and compares
the written `config/rightclickharvest.json5`, comments included, with the file the real
game writes; it also checks Legendary Tooltips' `legendarytooltips.toml` (Iceberg and
Night Config) and Mod Menu's pretty-printed `modmenu.json` (Gson). Known limits: `getMethod` of a missing method returns null instead of
throwing `NoSuchMethodException`; `List.subList` is a copy, not a view; a config
file that already exists is read back through the same reflection path
(`Field.set` with the parsed value). That works in the live client (an edited
`modmenu.json` is honoured after a restart) but is not covered by a test yet.

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
   classes are not. There are no per-mod shims left; do not add one.
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
