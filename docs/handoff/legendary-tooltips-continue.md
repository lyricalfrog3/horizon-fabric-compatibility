# Handoff: finish Legendary Tooltips tooltip rendering

You are continuing work on **Horizon Fabric Compatibility** at
`/home/daniel/Desktop/horizon-rust-fabric`. Read this whole file before editing. The
original handoff is `docs/handoff/legendary-tooltips.md`; this file supersedes its status
section.

## Where the work stands

The mod's textured frame, rarity colors, shadow, centered title, name separator, item-model
component and shine timing are implemented. The last live checks still looked wrong, and two
fixes for those reports are **uncommitted and not yet live-verified**. Start by verifying
them.

Current Git state:

- Root repository (`lyricalfrog3/horizon-fabric-compatibility`, `main`): last pushed commit
  `6e288c0` ("Render Legendary Tooltips from the real client component list"), on top of
  `f9c8103`. Uncommitted at the root:
  - `crates/hfc-runtime/src/bridge/events.rs`: the host line color now **overwrites** a
    component text sequence's color instead of `or_insert`. The centered title was being
    drawn in color 0 (black) and therefore invisible.
  - `crates/hfc-runtime/tests/legendary_tooltips.rs`: asserts the title draw keeps
    `0xFF55FFFF`.
- Horizon client (`workspace/horizon`, separate Git checkout): last commit `8351baa9d`
  ("Draw Fabric tooltips from their component list"), **not pushed**. Uncommitted:
  - `crates/horizon-client/src/game/stations.rs`: `tooltip_box` forces the host background
    to alpha 255 (`(background & 0x00ff_ffff) | 0xff00_0000`). The live screenshot showed
    inventory slots/hotbar through the tooltip because the background kept the mod's
    gradient alpha while Iceberg's background-cancelling mixin is not applied.

The client binary `workspace/horizon/target/fast/horizon-client` was rebuilt with both fixes.
The user has not tested after the opacity fix.

## What is implemented (and tested)

Runtime (`crates/hfc-runtime`):

- `bridge/events.rs`
  - `dispatch_event` calls every listener on an Iceberg event (GATHER, COLOREXT, POSTEXT,
    RenderTickEvents.START).
  - `dispatch_render_ticks` sends a real `DeltaTracker` (`getRealtimeDeltaTicks`) with the
    host time since the previous tooltip frame. Legendary's shine and model timers read it.
  - `tooltip_component_list` wraps the host's text lines in `FormattedText`, dispatches
    GATHER, and converts the returned `Either<FormattedText, TooltipComponent>` list to
    `ClientTooltipComponent`s. `Tooltips.getClientComponent` is called by name (real library
    bytecode) for right-side components.
  - `run_head_injectors` runs **non-cancellable** `@Inject(at = HEAD)` callbacks with the
    real receiver and arguments. At `GuiGraphicsExtractor.tooltip` this centers Legendary's
    title and inserts its title-break component. Cancellable HEAD callbacks still need the
    real target body and are skipped.
  - `apply_tooltip_line_colors` gives component text the host line colors; the host color
    wins (do not revert to `or_insert`).
  - `item_tooltip_render` measures components, records `extractText` and `extractImage`
    into a `GuiGraphics` display list, appends POSTEXT, and returns `TooltipRender`
    (width, height, draws) relative to the tooltip text origin.
- `bridge/mod.rs`: `TooltipRender`; `last_render_tick`; GUI texture sizes read from mod
  jars at load (`assets/*/textures/gui/*.png`).
