# Master prompt: make Legendary Tooltips render in Horizon

You are picking up work on **Horizon Fabric Compatibility**, a project that runs Fabric
Minecraft mods on Horizon, a Rust rewrite of the Minecraft: Java Edition 26.3 client.
You have no prior context. Read this whole prompt before doing anything.

## Your goal

Make **Legendary Tooltips** (with its libraries **Iceberg** and **Prism**) show its real
tooltip frames in the live Horizon client, through general code paths. Hovering an item in
an inventory should show the mod's own tooltip look: its border texture, its colors by
item rarity, the centered title, the separator line, and where feasible the 3D item model
and shine effect. Its config `config/legendarytooltips.toml` is already written correctly;
the rendering is what is missing.

Done means: in the live client, hovering an epic or rare item shows Legendary Tooltips'
frame instead of Horizon's plain tooltip, and a test that runs the mod's real bytecode
checks what it draws. Partial progress is fine if each step is real and documented.

## Where things are

- Root repository: `/home/daniel/Desktop/horizon-rust-fabric` (GitHub:
  `lyricalfrog3/horizon-fabric-compatibility`, branch `main`).
- The runtime: `crates/hfc-runtime`, a Java bytecode interpreter with Rust builtins.
  - `src/interp.rs`: the interpreter. Dispatch order is `execute_method` →
    `execute_method_dispatch` → `execute_method_inner`, then reflection (`src/reflect.rs`),
    special cases, builtins (`src/builtins.rs`, `src/text_io.rs`, `src/fs_builtins.rs`),
    then real bytecode. An unresolved call silently returns null; set
    `interp.trace_missing` / `interp.trace_calls` to see what was missed.
  - `src/bridge/events.rs`: tooltip dispatch. `item_tooltip` runs Fabric's
    `ItemTooltipCallback`; `item_tooltip_color` runs Iceberg's
    `RenderTooltipEvents.COLOREXT`.
  - `src/minecraft.rs`: `GuiDraw`, the display list that `GuiGraphicsExtractor` calls
    record into. The host replays it; the runtime never touches the GPU.
  - `src/bridge/menu.rs`, `src/bridge/screen.rs`: Fabric screens and menu widgets.
- The client: `workspace/horizon` is a **separate Git repository** (ignored by the root).
  Read `workspace/horizon/AGENTS.md` and `workspace/horizon/CLAUDE.md` before editing it.
  - `crates/horizon-client/src/game/fabric_bridge/` hosts the runtime (`mod.rs` has
    `fabric_tooltip` and `fabric_tooltip_color`; `gui.rs` replays `GuiDraw` lists).
  - `crates/horizon-client/src/game/native_hooks.rs` calls those hooks when it draws an
    item tooltip.
- Docs you must read first: `AGENTS.md` (root), `CONTRIBUTING.md`,
  `docs/architecture/fabric-runtime.md` (especially "Legendary Tooltips", "GUI replay" and
  "Reflection and config files"), and `docs/testing.md`.

## What already works and what does not

- Works: Legendary Tooltips, Iceberg and Prism initialize; `COLOREXT` is dispatched;
  `legendarytooltips.toml` is written with comments, typed values, allowed values and
  ranges (test: `crates/hfc-runtime/tests/config_files.rs`,
  `iceberg_mod_writes_its_toml_config_with_comments`).
- Does not work:
  - Iceberg's `RenderTooltipEvents.GATHER` and `POSTEXT` (and any pre-render event Legendary
    Tooltips uses) are **not dispatched**. The frame is drawn from those.
  - The `COLOREXT` callback reads `Minecraft.getInstance().level` and registry access; the
    runtime has no live client level, so the callback returns the incoming colors.
  - Horizon's tooltip renderer draws only a solid background and top/bottom border colors.
    It cannot draw a mod's textured, nine-slice frame, and mod tooltip textures are not
    loaded.

