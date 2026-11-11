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
entrypoint gets `ENTRYPOINT_BUDGET` instructions (currently **100,000,000**).

A JVM has no instruction cap. The cap exists only to stop a mis-decoded branch from hanging
the render thread. The previous 200,000 cap aborted legitimate Fabric API initialization and
left mods half-registered. Measured with the real Minecraft jar, libraries and JDK image:

| Entrypoint | Approximate cost | 200k | 10M | 100M |
|---|---:|---|---|---|
| `ModMenu.onInitializeClient` | ~367k | fail | pass | pass |
| Fabric `RecipeSyncImpl.onInitialize` | ~590k | fail | pass | pass |
| Fabric `CustomIngredientInit.onInitialize` | ~575k | fail | pass | pass |
| Fabric `AttachmentSync.onInitialize` | ~4.9M | fail | pass | pass |
| `CustomUnbakedBlockStateModelInit.onInitializeClient` | >50M | fail | fail | not re-measured at 100M |
| `MdmMod.onInitialize` (344 blocks, 15,360 states) | ~46.1M | fail | fail | pass (release ~5.3 s, debug ~54 s) |

The mdm cost is dominated by building `StateDefinition`s for blocks with many
states (`furniture_part` 2,916; three `electric_guitar_*` 3,468 each). The budget was raised
from 10M to 100M for it. `CustomUnbakedBlockStateModelInit` only registers custom block-state model codecs. It is unrelated to the
mods tested here; it was not re-measured after the raise and may now run further. A failure is logged as
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
- `net/minecraft/core/registries/BuiltInRegistries`
- `net/minecraft/core/particles/ParticleTypes`
- `net/minecraft/stats/Stats`

Cheap enums and singletons (`InteractionHand`, `InteractionResult`, `Direction`,
`EquipmentSlot`) still run their real initializers, so code that relies on their `$VALUES` or
other static state keeps working. `SoundEvents` is **not** in that group: it is not in
`has_synthetic_statics`, so the first read of one of its fields runs the real initializer and
registers Minecraft's whole sound registry. `crates/hfc-runtime/src/builtins.rs` records that path
at ~30 s on a widget click, which is why `AbstractWidget.playDownSound`/
`playButtonClickSound` are hand-written no-ops.

Every class in `has_synthetic_statics` has a matching branch in `create_static_field_val`; a
class in one list and not the other silently answers the `TagKey` fallback. `Stats` was in that
state until `ad2b274` gave it a synthesizer that builds a real `net/minecraft/stats/Stat`.

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
| Fabric `ClientTickEvents` | dispatched | `END_CLIENT_TICK` runs once per host tick, after `Screen.tick()` (`FabricBridge::client_tick` in `bridge/screen.rs`) |
| Fabric key mappings | supported | `KeyMapping`, `KeyMappingHelper` and `consumeClick`; the host binds by translation key or physical key |
| Mixins | **applied only to interpreted methods and two host seams** | `@Inject` at `HEAD`/`TAIL` whose target resolves to interpreted bytecode is run by the interpreter around the real body, cancellable or not: `cancel()`/`setReturnValue()` end the method. Separately, menu-widget callbacks get synthesized layout locals, and non-cancellable `HEAD` callbacks on `GuiGraphicsExtractor.tooltip` get the real receiver and arguments. `@Redirect`, `@ModifyArg`, `@ModifyVariable`, `@WrapOperation`, `@At(INVOKE)` and MixinExtras local capture are **not** applied; they are recorded unsupported with a reason. `@Accessor`/`@Invoker` is under repair (see the note below) |
| Access wideners | **not applied** | Nothing in `crates/hfc-runtime` reads an access-widener file; the install side only detects and existence-checks it (`hfc-fabric` `metadata.rs`/`archive.rs`) |

### Mod Menu

`ModMenu.onInitializeClient` completes, registers a Fabric key mapping and a client-tick
listener. Pressing that mapping through the host opens `ModsScreen`; the host initializes it
and replays its `GuiGraphicsExtractor` display list. The list itself is real: it builds an
`AbstractSelectionList`, filters all loaded mods and renders their names. A pinned test
(`tests/modmenu_screen.rs`) opens the screen from the key mapping, asserts the list and its
widgets, checks that `Minecraft`, `Mod Menu` and the other non-library entries are present (the
list renders "Showing 2 mods"), and closes it with Escape. One companion test drives the search
field: click it, type, see the filtered count and the typed character, press Backspace to clear
it, and click Done.

Two tests in that file fail at this commit and must not be read as coverage: the key-mapping test
above fails because the list's Website button draws no label (the text list holds an empty `_`
entry where it belongs), and `the_mod_list_scrolls_with_the_wheel_and_the_scrollbar` fails because
the wheel does not move the list. Both are runtime defects, not fixture skips — a skip prints
`skipping:` instead.

Fabric API's nested modules are libraries, and Mod Menu hides them by default. That depends on
Fabric Loader's custom metadata (`fabric-api:module-lifecycle` under `custom` in
`fabric.mod.json`) and on `ModMenuConfig.SHOW_LIBRARIES` defaulting to `WITH_CONFIG`; both are
answered from the real mod metadata, so the list shows the two outer jars (`modmenu` and
`fabric-api`) rather than Fabric API's 44 nested modules. `FabricLoader.getAllMods` itself still
returns every container it was given — the loader's own `fabricloader`, `minecraft` and `java`
entries plus each jar root and nested module — so the filtering to 2 happens in Mod Menu's screen,
not in the runtime.

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
and all gameplay/draw-code transforms are not applied.

### `@Inject` on an interpreted method

A `@Inject` whose target is a method the interpreter runs is applied by the interpreter, not by
a class-file rewrite. Every mod and Minecraft method this runtime executes goes through
`Interpreter::execute_method_inner`, so the interpreter is the one place where "before the
body" and "after the body" both exist. `mixin::verify_injector` checks each parsed annotation
against the classpath — the target class loads, the target method resolves to exactly one body
in its superclass chain, the callback exists and takes the target's parameters followed by the
matching `CallbackInfo` — and only a verified injector becomes a `WiredInjector`.
`FabricBridge::init` hands that set to the interpreter (`set_mixin_injectors`), which runs the
callbacks at the class that declares the target body:

- `HEAD` runs before the body with the receiver, the method's real arguments and a fresh
  `CallbackInfo`/`CallbackInfoReturnable`. A callback that cancels makes the method return
  immediately — `void` returns nothing, and a returnable returns `getReturnValue*()` — so the
  body never runs. This is what Mixin's inserted `if (ci.isCancelled()) return ...;` does.
- `TAIL` runs after the body with the value it is about to return as the callback info's
  initial value; a cancel replaces that value.

