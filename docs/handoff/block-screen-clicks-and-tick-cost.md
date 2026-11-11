# Handoff: make a mod screen's slots clickable, and cut the cost of a block entity's tick

You are continuing work at root commit `9c83fc0` ("Model the client's game mode and the
menu a screen shows") in **Horizon Fabric Compatibility** at
`/home/daniel/Desktop/horizon-rust-fabric`.

Two defects are open. Both were reproduced against the real Farmers Delight cooking pot
(`test-mods/corpus/FarmersDelight-26.3-3.6.30+refabricated.jar`) and the machine's
Minecraft 26.3. Neither is fixed. Nothing half-finished is committed: the working tree is
clean at `9c83fc0`.

Before you touch code, read the root `AGENTS.md`, [`docs/testing.md`](../testing.md), and
the block-entity and menu sections of
[`docs/architecture/fabric-runtime.md`](../architecture/fabric-runtime.md) (stage C4). If you
change client code, also read `workspace/horizon/AGENTS.md`.

What already works and must keep working (each has a test):

- the pot opens its own `CookingPotScreen` through the client's `MenuScreens` entry
  (`a_registered_menu_type_opens_the_mods_own_screen`);
- that screen draws its own `cooking_pot.png` at full resolution, and its draws follow the
  pointer (`a_screens_draws_follow_the_pointer`);
- the player's own inventory reaches the screen's player slots
  (`the_player_inventory_reaches_the_pots_menu`);
- the block entity is the mod's own object, ticks through its own `getTicker`, and saves and
  loads through `saveAdditional`/`loadAdditional` (five further tests).

---

## 0. Rules that apply to both fixes

- **Never fake compatibility.** Do not report a handled click or a successful tick whose
  effect did not happen. If a stack cannot move, that must be visible, not silently dropped.
- **No mod-specific code.** No branching on `farmersdelight`, a block id, a class name or a
  jar name. Note the existing general hook this work builds on: the runtime follows a menu's
  own screen by looking its `getType()` up in `MenuScreens.SCREENS`, which is not
  pot-specific and must stay that way.
- **Do not weaken tests.** The fixtures are real jars in `test-mods/` plus the machine's
  Minecraft install; without them the tests return early. Do not substitute fabricated
  Minecraft classes for a real jar to make something pass.
- **Do not add a trace-and-guess loop.** See section 3; the previous attempt wasted most of
  its time patching `eprintln!` into `interp.rs` and rebuilding. Write the failing test first.
- Commands (from the repo root):
  ```sh
  cargo fmt --check -p hfc-runtime
  nice -n 19 cargo test -j2 --release -p hfc-runtime --test block_entities -- --test-threads=1
  # only when workspace/horizon/client changes:
  (cd workspace/horizon && nice -n 19 cargo build -j2 --profile fast -p horizon-client --no-default-features)
  (cd workspace/horizon && nice -n 19 cargo test -j2 --profile fast -p horizon-client --no-default-features --lib fabric_bridge:: -- --test-threads=1)
  ```
  Builds here take 30 s–2 min; run them in the background and keep working rather than
  polling.

---

## 1. A mod screen's slots cannot be clicked (the main defect)

**Symptom, as the player sees it.** On the pot's own screen nothing responds except the
recipe book button, and that opens nothing. No stack can be picked up, moved or dropped. The
menu looks alive (it draws, it hovers) but is inert.

**What is already in place.** `AbstractContainerScreen.slotClicked` hands every slot click to
`minecraft.gameMode.handleContainerInput`, which is what runs the menu's own `clicked` and so
moves the stack. The runtime's `Minecraft` had no `gameMode` field at all, so that call went
to null. `9c83fc0` added the object (`crates/hfc-runtime/src/builtins.rs`, in
`Minecraft.getInstance`) and made `Minecraft.setScreen` point `gameMode.menu` at the new
screen's own `menu`, so a click can find the menu it belongs to. That is necessary but **not
sufficient** — a click still moves nothing.

**The 26.3 control flow, established from `javap` of the real 26.3 jar.** Read this before
you start; chasing the wrong branch is what went wrong last time.

- **Left click is handled on release, not press.** In
  `AbstractContainerScreen.mouseClicked`, the branch at bytecode offset 88–105 sends
  anything that is not button 1 (right) or 3 (middle) to offset 377, which is
  `checkHotbarMouseClicked` and then `return`. Only right-click reaches the slot logic in
  `mouseClicked`.
- The left-click pickup is in `AbstractContainerScreen.mouseReleased`: it resolves
  `getHoveredSlot`, then reaches `slotClicked` at offsets ~358 and ~438 (with
  `ContainerInput.PICKUP`, or `QUICK_MOVE` when shift is held).
- `slotClicked` calls `gameMode.handleContainerInput(containerId, slotIndex, button, input, player)`.
- `MultiPlayerGameMode.handleContainerInput` is client-side: it builds a hashed stack diff
  and sends a `ServerboundContainerClickPacket`. For a single-player host the useful part is
  `menu.clicked`, which is what applies the slot rules.
- `AbstractContainerMenu.clicked` wraps `doClick` in a crash-report try/catch.
- `doClick` reaches the pickup path when `ContainerInput` is `PICKUP`/`QUICK_MOVE` **and**
  `isContainerClickButton(button)` (button 0 or 1) — bytecode offsets 561–579.
- At offset 795 `doClick` calls `tryItemClickBehaviourOverride(...)`; if that returns **true**
  it jumps to 1066, which is a no-op. That is a real way for a click to be swallowed, though
  it was checked and returns false here (see below).

**What has been ruled out, so you do not redo it.**

- The click reaches the screen: `mouse_button` returns `true`, and a direct call to
  `AbstractContainerScreen.mouseClicked` returns `true`.
- `getHoveredSlot` resolves a real slot at the click point. At GUI size 427×240 the pot's
  panel is `leftPos=125, topPos=37`; menu slot 10 sits at `Slot.x=8, Slot.y=84` and hovers
  correctly.
- `menu.getCarried()` is an empty stack (`ItemStack.isEmpty()` true), so `doClick`'s
  "carried is empty" path is the one being taken.
- `Slot.getItem()` returns the carried item (e.g. `minecraft:beef` ×5) when the host's
  inventory reports it, so the slot-to-container path works.
- `ItemStack.overrideOtherStackedOnMe` returns `false`, so `tryItemClickBehaviourOverride`
  is not the blocker.

**Where it was left.** `doClick` does not reach `Slot.mayPickup`, `Slot.tryRemove` or
`setCarried`; the carried stack stays empty. The exact instruction at which it diverges was
not found. That is the open question.

**Suggested approach — test first, then instrument once.** Do not repeat the trace loop.

1. Add a test that drives the **real** sequence, which the previous attempt never did:
   ```rust
   bridge.mouse_button(x, y, 0, true);   // press
   bridge.mouse_button(x, y, 0, false);  // release — this is where 26.3 picks up
   ```
   then assert the menu's `carried` holds the stack and the slot is empty. Use the slot's
   own `x`/`y` plus `leftPos`/`topPos` for the point, as
   `the_player_inventory_reaches_the_pots_menu` already does. Give the host stub a full
   inventory (the constructor maps menu slot → container slot; fill every entry so the
   mapping cannot mislead you).
2. Only if that fails, instrument `doClick` **once** and print, in a single run, the value of
   every branch condition in one go — not one condition per rebuild.
3. Decide the fix. `handleContainerInput` is intercepted nowhere today; the natural place to
   run `menu.clicked` for a single-player host is either
   `Interpreter::execute_method_inner` (it can dispatch other calls) or a builtin, but a
   builtin only receives `heap`, so it cannot call `clicked` itself. Whatever you choose must
   also keep the menu's slots backed by the mod's own inventory, so the block's
   `saveAdditional` still persists what the player put in — do not reintroduce a host-side
   copy.

**Beware this trap.** The screen's `children` list contains a `RecipeBookComponent` sized
`427×240` at `(0,0)` — the whole screen. It is a `GuiEventListener` with its own
`isMouseOver` that always returns `false`, so it does not currently swallow clicks, but any
change to hit-testing must not let it start. The user's report that "only the recipe book
button reacts" is consistent with the `ImageButton` at `(130,71)` `20×18` winning the hit
test and the rest of the screen being inert for the `doClick` reason above, not with the
recipe book component eating clicks.

---

## 2. A placed pot makes the game lag

**Measured.** One pot's block entity costs **~7.2 ms per tick** — steady, not growing
(`tick 0` 7.17 ms, `tick 39` 7.13 ms over 40 ticks). At 20 ticks/s that is ~14% of a
second, every second, for a single block, and it is on the server thread.

**How it was measured, and the parts already excluded.** A test loop calling
`FabricBridge::tick_block_entity(&world, [4,1,4], "farmersdelight:cooking_pot")`
(`crates/hfc-runtime/src/bridge/blocks.rs:1416`) 40 times gave the figure above. Timing the
stages inside it gave, per tick, roughly:

| stage | cost |
|---|---|
| `world_around` (5×5×5 = 125 `host.block`/`block_properties` queries) | ~7 µs |
| `snapshot_level` | ~12 µs |
| `getTicker` (`execute_method` on `EntityBlock.getTicker`) | ~31 µs |
| the interpreter's `tick` call | ~37–42 µs |

**That accounts for ~90 µs of 7,200 µs.** So the profile is incomplete and the real cost is
somewhere not yet instrumented. `heap.begin_scope()` / `end_scope()` are correctly paired in
`tick_block_entity` (`blocks.rs:1439` and `:1490`), so it is not a heap leak; that pairing
was checked and is not the answer. Find the remaining ~7.1 ms before optimising anything —
the table above is where to start looking, not a conclusion.

**Facts you will need.** The pot's own tick is
`CookingPotBlockEntity.cookingTick(ServerLevel, BlockPos, BlockState, CookingPotBlockEntity)`,
registered as a static ticker, 94 bytecode instructions. It calls `isHeated(level, pos)` and
`hasInput()`, and only when both are true builds a `RecipeWrapper` and calls
`getMatchingRecipe(wrapper, level)` — i.e. it queries the recipe manager. **Note:** in the
test stub the pot is not heated, so `cookingTick` short-circuits at `hasInput` and does
almost nothing; a nearly-free tick body yet 7.2 ms of wall time is exactly why the real cost
must be located before changing anything.

**Do this first.** Reproduce the figure with a test that times the whole
`tick_block_entity` call and each stage, on both an unheated and a heated pot (the test
stub's `World { heated: true, .. }` plus a `farmersdelight:stove` below the pot puts it in
the heated path). Do not optimise on the strength of the table above alone.

**Candidate fixes, in order of likely value.** Judge them against a measurement:

1. Cache what does not change per tick: `world_around`'s snapshot, the block object lookup,
   and the ticker object. `getTicker` returning the same ticker every tick is vanilla
   behaviour; calling it 20 times a second is not required by anything.
2. Give the recipe lookup a general builtin so `getMatchingRecipe` stops being interpreted
   (there is no `RecipeManager` modelling in the runtime today — `grep RecipeManager
   crates/hfc-runtime/src` finds nothing).
3. Throttle a block entity's tick when its code is known to be idle (`cookingTick`
   short-circuits when unheated and inputless). This must not change observable behaviour:
   a pot must still cook at the right rate.