- `builtins.rs`
  - `GuiDraw::Item` from `GuiGraphicsExtractor.item/fakeItem` and from Iceberg's
    `CustomItemRenderer` (`renderDetailModelIntoGUI`, `renderItemIntoGUI`,
    `renderItemModelIntoGUIWithAlpha`). The quaternion's Y angle is decoded from `y`/`w`.
  - `TextureManager.getTexture` / `GpuTexture.getWidth/getHeight` from the jar PNG size.
  - `FormattedCharSequence.accept` feeds a `FormattedCharSink`; sequences carry a color;
    `FormattedCharSequence` factories keep text and color; `Language.getVisualOrder`
    returns a colored sequence.
  - `new ArrayList(collection)` copies; `StringBuilder.append(char[])` appends chars
    (Iceberg's title recomposer uses both).
  - `List.of`/`Set.of`/`Map.of`/`copyOf` return the runtime's collections.
  - `Either.left/right` distinguish the static factory (descriptor starts with
    `(Ljava/lang/Object;)`) from the instance accessor (returns `Optional`).
  - `ItemStack.getMaxDamage/getDamageValue/isDamageableItem` use the host-provided
    `max_damage` (required for Legendary's `render_item_model = "EQUIPMENT"`).
  - `DeltaTracker.getRealtimeDeltaTicks/getGameTimeDeltaTicks`.
- `interp.rs`: `ItemStack.getRarity()` resolves the real `Rarity` enum constant from the
  host's rarity; inherited static fields resolve to their declaring class; inherited
  collection storage dispatches correctly; mixin-added interface methods (from `f9c8103`).
- `minecraft.rs`: `GuiDraw::{Fill, Gradient, Text, Item, Blit, Scissor, ScissorOff}`.
  Blits carry normalized sub-rect UVs; gradients carry both colors.

Client (`workspace/horizon`):

- `game/fabric_bridge/mod.rs`: `fabric_tooltip_render` (component list + POSTEXT),
  `fabric_tooltip_frame` (fallback when no Iceberg events), `stack_rarity`.
- `game/stations.rs`: when `fabric_tooltip_render` returns `Some`, geometry uses the
  component width/height, the component/`POSTEXT` draws are translated to the tooltip
  origin and replayed, and the host's own text renderer is skipped. The host background
  stays under a transparent mod background and is forced opaque.
- `game/fabric_bridge/gui.rs`: `replay_fabric_draws(batch, atlas, draws, game)` replays
  `GuiDraw::Item` through `Game::draw_item_model`; `translate_draw` handles `Item`.
- Replay call sites updated in `game.rs`, `startup_menu.rs`, `fabric_bridge/menu.rs`.

Tests (`crates/hfc-runtime/tests/legendary_tooltips.rs`, 3 tests, all passing):

1. Epic item: frame sub-rect blits, GATHER adds the model/padding components, the title is
   padded by the HEAD injector and keeps its color, the separator fills and `GuiDraw::Item`
   are recorded.
2. Common item: no frame, non-zero background/border colors.
3. Two renders with real time between them advance `TooltipDecor.shineTimer`.

## What is still wrong / not done

**Verify these first.** The user's last report ("still very bad") was before the title-color
and opaque-background fixes. If the background is still transparent, instrument
`tooltip_box` with a temporary `eprintln!` of `background`/`mod_tooltip` and ask the user to
hover; remove the print before committing. Check for leftover `DBG`/`eprintln` with
`grep -rn "DBG" crates/hfc-runtime/src crates/hfc-runtime/tests workspace/horizon/crates/horizon-client/src`.

1. **Inline model placement.** `ItemModelComponent` is 22×22 but reports `getWidth=22`,
   `getHeight=4`. Vanilla draws all text first and then all images, so the model panel is
   painted over the title's first 22 px; the title only clears it because Legendary's
   centering adds leading spaces. `Tooltips.InlineComponent` is a marker used by Iceberg's
   `calculateRect`, not by the vanilla draw path. If the title is still covered, implement
   inline layout in the host: find the inline component, reserve its width on the first
   title line, and shift that line right / reduce its centering width. Do not special-case
   Legendary by name; use the marker interface.
2. **Model rotation.** `GuiDraw::Item` records the Y rotation, but the host ignores it:
   `Game::draw_item_model` draws the standard GUI item (16×16). The HUD has no 3D/rotation
   transform. To finish, add a rotated/scaled item draw to `item_render::gui::draw` or a
   pose transform to `HudBatch`, and use `GuiDraw::Item.size/rotation`.
3. **`model_rotation_speed` config read-back.** The temp config file is written with
   `model_rotation_speed = 12`, but the in-memory supplier returns a `java/lang/Double`
   whose `value` is `0.0` (seen while testing the rotation timer). The live config currently
   has `model_rotation_speed = 0`, so the model is static regardless. Fix Night Config /
   Iceberg double read-back if rotation should follow the file. Do not fake the value.
4. **Shine orientation.** RenderTickEvents.START advances `shineTimer` (tested), but
   `GuiHelper.drawGradientRectHorizontal` rotates through
   `GuiGraphicsExtractor.pose()`; there is no pose-stack builtin, so the call returns null
   and `fillGradient` records a vertical `GuiDraw::Gradient`. The shine appears vertical.
   Implement a minimal GUI pose stack (push/pop/translate/rotate) for fills/blits, or add a
   horizontal gradient flag recorded from the rotated call, and replay it horizontally.
5. **Tooltip layout/padding.** The component heights (model 4, padding 12, title-break 0,
   10 per text line) come from the mod. The host box and frame use `max(component width,
   host width)` and `max(component height, host height)`; check the visual gap between the
   title and the next line and the frame alignment. Bundle tooltips and the host's own
   `native_tooltip_component` are still drawn separately after the component draws and may
   be mispositioned.
6. **Color mapping by line index.** `apply_tooltip_line_colors` maps text components to
   host lines by order. Legendary's compact mode can remove `item.modifiers.` lines; our
   `MutableComponent`s do not implement `getContents`/`TranslatableContents`, so that
   removal likely does not happen, but if line removal is added, keep colors aligned.
7. **Opaque background.** The uncommitted fix forces alpha 255 on the host box. Confirm
   this is the wanted surface; the mod's own background mixin is not applied.

## Start here

1. Read `docs/architecture/fabric-runtime.md` (Event coverage, Legendary Tooltips, GUI
   replay, Reflection and config files), `AGENTS.md`, and this file.
2. Verify the two uncommitted fixes live with `~/horizon-fabric-test/run.sh`. Hover an
   enchanted netherite/diamond pickaxe in the inventory. Expected: opaque dark box, gold
   frame, colored centered title clearing the model panel, separator, model panel, attribute
   lines. A common item gets the white/rarity border and no gold frame.
3. If the model panel still overlaps the title, implement inline layout (item 1 above).
4. Commit the pending fixes first if they are good:
   - Root: `crates/hfc-runtime/src/bridge/events.rs` + the test assertion.
   - Client: `workspace/horizon/crates/horizon-client/src/game/stations.rs`.
   No `Co-authored-by:` trailer of any kind (the user removed a tool from the contributor
   list). Commit runtime at the root and client inside `workspace/horizon`. Push **only**
   root `main`; never push `workspace/horizon` without explicit permission.
5. Then take the remaining items in this order: inline layout, model rotation, config
   read-back, shine orientation. Update the tests for each real behavior and update
   `docs/architecture/fabric-runtime.md` plus the README's Legendary Tooltips row only for
   what you verify.

## Commands and machine rules

- Never run the whole runtime suite (it freezes the PC). One file at a time:
  ```sh
  nice -n 19 cargo test -j2 -p hfc-runtime --test <file> -- --test-threads=1
  ```
  Wrap risky runs in `timeout`.
- Mandatory files before finishing: `legendary_tooltips`, `config_files`, `veinminer`,
  `bridge_test`, `modmenu_screen`.
  - `modmenu_screen` has two **known pre-existing failures**:
    `a_fabric_key_mapping_opens_its_screen_and_the_host_replays_it` and
    `the_mod_list_scrolls_with_the_wheel_and_the_scrollbar`. Any other failure is yours.
- Format: `cargo fmt --check -p hfc-runtime`. For the client use
  `rustfmt --edition 2024 --check` on edited files; `cargo fmt -p horizon-client` reports a
  pre-existing diff in `game.rs`.
- Build the client:
  ```sh
  cd workspace/horizon
  nice -n 19 cargo build -j2 --profile fast -p horizon-client --no-default-features
  ```
- Live test: `~/horizon-fabric-test/run.sh` (own game/home dirs, `client.log`). Hover items
  in the inventory. Do not write to the real world or `~/.local/share/horizon*`.
- Never commit jars, assets, decompiled sources, saves, accounts or tokens.
- The interpreter is not a sandbox.

## Key files

| Path | Role |
|---|---|
| `crates/hfc-runtime/src/bridge/events.rs` | Event dispatch, GATHER/component list, HEAD injectors, tooltip render, render ticks |
| `crates/hfc-runtime/src/bridge/mod.rs` | `TooltipRender`, texture sizes, render-tick clock |
| `crates/hfc-runtime/src/builtins.rs` | GuiGraphics recording, texture sizes, FormattedCharSequence/strings/collections, item stacks, DeltaTracker |
| `crates/hfc-runtime/src/interp.rs` | `FormattedCharSequence.accept`, `getRarity`, inherited statics, mixin interface/accessor dispatch |
| `crates/hfc-runtime/src/minecraft.rs` | `GuiDraw` and Minecraft object models |
| `crates/hfc-runtime/tests/legendary_tooltips.rs` | 3 tooltip tests |
| `workspace/horizon/crates/horizon-client/src/game/fabric_bridge/mod.rs` | Client calls into the bridge (`fabric_tooltip_render`, rarity, max damage) |
| `workspace/horizon/crates/horizon-client/src/game/fabric_bridge/gui.rs` | `GuiDraw` replay incl. `Item` |
| `workspace/horizon/crates/horizon-client/src/game/stations.rs` | Tooltip geometry, background box, draw order |
| `docs/architecture/fabric-runtime.md` | Current architecture/status note to update |