## A sensible plan (adjust as you learn)

1. Read Legendary Tooltips', Iceberg's and Prism's bytecode (`javap -c -p` on classes
   unzipped into a scratch folder) to find every Iceberg event and every
   `GuiGraphicsExtractor` call on the tooltip path. Write down the real call graph before
   coding.
2. Look at how Minecraft 26.3 itself renders tooltips (the client jar is at
   `~/.local/share/horizon-launcher/minecraft/versions/26.3/26.3.jar`). Find where vanilla
   fires the hooks Iceberg's mixins attach to, so the host can fire the same events at the
   same points.
3. Dispatch `GATHER` and `POSTEXT` (and whatever else step 1 finds) from the bridge, the way
   `item_tooltip_color` dispatches `COLOREXT`. Generalize the dispatch rather than adding a
   third copy.
4. Let the tooltip path render into a `GuiDraw` list, and have the client replay that list
   where it draws the tooltip. That needs blits from mod textures, nine-slice sprites and
   gradients in the replay path; add those to the shared `GuiDraw` replay, not a
   tooltip-only renderer.
5. Give the callbacks what they read: a client level and registry access backed by real
   host data, or the narrowest real substitute. Never a fabricated object that only
   returns defaults.
6. Add a runtime test that loads fabric-api, Iceberg, Prism and LegendaryTooltips from
   `test-mods/`, renders a tooltip for an epic item and asserts on the recorded draws
   (texture blits, border colors). Then build the client and check it live.

## Rules you must follow (these are binding)

- **Never fake compatibility.** A listener that runs but returns defaults is partial. Only
  call something working once it renders, and say exactly what is drawn.
- **No mod-specific code.** Never branch on a mod id, package, class name, config field,
  jar name or version. Fix the shared path (an event, a builtin, the `GuiDraw` replay, a host
  boundary). Library adapters for APIs many mods share, like Iceberg's events, are allowed.
- **Prefer real bytecode.** Add a builtin only at a host boundary or for a class the runtime
  deliberately replaces, and give it real semantics. Never stub a method to return a fixed
  value so output looks right.
- **Don't weaken checks** to make a test pass, and don't fabricate Minecraft classes in
  tests that claim real-classpath coverage.
- **Never commit** game or third-party binaries, jars, assets, decompiled sources, saves or
  secrets. Never read or print launcher accounts or session tokens.
- **Keep runs out of player data.** Use `~/horizon-fabric-test/` (run the client with
  `~/horizon-fabric-test/run.sh`, which uses its own game and home folders) or a temp dir.
  Never write to `~/.local/share/horizon*` or `~/.local/bin`.
- Commit runtime changes in the root repository and client changes inside
  `workspace/horizon`. End each commit with your `Co-Authored-By:` line. Don't push the
  `workspace/horizon` checkout anywhere without the user's say-so.

## Working with this machine and this user

- **The full test suite can freeze the PC.** It loads the whole Minecraft classpath. Never
  run `cargo test -p hfc-runtime` on everything, and never run several heavy jobs at once.
  Run one test file, at low priority:
  `nice -n 19 cargo test -j2 -p hfc-runtime --test <file> -- --test-threads=1`.
  Wrap anything that might hang in `timeout`.
- Build the client with
  `cd workspace/horizon && nice -n 19 cargo build -j2 --profile fast -p horizon-client --no-default-features`.
- The user prefers to test the live game themselves. When a change is ready, build the
  client and ask them to check one specific thing, instead of long automated runs.
- Interpreter hangs are usually infinite loops caused by a call that returns null where
  Java would throw. To find one, temporarily print calls in `execute_method_dispatch`,
  count the most frequent, and print the call stack with `trace_calls` on. Remove every
  debug line before committing (`grep -n DBG crates/hfc-runtime/src/*.rs`).
- Update `docs/architecture/fabric-runtime.md` and the README's "What works" table when, and
  only when, something is verified.