Whatever you change, keep the documented behaviour that a tick's effects go through the host
exactly as a block tick's do, and that `TICK_BUDGET` (2M instructions) still bounds it.

---

## 3. Process notes from the previous attempt

Worth reading so the time is not spent twice.

- **The click investigation failed mostly on method.** Dozens of rounds went into patching
  `eprintln!` into `crates/hfc-runtime/src/interp.rs`, rebuilding, and reading one line. The
  decisive fact (left click is handled on *release*) was available from `javap` at the start.
  Read the bytecode, form a hypothesis about which branch runs, then write one test that
  proves it.
- **One measurement was wrong in a way that misled badly.** An `isEmpty` check was run
  against a `Slot` object instead of an `ItemStack`, which returned `true` and sent the
  investigation down a false path. When probing, print the object's class alongside the
  value.
- **Instrumentation must not recurse.** Traces placed inside `execute_method_inner` that call
  `execute_method_inner` again produce "call stack overflow" errors that look like real
  failures. Place traces at `execute_method_dispatch` (the choke point) or print without
  re-dispatching.
- **Verify a probe before believing it.** Several probes printed nothing at all because the
  hook was in a path that was not taken; absence of output is not evidence of absence of a
  call.
- **The user reads the transcript.** Do not claim a fix is done until a test fails without it
  and passes with it. Reverting the change to confirm the test catches the bug is the
  cheapest guard against a false positive, and was used successfully for the texture fix
  (`9ecae01b8`) — do the same here.

---

## 4. Committing

Work that is finished and verified goes in the root repository; client changes go in
`workspace/horizon` (branch `hfc`, remote `fork`, never upstream `origin`), then the
submodule pointer is bumped at the root. Both end with a `Co-Authored-By:` footer. Do not
commit unless the relevant tests pass, and do not commit a test you could not make pass.