The callback object is `crate::minecraft::CallbackInfo`, a native object whose builtins answer
Mixin's contract: `cancel()` (which flags it), `isCancelled`, `isCancellable`, `getId`,
`getReturnValue`/`getReturnValueI`/`J`/`F`/`D`/`Z`/`B`/`C`/`S` (an unset value reads as the
type's zero) and `setReturnValue`, which records the value **and cancels**, exactly as Mixin's
does. A callback that cancels a non-cancellable injection raises `CancellationException`, as
Mixin's `cancel()` throws.

An annotation that cannot be applied is recorded in `MixinManifest::unsupported` with its reason
(an unresolvable target, an overloaded target with no descriptor in the annotation, a
`CallbackInfo`/`CallbackInfoReturnable` mismatch, a callback whose parameters are not the
target's, an `INVOKE`/MixinExtras point) and is **not** reported as applied. The census's
`applied` outcome now means "wired and dispatched by the interpreter"; a target whose
annotations parsed but were never wired is `partial` or `ignored`. `supported()` on a parsed
`MixinInjector` is only a parse check and must not be read as compatibility.

Mixin passes the callback a *merged* view of the target: `this` is the target receiver, not an
instance of the mixin class. The runtime passes that receiver, so a callback's `getfield`/
`putfield` reach the target object's own field table by name — which is what a `@Shadow` field
compiles to. No separate shadow-field merge happens; the names simply line up because both
classes refer to the same field name on the same object. Menu buttons whose sprites are not in the HUD atlas (Mod Menu's own icon) are resolved by
`FabricBridge::gui_texture`: a `namespace:path` sprite is read from
`assets/<namespace>/textures/gui/sprites/<path>.png` in the loaded mods' jars, decoded by the host
and placed in the atlas's run-time region, the same way registered icons are. Title/pause widgets
and Fabric screens share that upload. Nine-slice `.mcmeta` scaling is not read; such a sprite is
stretched. An identifier no jar provides still renders as a labelled placeholder tile.

`@Accessor` and `@Invoker` interfaces are being repaired and are **not** a working path right
now. The runtime does read them from every mixin class (`mixin::accessors_from_class`, name
inference as Mixin does) and registers them with the interpreter, and a call naming the interface
is redirected to the target's method or field (`Interpreter::redirect_mixin_accessor`). The
current registration is narrow: `MixinAccessor` carries one `target_class` and takes it from
`targets.first()`, so an `@Accessor` interface mixed into several targets is registered for only
the first; a getter reads the receiver's own field map and answers null when the field is absent;
and the invoker path forwards the accessor's own descriptor as the target method's.
`tests/rch_test.rs` asserts that Right Click Harvest's `CropBlockAccessor` is *parsed and
registered*; no test calls one of its methods, so nothing here proves an accessor runs. Treat a
mod that depends on one as unsupported until that work lands.

Reflection that constructs objects (`Class.getConstructor`/`getDeclaredConstructor` and
`Constructor.newInstance`) runs the class's real constructor. That is how JamLib's
`ConfigManager` builds a mod's config class, so Right Click Harvest's `Config` and its enums come
from the mod's own bytecode. Config files are read and written through the runtime's file-system
boundary (`Files`, `File`, `Path`), which is the game folder; JamLib/Jankson and Night Config
configure a mod the way they do in the real game (`tests/config_files.rs` pins the exact bytes
Right Click Harvest and Legendary Tooltips write). Unresolved calls returning defaults is also why a test can pass without a feature
working; check `Interpreter::missing` (set `trace_missing`) before claiming a path runs.

### Exceptions

A thrown value unwinds. `athrow` takes the object and looks for a handler of the *current* frame
that covers the throw point and accepts it through the real class hierarchy; with no such handler
the exception becomes the interpreter's pending exception and the frame ends, so the caller checks
its own covering handlers at the call site (`Interpreter::fault_is_caught`). A `finally` is the
catch-all entry of the exception table, so it runs on the way out. Leaving a class initializer
turns anything but an `Error` into `ExceptionInInitializerError`, as a JVM does.

The exceptions are the JDK's own classes, built by their own `(String)`/`(Throwable)`
constructors, so `getMessage`, `getCause`, `getClass().getSimpleName()` and `toString` are the
real answers. The JVM-defined ones are raised where the runtime used to answer a default:
`NullPointerException` (naming the receiver or field), `ClassCastException`,
`ArrayIndexOutOfBoundsException`, `NegativeArraySizeException` and
`ArithmeticException: / by zero`. A class whose initializer throws fails once with
`ExceptionInInitializerError` (cause attached) and answers `NoClassDefFoundError` after that.

Two boundaries keep the runtime honest without turning its own gaps into mod failures:

- **A fault the interpreter detects** (division by zero, a null receiver, array bounds, a bad cast)
  is only thrown when a handler on the interpreted stack would catch it. On a JVM the fault would
  end the call, but here the null or the stand-in behind it almost always comes from something the
  runtime does not model yet, and throwing it would abort the entrypoint. When nothing would catch
  it, the instruction keeps its lenient answer and the gap shows in `Interpreter::missing`. A cast
  or a field store is only checked when both the value's class and the target are real classes the
  loader has, for the same reason.
- **An exception escaping a host call** (an entrypoint, an event listener, a bridge method) has no
  caller to unwind into, so it is reported as `class.method threw X` instead of being handed back
  as a default result. The one exception is a failure the runtime caused itself (a symbol it
  lacks): that is logged with the symbol and does not poison the class, because a JVM would never
  have taken that path.

A differential suite ([`tests/jvm_diff.rs`](../../crates/hfc-runtime/tests/jvm_diff.rs)) compiles small
Java programs from `crates/hfc-runtime/tests/jvm/` and runs each on the machine's JVM and in the
interpreter over the JDK image the client uses, comparing their labelled output.
[`tests/jvm/known_divergences.toml`](../../crates/hfc-runtime/tests/jvm/known_divergences.toml) is
the maintained list of what the interpreter still gets wrong (Java's float and concatenation
formatting, a `super.field` read, records, immutable collections); it may only shrink, and each
entry is a bug in the general path.

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

GUI pose: `GuiGraphicsExtractor.pose()` returns a real JOML `Matrix3x2fStack` (one per
graphics object; push/pop/translate/rotate/scale run as JOML bytecode, with
`joml.nounsafe=true` so JOML copies fields instead of using `Unsafe`). Every recorded draw
is transformed by the current matrix: fills, gradients and scissors by both corners, a
gradient under a quarter turn is recorded `horizontal`, and text, blits and items move and
scale with it (their rotation is not applied). The item size starts from Iceberg's 16 px
model square, and the host scales its GUI item draw to the recorded size.

Verified live: hovering an enchanted netherite pickaxe shows the gold frame, the item in
its model panel, the title beside it and the horizontal name separator under it; a common
item keeps the normal dark tooltip and no frame. `rotated_gradients_land_inside_the_tooltip`
checks the rotated gradients' placement.
`tests/legendary_tooltips.rs` now also asserts that GATHER adds the item-model and padding
components, that Legendary's HEAD injector padded (centred) the title, that the name
separator fills were drawn, that the model component recorded a `GuiDraw::Item`, and that
the shine timer advances with real time.

Still partial: the model's recorded Y rotation is handed to the host, which draws it turned
through `draw_item_model_rotated` (submodule commit `015d7684c`), but no test asserts the rotation
field — `tests/legendary_tooltips.rs` has no `rotation` hit — so only the placement and the
component list are pinned. The default config's `model_rotation_speed` is 12, not 0. The shine is
recorded with the pose applied but has not been checked live. A component type other than text or
an image
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

## Registered items

Items a mod registers in code (`Registry.register(BuiltInRegistries.ITEM, key, item)`) reach
the host as data: `FabricBridge::registered_items()` returns, in registration order, a
`RegisteredItem { id, template, components, max_stack_size, rarity }` for each. That is the
shape of a Horizon `data/<ns>/horizon/item/<path>.json` file: a vanilla `template` plus a
data component patch (`"minecraft:<type>"` sets one, `"!minecraft:<type>"` with `{}` removes
it). The client turns the list into such files.

How it works. `Item$Properties` is a class the runtime replaces (`item_builtins.rs`): each
method records into a native `ItemProperties` (`bridge/items.rs`) the components vanilla
would set, with vanilla's formulas read from `Item$Properties` and `ToolMaterial`:
`durability` sets `max_damage`, a stack of one and `damage` 0; `pickaxe`/`axe`/`hoe`/
`shovel`/`tool` set `repairable` (`#<repair tag>`), `enchantable`, `tool` (the
incorrect-for-drops rule with `correct_for_drops: false`, then the mineable tag with the
material's speed), `attribute_modifiers` (`base_attack_damage` = damage + the material's
bonus in `f32`, `base_attack_speed`) and `weapon` (2 damage per attack; the axe also disables
blocking for 5 s); `axe`/`hoe`/`shovel` add `block_transformer`; `sword` sets vanilla's
cobweb/sword-tag `tool` rules (`damage_per_block` 2, no creative breaking) and an empty
`weapon`. `Item.<init>(Properties)` copies the record onto the item object (a mod subclass
calls it through `super`), and `Registry.register` stores id and record and returns the
value.

Modelled `Properties` methods: `setId`, `stacksTo`, `durability`, `rarity`, `fireResistant`
(`damage_resistant: #minecraft:is_fire`), `enchantable`, `repairable` (tag or a vanilla
item), `tool`, `pickaxe`, `axe`, `hoe`, `shovel`, `sword`, `food` (the `FoodProperties`
record's nutrition, saturation and can-always-eat, plus a `Consumable` or vanilla's default
eat), `craftRemainder` (`use_remainder`) and `useBlockDescriptionPrefix` (the host looks up
both name forms). Other methods (`component`, `attributes`, `spear`, `equippable`, armor,
`requiredFeatures`, ...) are recorded as unsupported and logged once;
`FabricBridge::unsupported_item_properties()` lists them. The exported item lacks what they
would have set.

Templates. The vanilla tool of the same kind, so the item keeps vanilla's behaviour class
(an axe strips logs, a hoe tills): `diamond_pickaxe` for pickaxes and `tool`,
`diamond_axe`, `diamond_shovel`, `diamond_hoe`, `diamond_sword`. A plain item uses
`minecraft:flint`, which vanilla builds with `new Item(new Properties())` and which carries
only the components every item has (no fuel value or trim material; a food item sets
`minecraft:food` and `minecraft:consumable` from its properties). The patch sets
every component the properties determine and removes (`!minecraft:x`) a component the
template carries that they did not set. `item_model` and `item_name` are not set: Horizon
renames the template's id inside the copied item, which rewrites both. `max_stack_size` and
`rarity` are the struct's own fields; a tool template whose item never set a stack size
reports 64.

Not exported:

- **Behaviour.** A custom `Item` subclass's overrides (`use`, `useOn`, `inventoryTick`, ...)
  do not run in Horizon; only data crosses. A pickaxe or sword needs none (its behaviour is
  its components), an axe, hoe and shovel get vanilla's through their template.
- **Items extending a vanilla item class** (`BlockItem`, `BowItem`, ...): their behaviour has
  no data form, so they are logged and not exported.
- **Tab-wide listeners and tab contents.** `CreativeModeTabEvents.MODIFY_OUTPUT_ALL`
  listeners (they need a `CreativeModeTab` object) and output calls that read the tab
  (`getDisplayStacks`, predicate anchors, `getContext`) are logged once and not answered;
  `FabricBridge::unsupported_creative_tab_calls()` lists them.
- Registration of anything other than items (blocks, entity types, ...).

### Creative-tab placement

`FabricBridge::creative_tab_additions()` returns, in the order the mod made them,
`CreativeTabAddition { tab, item, placement }` (tab and item are `namespace:path` ids).
`TabPlacement` is `End` (`accept`), `Start` (`prepend`; several apply in order, each at the
front), `After(x)` and `Before(x)`. `insertAfter(x, a, b)` is `After(x)` for `a` and
`After(a)` for `b`; `insertBefore(x, a, b)` is `Before(x)` for both, applied in order
(`a, b, x`).

Fabric API's own bytecode runs `CreativeModeTabEvents.modifyOutputEvent` (one `Event` per
tab key in `CreativeModeTabEventsImpl`, built on the `EventFactory` builtins) and the mod
registers on it. When `init` has run the entrypoints, `run_creative_tab_events`
(`creative_tab_builtins.rs`) calls every listener of every tab event with a native
`FabricCreativeModeTabOutput`, a host boundary standing for the tab's item list, that
records each `accept`/`prepend`/`insertAfter`/`insertBefore`. Items resolve to ids as a
mod-registered item object, a vanilla `Items.X` (also as an anchor) or an `ItemStack`'s
item. Only data crosses: stack components on an `ItemStack` are not exported.

A mod can also build and register **its own tab** (`CreativeModeTab.builder()` and
`Registry.register(..., CREATIVE_MODE_TAB, ...)`). The runtime records it by the value's
class, runs its `displayItems` generator into a native item list (`CreativeTabItems`, the
same host boundary as above) and exports a content-mod `data/<ns>/horizon/creative_tab/`
file with the title (the component's English text), the icon item and the items in generator
order. If the icon supplier answers a stack shape the runtime does not read, the first item
stands in as the icon; otherwise the tab is rejected by the content fence and takes its
whole mod down, so the fallback is required.

Verified by `farmers_delight_food_its_tab_and_blocks_are_exported`: Farmer's Delight's own
tab exports its foods and tools, its bacon carries `minecraft:food`, and a live run shows
the tab in the creative inventory. `emerald_tools_creative_tab_placement` covers the
existing tabs; no fixture mod uses `insertAfter`/`insertBefore`/`prepend`, so their ordering
is untested.

Also changed: `BlockTags.X`/`ItemTags.X` constants now carry their real id
(`minecraft:mineable/pickaxe`), read from the tag class's initializer, instead of the
lower-cased field name.

Verified by `tests/registered_items.rs`: Emerald Tools (the mod's `PandoricalApi.isAvailable()`
is an unresolved call that returns false like any missing class) registers five items whose
components match values read from its bytecode; and vanilla's own arguments for six tools
(`Items.<clinit>`: the diamond tools and the fire-resistant netherite sword) produce the
same `tool`, `attribute_modifiers`, `max_damage`, `enchantable`, `repairable`, `weapon`,
`block_transformer` and `damage_resistant` as the client's `items.json`.

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
`modmenu.json` is honoured after a restart). `tests/config_files.rs` covers the TOML
case: Legendary Tooltips relaunched on its own file leaves it unchanged, and an edited
`model_rotation_speed` is read back through the mod's `Supplier<Double>`.

Boxing is by plain value: `Character.valueOf/charValue` and `Double/Long/Float.valueOf`
return the primitive, and `xxxValue()` converts it (a box the JDK constructor made, with
a `value` field, is unwrapped). `Map` operations match `String` keys by text, since a
string built at runtime (`new String(char[])`) is not the interned literal.

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

## Registered blocks

`Registry.register` for the BLOCK registry records the mod's block object (same interception
as items). `Interpreter::collect_block_facts` (called at the end of `init`) then reads facts
by calling the real bytecode: `Block` getters, `StateDefinition.getProperties` and
`getPossibleStates`, `BlockStateBase` getters (destroy speed, light emission, map colour,
sound type, occlusion) and the `defaultBlockState` field. `FabricBridge::registered_blocks`
returns them as `RegisteredBlock`. A fact that fails to read goes into `unreadable`; nothing
is guessed. The BlockItem of each block comes from the item registration (`BlockItem.block`),
with the components its `Item.Properties` set (`item_components`). A state without a map
colour or sound type keeps vanilla's defaults (0 and stone) instead of dropping the block;
properties whose Java initializer did not complete (`BlockStateProperties.HALF`,
`WATERLOGGED`) are answered from their native stand-ins. The facts cache version is bumped
when any of this changes. `overrides` lists the
vanilla methods the mod's own classes override, as `<vanilla class>.<method>` (the most derived
vanilla class in the chain declaring the same name and descriptor; constructors, static and
private methods left out), read from the loaded class files: it tells which vanilla behaviour
the mod replaced (mdm's beds override `AbstractBedBlock.setPlacedBy`).

Shapes are not read in `init`: `Shapes.or` over many fine boxes costs tens of millions of
instructions (three `electric_guitar_*` blocks ~40M each, ~168M for all 344 blocks, ~13 s
release). `FabricBridge::read_block_shapes(ids)` runs `getShape`/`getCollisionShape` with
`EmptyBlockGetter`, `BlockPos.ZERO` and `toAabbs()` for the given ids (empty = all).

That pass is bounded and memoized, because a cold start spends most of its shape time there:

- every call runs under `BLOCK_SHAPE_BUDGET`, and every block under the same total, so one
  block with thousands of states cannot hold the start. A state whose shape does not fit stays
  `None` and the block's `unreadable` says so (the host then keeps the template's shape);
