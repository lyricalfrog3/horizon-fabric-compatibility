# Follow-up prompt: finish Legendary Tooltips (3D model, title, separator, shine)

Nice work on the Legendary Tooltips frame: it renders live and the user confirmed it looks
right. Keep going and finish the rest of the mod's tooltip look in the live Horizon client,
through general code paths, with the same rules as before.

## What changed since your commit (read this first)

Another agent reviewed your work and made two follow-up changes, committed and pushed as
`f9c8103` in the root repository. Pull them into your understanding before you edit:

- **The Prism `IColor` adapter is gone.** It was replaced by general support for
  mixin-added interfaces: `mixin::interface_methods_from_class` reads any mixin class that
  implements an interface. The interpreter adds that interface to the target for
  `instanceof` (`mixin_interfaces`). A call to one of the interface's methods runs the
  mixin's own bytecode with the target as `this` (`redirect_mixin_interface_method` in
  `interp.rs`). A receiver whose own class implements the method keeps it. Don't add
  another hand-written mixin adapter; extend this general path if you need more.
- **`static_field_owner` is cached** per (class, field) in `Interpreter::static_field_owners`.

Both are documented in `docs/architecture/fabric-runtime.md` under "Reflection and config
files". Your tests (`tests/legendary_tooltips.rs`) still pass and the live tooltips were
re-checked.

## What to build now

From the design note's "Still partial" list, in the order that unblocks the most:

1. **The tooltip component list.** The bridge passes an empty `ClientTooltipComponent` list
   to `POSTEXT`/`COLOREXT`. Build the real list the way vanilla does (text lines become
   `ClientTextTooltip`, image tooltips become their components) and pass it through. The name
   separator depends on it.
2. **Dispatch Iceberg's `RenderTooltipEvents.GATHER`.** That's where the mod adds its
   item-model and padding components. Generalize the existing event dispatch in
   `bridge/events.rs` (it already serves `COLOREXT` and `POSTEXT`) instead of adding a third
   copy. The host must lay out and draw the components the event adds.
3. **The centred title and name separator.** These come from Legendary's
   `GuiGraphicsMixin`, a mixin callback rather than an event. Find the vanilla method it
   injects into and the injection point. Support that injector kind in `mixin.rs` generally:
   it already applies a subset of `@Inject` for host menus, and records everything else as
   unsupported. Don't special-case Legendary.
4. **The 3D item model.** The component from `GATHER` renders the item through Minecraft's
   item rendering on the GUI. Horizon already draws item models (its own `item_render` /
   `item_draw`), so add a `GuiDraw` variant for "draw this item stack at this rect with this
   rotation" that the host replays with its real item renderer. Keep it a host boundary: the
   runtime records what to draw, the client draws it. The rotation speed comes from
   `model_rotation_speed` in the config.
5. **The shine effect.** It is an animated draw over the frame. Find what it calls
   (probably blits or gradients driven by time) and make sure time reaches it
   (`Blaze3D.getTime`, `Util.getMillis`). Then the existing replay draws it.

Done means: in the live client an epic/rare item shows the centred title, the separator under
the name, the rotating 3D item model on equipment, and the shine. A runtime test asserts on the
recorded draws for each part you add. Partial progress is fine if each step is real and the
docs say exactly what is drawn.

## Rules (unchanged, binding)

- **Never fake compatibility.** Say exactly what is rendered. A listener that runs but
  returns defaults is partial.
- **No mod-specific code.** Never branch on a mod id, package, class name, config field, jar
  name or version. Fix the shared path: an event, an injector kind, a builtin, the `GuiDraw`
  replay, a host boundary. Library adapters for shared APIs like Iceberg's events are allowed;
  prefer running the library's real bytecode over reimplementing it.
- **Prefer real bytecode.** Builtins only at host boundaries, with real semantics, never a
  fixed value to make output look right.
- **Don't weaken checks**, and don't fabricate Minecraft classes in real-classpath tests.
- **Never commit** jars, game assets, decompiled sources, saves or secrets. Don't add a
  `Co-authored-by: CommandCodeBot` (or any tool's) trailer to commits. The user removed one
  such tool from the contributor list and doesn't want more.
- Runtime changes are committed in the root repository; client changes go inside
  `workspace/horizon`. Don't push `workspace/horizon` anywhere.

## Working on this machine

- **Never run the whole runtime suite**; it freezes the PC. One file at a time, low priority:
  `nice -n 19 cargo test -j2 -p hfc-runtime --test <file> -- --test-threads=1`, wrapped in
  `timeout` if it might hang.
- Before you finish, run at least `legendary_tooltips`, `config_files`, `veinminer`,
  `bridge_test` and `modmenu_screen` that way. In `modmenu_screen`, two tests already fail:
  `a_fabric_key_mapping_opens_its_screen_and_the_host_replays_it` and
  `the_mod_list_scrolls_with_the_wheel_and_the_scrollbar`. Any other failure is yours.
- Build the client:
  `cd workspace/horizon && nice -n 19 cargo build -j2 --profile fast -p horizon-client --no-default-features`.
  The user tests the live game with `~/horizon-fabric-test/run.sh`; tell them exactly what
  to hover and what they should see.
- Interpreter hangs are usually a call returning null where Java would throw. Find them by
  counting the most frequent calls in `execute_method_dispatch` and printing the stack with
  `trace_calls`. Remove every debug line before committing.
- Update `docs/architecture/fabric-runtime.md` and the README's "What works" row for
  Legendary Tooltips only for what you verified.