- a `VoxelShape` is immutable, so its `toAabbs()` boxes are read once per shape object for the
  whole pass. States that differ only in a property the shape ignores (waterlogged, lit) and
  blocks whose outline is their collision shape share one read: Farmer's Delight's 2,580 states
  went from 1.4 s to 0.26 s of shape reading;
- a block whose classes do not replace `getShape`/`getCollisionShape` answers the same shapes
  for every state (`BlockBehaviour`'s implementations ignore the state: `Shapes.block()`, and
  that or `Shapes.empty()` from the block-wide `hasCollision`), so one state's pair is read and
  given to the rest (27 of Farmer's Delight's 135 blocks, all their 4 to 8 states).

### The JDK host boundary

A JVM-native method has no bytecode, so the interpreter has to answer it. The rule is the same
as everywhere else: implement it at the boundary with the JDK's own semantics, not a plausible
default. `java.io.File` is real JDK bytecode (path building, `getAbsolutePath`, exceptions); the
natives beneath it are answered from the runtime's file system (`fs_builtins.rs`):
`canonicalize0`, `getBooleanAttributes0`, `checkAccess0`, `getLastModifiedTime0`, `getLength0`,
`createFileExclusively0`, `createDirectory0`, `delete0`, `list0`, `rename0`,
`setLastModifiedTime0`, `setReadOnly0`, `setPermission0`, `getNameMax0` and `initIDs`, for both
`java.io.FileSystem` and `java.io.UnixFileSystem`. A relative path is the mod's, so it resolves
against the game folder, exactly as a JVM process started there would; the folders above the game
folder exist (a walk up `getParent()` terminates) but nothing outside it is listed, read or
written. `File.createNewFile` fails when the file is there, `mkdir` needs its parent, and
`rename` moves one entry.

The rest of the boundary, in the same spirit: `System`'s property table (`user.dir` is the game
folder, `java.io.tmpdir` is `tmp/` under it, `file.separator`/`path.separator`/`java.version` and
the rest as a JVM 25 reports them), `System.getProperties`/`setProperty`/`clearProperty` and
`Properties.getProperty`, `System.identityHashCode` (the heap id, stable and distinct per object),
a monotonic `System.nanoTime`, `Runtime.availableProcessors`, `String.intern` (the interned
string of that text; `new String(s)` is a separate object), `Float.floatToRawIntBits`/`floatToIntBits`/
`intBitsToFloat`, the `registerNatives` calls of `Class`, `ClassLoader`, `Thread` and `Object`,
`Thread.getNextThreadIdOffset`, `BootLoader.setBootLoaderUnnamedModule0`,
`Unsafe.staticFieldBase0`, `VM.initialize`/`getNanoTimeAdjustment`, and `StackWalker`, whose
`walk` reports no frames (its function runs over an empty stream). `getCallerClass` (and
`Reflection.getCallerClass`) read the interpreter's own frame stack, the class of each interpreted
frame. Later additions: `Unsafe.ensureClassInitialized` runs the class's `<clinit>` (how
`SharedSecrets` installs a lazily set access object) and the memory fences do nothing (one
thread); `Thread.sleep` really waits; `System.getenv` answers the process environment;
`System.out`/`System.err` print to the host process's standard output and error (the JVM sets
them in `initPhase1`, which the runtime does not run);
`FileSystems.getDefault()` is the runtime's file-system stand-in and
`FileSystemProvider.installedProviders()` is empty (the runtime's `java.nio.file` is not built
from JDK providers; running the JDK's would load `libnio`). A static field of a JDK class that
its `<clinit>` left null stays null; the runtime no longer invents a value for it.

`crates/hfc-runtime/tests/jvm_diff.rs` runs every case in a temporary folder that is both the
JVM harness's working directory and the interpreter's file-system root, and
`tests/jvm/hfc/cases/Jdk.java` pins these answers against the real JVM: identity hashes, interning,
float bits, processors, monotonic time, the properties, and `File` create/list/canonicalize/
rename/delete (including that two spellings of a path canonicalize the same).

Measured with the census (`--mods test-mods --alone`): **55 missing symbols before, 18 after**.
iceberg 5→0, jamlib 5→0, legendarytooltips 16→1, mdm 21→11, modmenu 7→5, rightclickharvest 1→1.
The next slice (below) took the 18 to 3.

### Collections, streams, lambdas and the built-in registries

The census lists a symbol only where a mod's own code reached it, and most of the 18 left after
the JDK slice had a shared cause underneath. A debug line now names the receiver of every
unresolved call (`Method not found in hierarchy of X: m() (receiver ...)`), which is how these
were found.

- **Runtime collections run as their JDK skeleton.** `ArrayList`/`LinkedList`/`HashMap`/
  `LinkedHashMap`/`HashSet`/`LinkedHashSet` are the runtime's own list, map and set objects.
  Any method the collection builtins do not answer now runs as the bytecode of
  `AbstractList`/`AbstractSet`/`AbstractMap` (and the interfaces' defaults), whose
  implementations are written against `size`/`get`/`iterator`/`entrySet`, which the builtins do
  answer. That gives `listIterator`, `toString` (`[a, b]`, `{k=v}`, also inside string
  concatenation and `StringBuilder.append(Object)`), content `equals`/`hashCode`, `List.sort`,
  `removeIf` and the rest without a builtin each. `Iterator.remove` writes through to the list or
  set the iterator walks. `TreeMap` and `TreeSet` are no longer modelled at all: their order is the
  comparator's, so they run as the JDK's classes.
- **Streams collect with the collector's own functions.** The stand-in `Collectors` (an
  `Instance` of the `Collector` interface with a `type` string) is gone. `Stream.collect(c)` does
  what a sequential pipeline does with any `Collector`: `supplier().get()`, `accumulator()` per
  element, then `finisher()`. `groupingBy`, `joining`, `toMap`, downstream collectors and
  libraries' `Collector.of` therefore run as bytecode. `Stream.max`/`min(Comparator)` reduce like
  `BinaryOperator.maxBy`/`minBy`. The JDK-internal `String.join(prefix, suffix, delimiter,
  String[], size)` that `StringJoiner` reaches through `JavaLangAccess` is answered, since the
  runtime's strings are not byte arrays, and so is `String.toString()`.
- **Comparators are the JDK's.** The stand-in `Comparator.comparingInt` read its key function
  from the wrong argument, so every `comparingInt` comparator called `applyAsInt` on null (the
  census's `ToIntFunction.applyAsInt`). It is gone. A lambda is now unpacked only for its abstract
  method: a call to a method with a body (`Comparator.thenComparing`, or another lambda's
  implementation that takes this lambda as its first argument) runs as itself. Before, such a
  call ran the receiver lambda's target with the wrong arguments.
- **`dup2`/`pop2` know `long` and `double`.** A category-2 value is one entry on this operand
  stack, and the `dup2`, `dup2_x1`, `dup2_x2`, `dup_x2` and `pop2` forms moved two entries where
  the JVM moves one such value. `TimeUnit.MILLISECONDS.toNanos(10)` answered `Long.MAX_VALUE`, so
  `Thread.sleep(10)` would have slept forever. `tests/jvm/hfc/cases/Wide.java` pins each form.
- **String concatenation honours the call site's types.** A `boolean` prints `true`/`false` and a
  `char` its character, in `makeConcatWithConstants` and in `StringBuilder.append(boolean)`.
  `new String(s)` is a new object.
- **The built-in registries take registrations.** `BuiltInRegistries.X` is the runtime's registry
  object for kind `X`. It now answers `key()` (its token, the same one `Registries.X` is),
  `register(key, value, info)` (what `Registry.register` calls), `createIntrusiveHolder` and
  `getKey`. Registrations live in `Heap::registry_entries`, vanilla's own included: `SoundEvents`'
  initializer registers its sound events, so a `SoundEvents` constant is a registered
  `Holder.Reference` whose key names the sound (an armor material's equip sound is read from it).
  A `Holder.Reference` carries `Holder.Reference`'s own fields, so its bytecode (`unwrapKey`, `is`)
  reads them. The vanilla contents of registries other than items and blocks are not known to the
  runtime. `getKey` of an object nobody registered answers null, and `iterator()` stays
  unanswered, because a partial iteration would be a lie.
- **Items from blocks.** `Item.byBlock` (what `Block.asItem()` asks) answers the `BlockItem` a mod
  registered for the block, as Fabric's registry sync fills `Item.BY_BLOCK`, or the vanilla item
  of the same id, or `Items.AIR`. `new ItemStack(itemLike[, count])` builds the runtime's native
  stack. Together they let a creative tab generator's `output.accept(block.asItem())` record the
  item (mdm's 339 tab entries were dropped before).
- **Fabric Loader's `CustomValue.getType()`** answers the `CvType` constant (an interned name, so
  `== CvType.OBJECT` compares like the enum), and `getAsNumber` is answered. Mod Menu now reads
  the `library` badge in a mod's `custom.modmenu` and hides library mods by default, as it does on
  a JVM.
- **`ServiceLoader`** reached as an `Iterable` (Log4j's spliterator) iterates the providers its
  `META-INF/services` files name.

Census after this slice: **3 missing symbols** (from 18), all mdm's: iceberg 0, jamlib 0,
legendarytooltips 0, mdm 3, modmenu 0, rightclickharvest 0. mdm's three are one gap:
`Fluids.<clinit>` iterates `BuiltInRegistries.FLUID` (`DefaultedRegistry.iterator()`), and the
runtime answers no registry iteration it cannot make complete. `tests/jvm/known_divergences.toml`
went from 32 entries to 16; the new `Wide.java` case and the additions to `Jdk.java` (environment,
`getCallerClass`) and `Lambdas.java` (`Stream.max`/`min`, a reverse-ordered `TreeSet`) match the
JVM.

Cost, measured with `block_profile` on Fabric API + Farmer's Delight (cold, three runs each):
init with facts 3.5 s before, 3.8 s after; shapes 250 ms before, 262 ms after. Most of it is
initialization that now completes (vanilla `SoundEvents` registers its ~1,700 sound events, the
JDK's charset and class-loader initializers run); the interpreter's frame-class stack, which
`getCallerClass` reads, reuses its buffers and costs about 0.1 s of it. A lambda call checks the
method it implements by name first, so the body lookup above runs only for other methods.

### Shape maps keyed by an enum

Minecraft caches rotated shapes in `java.util.EnumMap`s keyed by `Direction` (`Shapes`
rotate helpers, `WallSignBlock.SHAPES`, `Direction.COUNTER_CLOCKWISE`, ...). `EnumMap` reads an
enum's constants through `SharedSecrets.getJavaLangAccess()`, which on a JVM is filled in by the
bootstrap (`System.initPhase1`), not by any class initializer, so the runtime installs the JDK's
own `JavaLangAccess` (`java/lang/System$1`) when `SharedSecrets` initializes. Without it those
maps were built over no constants: `Map.get(Direction.NORTH)` answered null for every facing,
`getShape` answered null for 448 of Farmer's Delight's 2,580 states (signs, pies, baked feasts,
baskets, beds) and the host fell back to the template's shape, and `Direction.getClockWise`/
`getCounterClockWise` (which `BlockBehaviour.rotate`/`mirror` reach through
`Rotation.rotate`/`Mirror.mirror`) answered null, so rotating a block's state could not produce
the rotated facing at all. `crates/hfc-runtime/tests/vanilla_shape_maps.rs` pins the vanilla
wall-sign table per facing, and the Farmer's Delight test pins the canvas wall sign's per-facing
shapes and that every state of every block it registers now has one.

General runtime fixes this needed: `Collection.toArray(T[])`, structural List map keys,
adopting bytecode collections (Guava `ImmutableList`) into heap lists in `copyOf`/copy
constructors, three-argument `Stream.collect`, `Object.clone` on plain instances and
`Integer.toString()`/`Long.toString()` as instance methods.

### Blocks in the client (stage B: decoration)

`workspace/horizon` turns each `RegisteredBlock` into a Horizon block declaration in the
generated content mod (`game/fabric_bridge/content/blocks.rs`, `data/<ns>/horizon/block/`).
The template follows the vanilla class: a vanilla block of the block's first vanilla class
with the same states, unless the mod overrides that class's behaviour (`overrides`, minus the
methods the declaration carries from facts: shapes, codec, state definition, rotate/mirror);
then the next vanilla class up; at `Block`, a plain vanilla block (class `Block`, one state,
not flammable, no fluid/ticks/offset/block entity) with the same sounds where one has them.
A block whose kept vanilla behaviour needs states no vanilla block of its class has is left
out. The declaration carries the default state's hardness, resistance, light, tool need,
friction, map colour, sounds and `can_occlude`, the block's own state properties, and its
outline and collision per state (`shapes`/`collision` keyed by the fewest properties they
depend on; over 16 boxes are merged into a cover). Shapes are read once per run on the
content-root thread (about 15 s for mdm's 344 blocks in the fast build). Block entities' own
code and ticks are not run; use is stage C3 and placement stage C1.
Drops come from the jar's own `data/<ns>/loot_table/blocks/` files through Horizon's loot
engine. Tables written in an older format (a `conditions` list of `{"condition": ...}`) roll
as 26.3 rolls them: its codecs read only `condition`, so the old list is an ignored field.

### Placing a registered block (stage C1)

The host asks the bridge for the state of a block a mod registered
(`FabricBridge::state_for_placement`, called from `Game::fabric_state_for_placement` in the
server's `before_state_for_placement` hook) and, once it placed it, for the other blocks the
mod sets (`FabricBridge::block_placed`, from `fabric_block_placed` after `native_place_block`).
Only blocks whose `overrides` name `getStateForPlacement` / `setPlacedBy` / `onPlace` run mod
code; the rest keep the host's placement.

The mod's real bytecode runs with a native `BlockPlaceContext` (`place_builtins.rs`) built from
the click: clicked position, face and hit point, the placer's yaw, pitch, position and sneaking,
the item, `replacingClickedOnBlock`, and the real `Direction` constants. The `Level` is a
snapshot of the host's blocks within 2 of the position (`HostBridge::block`, `block_properties`);
a mod block is its own `BlockState` object, so the mod's `getBlockState(..).getBlock()` works. A
null result refuses the placement; the state maps back to Horizon's by its property values.
`setBlock` calls made by `setPlacedBy`/`onPlace` are collected and set through
`native_set_block`. Budget 20M instructions per call; a call over 50 ms is logged.

Limits: the hand is always the main hand (the hook does not carry it); the world border and
build height are the overworld's defaults; `canBeReplaced` of a vanilla block is air or the
`minecraft:replaceable` tag; no block entity is created or given the stack's data by
`setPlacedBy`; `getStateForPlacement` is not run on a client that predicts.

### Containers (stage C2)

`RegisteredBlock::container` (`container_builtins.rs`, read in `collect_block_facts`) runs the
block's real `EntityBlock.newBlockEntity` on its default state and asks the entity:
`getContainerSize()`, `getDisplayName()`, `getMaxStackSize()`, the menu `createMenu(0, Inventory)`
builds (its `AbstractContainerMenu.getType()` matched against `MenuType`'s constants) and, for a
`WorldlyContainer`, `getSlotsForFace(Direction)` for each of the six directions. A block entity
that does not extend `BaseContainerBlockEntity` is `NotVanillaContainer(class)`; anything that
failed is `Unreadable(reason)`. `title` is the text `getDisplayName()` answered (a literal, or the
mod's own language file's text when it used a translation); `title_key` is the translation key
when the component was a `Component.translatable` (the runtime keeps it on the component it
builds). `max_stack_size` is `getMaxStackSize()` (1 to 99; `None` with a note when it could not be
read or is out of range, and the host then keeps its own limit). `faces` is
`getSlotsForFace(Direction)` in `Direction` order (down, up, north, south, west, east), slots
outside the container dropped; `None` when the entity is not a `WorldlyContainer`.

What is not a fact: `canPlaceItem(slot, stack)`, `canPlaceItemThroughFace(slot, stack, face)` and
`canTakeItemThroughFace` depend on the stack, so they cannot be read once; they are reported as
not reproduced and the declared slots take and give anything the stack permits.

The client writes `"container": {slots, menu, title|title_key, max_stack_size?, faces?}` into the
block declaration only when Horizon has that menu with that slot count (`containers::MOD_MENUS`),
and a title only when it is not the block's own `block.<ns>.<path>` key (a `title_key` is
preferred over the literal). `max_stack_size` and `faces` are written only when they differ from
the host's defaults (64, every slot from every face). Horizon's side is documented in
`workspace/horizon/docs/mods.md` (a block's `container`): the content mod's block gets the world
class `HorizonContainerBlock` (opens its menu on use), a mod block entity of its own id that
holds the slots, hopper and comparator access, drops on break, the slots each face reaches and
the stack size its slots hold (`ModContainer::max_stack`, enforced by the menu and the hopper
logic), all through the machinery native mod containers use. Not run: the block entity's own
code (ticking, per-stack slot rules, loot tables).

mdm (344 blocks): 130 are 27-slot `generic_9x3` containers (storage furniture; each block
entity extends `RandomizableContainerBlockEntity`; `getMaxStackSize()` is 8 and every slot is
reachable from every face), 211 have `DecorationPlacementBlockEntity` and 2 have
`WallClockBlockEntity` (neither is a container; they stay decoration), 1 (`furniture_part`) has
no block entity.

### Using a registered block (stage C3)

`FabricBridge::block_used` (from `right_click_block` in `bridge/events.rs`, after the Fabric and
JamLib listeners pass) runs a registered block's own `useItemOn` (when the hand holds an item)
and `useWithoutItem` through `use_builtins.rs`. It uses the same `Level` snapshot, placer and
`UseOnContext` placement builds (`place_builtins.rs`), at the state the host's block stands in.
Only blocks whose `overrides` name those methods run mod code. Sneaking with an item in either
hand skips the block's own use, as `ServerPlayerGameMode.useItemOn` does, so sneak-placing works.
`InteractionResult.SUCCESS` and `FAIL` consume the click; an empty `useItemOn` result or
`TRY_WITH_EMPTY_HAND` lets `useWithoutItem` run on the main hand; `PASS` goes to the item's own
use. An `InteractionResult` the runtime cannot name is reported and fails the use, never treated
as PASS. A block whose block entity is a `Container` keeps the host's container path for the
empty hand (its `useWithoutItem` usually only calls `openMenu`, which the host does itself); an
overridden `useItemOn` still runs, so a container that reacts to an item works.

What the code changed is applied through the host as `BlockEffects`: the states it set
(`Level.setBlock`/`setBlockAndUpdate`, by property), the items it dropped (`popResource`,
`dropStacks`), the blocks it removed (`removeBlock`, `destroyBlock`, with the drop flag), the
block ticks it scheduled and the particles it sent. A state write that replaces the block with
another one, a position both set and removed, or a removal on a host without `remove_block` is
reported and leaves the host's own click path in charge (`BlockUse::Failed`), never half-applied.
A use that changes the held stack (`shrink`/`consume`/`setItemInHand`, detected on the one stack
object the hand and the use share) is refused the same way until a host hook can apply it.
`RightClickBlockEvent` carries the hit point, whether the player is sneaking, and which hand acts
(the item is the one in that hand). Budget 20M instructions per call; a call over 50 ms is
logged.

Not run: neighbour updates
(`updateShape`, `neighborChanged`), block-entity data and per-stack container rules. mdm's two
non-container block entities (`DecorationPlacementBlockEntity`, `WallClockBlockEntity`) declare
no fields and no methods (javap): they store nothing to save. What a wall clock draws (the level
time) and where a decoration places itself are the mod's renderer, which the host does not run;
those blocks stay decoration, as their declarations say.

### Block entities (stage C4)

A registered block that implements `EntityBlock` has a block entity made by its own
`newBlockEntity` (`block_entity_builtins.rs`). The runtime keeps the mod's real object per
position (`Heap::block_entities`), so its fields, its inventory and its menu are the mod's code;
`Level.getBlockEntity` on the snapshot answers it, `BlockEntity.setLevel` is given the snapshot's
level, and a use or tick creates one if the runtime has none yet. The host makes one when a block
is placed and when it is right-clicked (`FabricBridge::ensure_block_entity`).

`getTicker` is called with the entity's own `getType()`; the `BlockEntityTicker` it answers is run
as `tick(level, pos, state, entity)` once per game tick for every kept entity whose block is still
there (`FabricBridge::tick_block_entities`, from `Game::tick_fabric_block_entities`). A tick's
effects go through the host exactly as a block tick's do.

Each tick runs inside a heap scope, and closing it asks whether the mod's code stored a fresh
object into one the heap keeps across calls. A scoped id did not exist when the scope opened, so
only a write this scope made can have put one into an older object: the heap's write barrier
notes every `get_mut` into a pre-scope object and the scan walks those, not the heap. Before that,
the scan walked every object on the heap: a Farmers Delight cooking pot's tick measured ~8 ms
(7.0–12.7 ms of it in the scan alone) on a 306k-object heap, and ~0.10 ms after (measured with
`tests/block_entities.rs`, which fails if the scan grows back with the heap). `TICK_BUDGET` (2M
instructions) still bounds each tick.

Saving runs the entity's own `saveAdditional` into a `ValueOutput`, and loading runs
`loadAdditional` from a `ValueInput` (`value_io_builtins.rs`). The storage interface is modelled
as a value tree (`putInt`, `getIntOr`, `putString`, `putBoolean`, `putIntArray`, `child`,
`childOrEmpty`, `discard`, `isEmpty`); the host stores it in the block entity's compound under
`HorizonFabric` (`game/fabric_bridge/block_entity_nbt.rs`) before a chunk is snapshotted, and
feeds it back on load.

Tested with Farmers Delight's cooking pot (`tests/block_entities.rs`, real jar): the entity is its
own `CookingPotBlockEntity` with a 9-slot `ItemStackHandler`, `useItemOn` runs on an empty hand and
consumes the click, the ticker runs `cookingTick` (its `cookTime` field was 10 and the tick left
8), and `saveAdditional`/`loadAdditional` round-trip `CookTime`/`CookTimeTotal`.

### A block's own menu (stage C4)

`Player.openMenu(provider)` records the provider on the snapshot's `Level`; the bridge asks it
for the menu (`Player.openMenu`'s provider is the block entity, whether it implements
`MenuProvider`'s two-argument `createMenu(int, Inventory)` or Fabric's three-argument
`createMenu(int, Inventory, Player)`), reads `AbstractContainerMenu.slots` for the stacks shown and
each `Slot.mayPlace` for the slots that take a click, and reports it as `BlockEffects.menu`. The
host takes it once (`FabricBridge::take_opened_menu`) after applying the use, and shows it with
the title `getDisplayName` answered. Two ways, by what the client knows:

- **The mod's own screen, when the client has one.** The menu's type is looked up in the
  client's own `MenuScreens.SCREENS` (vanilla's map for vanilla menus; `ExtendedMenuType`
  registrations add mod entries). When an entry exists, the runtime calls the registered
  `ScreenConstructor.create(menu, inventory, title)` directly with the already-built menu
  (not the network `fromPacket`, which would re-create the menu from a packet) and shows it
  with `Minecraft.setScreen`. The host follows that screen (`Game::fabric_follow_screen`,
  `Screen::Fabric`) and replays its display list; it draws no host grid and writes no host
  items. A click runs the real screen's slot code, which writes the block entity's own
  inventory, so the block's `saveAdditional` (run by `Game::fabric_save_block_entities`)
  persists what the player put in with no host-side copy. This is the general path: it
  branches only on whether the menu's own `getType` has a `MenuScreens` entry, so it works
  for any mod that registers a screen for its menu type.
- **The host's grid, when the client has no screen for the type.** The host draws a vanilla
  grid for the slot count (a `MOD_MENUS` lookup); a menu whose slot count no host screen
  draws is reported and not shown. The screen's slots are written back into the block
  entity's own inventory through `FabricBridge::sync_menu_items`, so a cooking tick reads
  what the player put in.

Tested with the cooking pot (`tests/block_entities.rs`, real jar): its own `CookingPotMenu`
builds (45 slots, titled "Cooking Pot", its result slot outside the slots a click may fill);
opening it installs the mod's own `CookingPotScreen` (built by the registered `ScreenConstructor`
for the pot's `MenuType`) and rendering it draws the pot's own `cooking_pot.png` background; and
a click's stack reaches the pot's `ItemStackHandler` (slot 0 read back as `minecraft:beef` x2).
In the game the pot opens its own screen (background, bowl slot, recipe-book button, title)
instead of a double-chest grid, and reopens with the pot's contents, before and after a save
and reload.

A slot click moves a stack. The click reaches the menu through the client's own path:
`AbstractContainerScreen.mouseClicked`/`mouseReleased` resolve the slot and call
`gameMode.handleContainerInput(containerId, slot, button, input, player)`, which reads
`player.containerMenu` and runs that menu's `clicked` (so the menu's own slot rules apply and the
block entity's inventory changes, which `saveAdditional` then persists). Three host boundaries had
to answer for that path, and each is general rather than pot-specific:

- `Player.containerMenu` is the menu the open screen shows; `Minecraft.setScreen` records it, and a
  native `Player` answers it through `Interpreter::native_field`.
- `ItemInstance.getMaxStackSize` (vanilla's default method) reads
  `DataComponents.MAX_STACK_SIZE` from the item's components. `DataComponents` is synthesized from
  the host's `items.json` (`Registries::item_stack_size`, a mod's own `stacksTo` first), so vanilla's
  own item code computes the real size. Answering the fallback of 1 made
  `ItemStackHandler.extractItem` take its partial branch, which *destroyed* the stack instead of
  returning it.
- `ItemStack.typeHolder` answers a real `Holder` for the stack's item (and `Items.AIR` for an empty
  stack), and `Level.blockEntityChanged`/`sendBlockUpdated`/`blockChanged`/`markUnsaved` complete:
  a container's `setChanged`, run after the click moves a stack, otherwise died on an unmodelled
  chunk.

Tested with the cooking pot (`tests/block_entities.rs`, real jar): a press and release on the pot's
own slot 0 leaves the menu carrying `minecraft:beef` x5 and the pot's own inventory empty — the
stack moved, rather than being dropped or silently destroyed.

Not run: `preRemoveSideEffects` (a pot dropping its meal on break); codec-based save data
(`ValueOutput.store`/`storeNullable`/`read`/`list` take a `com.mojang.serialization.Codec`, which
the runtime does not implement, so a slot list written through
`ContainerHelper.saveAllItems` is not saved and a container's own contents do not survive a
reload); the player-inventory side of a real screen (the host's player inventory is synthetic
and empty, and `Inventory.getDisplayName` is unhandled, so a screen's player slots and its
"Inventory" label draw empty); and the recipe check a cooking tick makes, which needs
`RecipeManager` and the mod's recipe type, so a pot's recipe book opens but a pot cooks nothing
yet.
 2 ticks while lit). The runtime records the call on its `Level`
snapshot; `HostBridge::schedule_block_tick` hands it to the host, which keeps it per dimension and,
when the game tick is due, runs `FabricBridge::tick_block`. That runs the block's real
`Block.tick(state, ServerLevel, pos, RandomSource)` over the host's blocks around it, under
`TICK_BUDGET` (2M instructions; a tick over 20 ms is logged). What the tick set, scheduled,
dropped, removed and sent is applied through the host the same way a use's effects are: property
writes, the next ticks, `ServerLevel.sendParticles` bursts, item drops and block removals. At most 64 scheduled mod ticks run per game tick; the rest
move to the next tick. A schedule whose block is no longer there is dropped.

`ParticleTypes` constants are synthesized from their field names (`FALLING_WATER` is
`minecraft:falling_water`): the runtime does not build the particle registry. `Level.getBlockEntity`
answers null, as it would in a chunk with no block entities. The host maps the id to its own
particle engine (`ParticleType::from_name`) and sends the burst to the players within 32 blocks
through a `ClientboundLevelParticlesPacket`, so the window and LAN guests see it. As in vanilla,
the client's **Particles** option gates it: at *Minimal*, a normal type such as `falling_water`
is not drawn (only particles that override the limiter are), so a running block that draws with
particles looks inert until the option is *All* or *Decreased*.

Fabric ticks go through the level's vanilla `Ticks` scheduler rather than a list of their own.
`HostBridge::schedule_block_tick` resolves the block's default state and schedules its `BlockId`
at the game time plus the delay, so a position/block pair is deduplicated, the schedule follows
the dimension, and it is saved in the chunk's vanilla `block_ticks` list (the `i` field is the
block's name, which includes content-mod blocks) and restored when the chunk loads. When the
scheduler makes one due, `GameLevel::before_tick_block` queues it after native mods had their
chance, and the server thread runs `FabricBridge::tick_block` right after the level's own block
ticks. At most 10 ms of Fabric ticks run in one game tick; the rest are rescheduled one tick out
(vanilla runs every due tick, so this is a deliberate difference).

Measured: a running shower head saved `block_ticks: [{i: mdm:shower_head, x: -20, y: 70, z: 9,
t: 1}]` in its chunk, and after `save`/`quit` and a relaunch it sprayed without a click. A
Fabric container block's items (a cabinet with 5 stone) survived the same reload.

mdm's shower head is the worked example: right-click toggles `running` and schedules a tick 2 ticks
out; each tick sends one `minecraft:falling_water` particle at each outlet and re-schedules itself;
turning it off returns before sending. One such tick costs about 0.95 ms in release (measured
through `FabricBridge::tick_block`, the `Level` snapshot included).

A tick runs in an allocation scope. `Heap::begin_scope` marks the heap end; after
`effects_of` has turned the level into Rust values, the objects allocated during the call are
freed unless something kept one. The escape scan looks at what the runtime keeps between calls
(interned ids, singletons, registered items/blocks, the block-state cache), every loaded class's
static fields, the interpreter's static cache, objects older than the scope, the bridge's own
hovered ids, and the heap ids the runtime's native objects hold (`Level`, `Player`,
`PlaceContext`, `BlockHitResult`, `EventResult`, `GuiGraphics`). A first tick that runs a class
initializer keeps its scope; later ticks do not. Measured with the shower head: 1,000 ticks left
the heap at 309,646 slots, exactly as after the warm-up tick; the same run without the scope
added 4,000 slots (four per tick: the `Level` snapshot, a `BlockPos`, a `RandomSource` and one
more object the mod's code allocates).

### Load time (stage D): the block facts and shapes cache

`FabricBridge::init` reads the facts of every registered block out of the mod's own objects
(`collect_block_facts`), and the content root then reads every block's shapes
(`read_block_shapes`). Neither result depends on anything but the classpath: the entrypoints
always run, because C1 placement, C2 container reads and C3 use call the mod's code at play time
and need the block objects, but the facts read back out of those objects are the same on every
start with the same jars.

`bridge/block_cache.rs` keys the facts on the SHA-256 of every JAR the interpreter loads (client
jar, libraries, every mod jar), the identity (`len` and mtime) of the JDK image, the version of
`hfc-runtime` and `BLOCK_FACTS_CACHE_VERSION`. The file is the game folder's
`.horizon-cache/fabric-blocks/facts-<key>.json`, written atomically (`hfc_core::atomic_write`),
and an older classpath's file is removed. A hit skips `collect_block_facts` and
`read_block_shapes` and serves the cached blocks (shapes included); a miss reads them from the
mod again and rewrites the file. Anything that does not read back exactly (a jar that cannot be
hashed, a missing, damaged or differently keyed file) is a miss, never a guess. The key cannot see
the game folder's `config/`, which can change which blocks a mod registers, so a load also checks
that the ids the entrypoints registered this start are exactly the cached list (same order and
length). The other facts are trusted to depend only on the jars.

The facts a cache hit serves are pinned equal to a fresh read by
`crates/hfc-runtime/tests/block_facts_cache.rs` (every block, state, shape and note, plus the
jar-set miss cases). A cold start logs the facts it read and the shape progress
(`reading the shapes of N registered block(s)`, then a line per quarter); a hit logs
`block facts: N block(s) read from the cache`.

Measured (release runtime, `fast` client build, offscreen, `horizon-ctl launch --wait`; the same
scratch game for the cold and warm runs, a fresh one without mdm):

| Start | mdm | Wall time |
|---|---|---:|
| Cold (first start with the jar set) | yes | 23.6 s |
| Warm (facts and shapes cached) | yes | 8.9 s |
| Cold, no block mod (Fabric API only) | no | 2.6 s |

Inside the cold start: mdm's entrypoint 5.1 s, facts 0.6 s, shapes 13.8 s, declarations 0.15 s.
Inside the warm start: the same entrypoint 5.1 s, facts read back from the cache in 74 ms (key
hashing included), no shape reading, declarations 0.16 s. The export (`content mod`) and the
declaration run once per start in these runs, not twice. Re-measured after the shape pass was
memoized and bounded: cold 23.6 s, warm 9.8 s (the entrypoint 46.1M instructions / 5.1 s
either way), mdm's 344 blocks in 13.0 s, which is mostly the three `electric_guitar_*` blocks
(~39M instructions each) and the shape algebra the mod's own code runs.

Farmer's Delight (135 blocks, 2,580 states; release runtime, same scratch-game method, cold
start measured before and after the shape pass was memoized and bounded): facts 0.14-0.15 s
either way; shapes 0.61-0.64 s before, when 448 of the 2,580 states answered null and every one
of them cost a full call, and 0.24-0.27 s after; declarations 0.12-0.14 s (132 of 135 blocks
declared — the three crop blocks have no default state); `onInitialize` 25.2M instructions
(2.8 s) before and 33.7M (3.0 s) after, because the mod's own code now runs the parts of its
initialization that the empty shape maps used to cut short.
`cargo run --release -p hfc-runtime --example block_profile -- <jars...>` prints the same
phases without a client and, with `RUST_LOG=debug`, one line per block with its states,
instructions and time: Farmer's Delight is dominated by `stuffed_pumpkin_block`,
`shepherds_pie_block`, `honey_glazed_ham_block`, `cooking_pot` and the two `basket` blocks
(0.5M-1.5M instructions each); mdm by the three `electric_guitar_*` blocks (~39M each, 3.6 s
of the 13 s).

What a hit cannot skip is `MdmMod.onInitialize` (~46M instructions, release ~5.1 s): it builds the
`Block` objects the later stages run. That is the warm start's remaining gap to a game without
the mod (8.9 s against 2.6 s). It is logged when an `onInitialize` uses over 1M instructions.
