# AnimatedDoors 2.0.2 — why it does not work, and what was fixed

Commit `7feca4e`. Analysis artefacts under `.modanalysis/`.

## Verdict

The mod cannot work in this runtime, for reasons that are structural, not incidental. But
chasing it found seven real general bugs, five of which are now fixed and tested, and one of
which (silent entrypoint abandonment) was hiding all the others.

## The three layers

### Layer 1 — the entrypoint died silently (FIXED)

`AnimatedDoorsClient.init` runs four steps. It died on step 2, in
`AnimatedDoorsConfig.save()` → `Properties.store` → `Date.toString` → `TimeZone.getDefault` →
the private `setDefaultZone` → the JVM native `getSystemTimeZoneID` → binary tzdata this host
does not ship.

The failure was invisible. `Interpreter::execute_method_dispatch` deliberately does not fail a
call when the runtime lacks a symbol (the gap is the runtime's, not the mod's) — a defensible
rule — but the consequence was that `EntrypointRun.ok` stayed `true` and `events` stayed empty.
"Registered nothing" and "died half-way through" looked identical. The census for this mod said
`1/1 entrypoints ok`.

Evidence it was real, independent of the runtime: `game/config/animated_doors.properties`
contained only `#AnimatedDoors client config` — the mod got as far as writing a header and no
further.

**Fixed:** the interpreter records what it abandoned, the bridge carries it on the entrypoint
run, and the census names the symbol. `TimeZone` is now a host boundary (UTC, offsets 0,
`setDefaultZone`/`getSystemTimeZoneID` answered where the JDK calls its own private
initializer). The mod's entrypoint now completes and registers its one and only gameplay hook:

```
ok: True   abandoned: None
events: [{UseBlockCallback.EVENT, listeners: 1, dispatched: True}]
```

### Layer 2 — the census claimed compatibility it did not have (FIXED)

All seven of the mod's mixin targets were reported `applied`. Nothing was ever rewritten:
`MixinInjector::supported()` is a parse check on the `@At` value, and the census turned that
into "applied". Only two call sites in the entire crate ever ran an injector — host-menu widget
layout and a non-cancellable tooltip HEAD.

**Fixed:** `applied` now means verified-against-classpath and wired into the interpreter;
`partial`/`ignored` carry reasons. The table also has a legend saying that wired is not the same
as the mod working — a callback on a method the host never calls is wired and still never runs.
The same run that showed 7/7 `applied` now shows Sodium as `native`, `LevelRenderer` as
`ignored`, and the honest rest.

### Layer 3 — the render path has no seam (NOT FIXABLE, structural)

This is the part that decides the outcome. The Horizon client is a Rust client with its own
renderer: `horizon-client-render` has **zero** dependency on `hfc_runtime`. Chunk meshing happens
on worker threads in `mesher.rs`, a port of `ModelBlockRenderer`. Nothing Java can influence
what is drawn.

The mod's model is "vanilla tesselates a block into quads; I hide that and submit my own." That
requires being *inside* Minecraft's `ModelBlockRenderer`/`RenderSectionRegion`/`LevelRenderer`.
Horizon never calls any of them — and the runtime has no stand-in for `LevelRenderState`,
`SubmitNodeCollector`, `PoseStack`, `BlockQuadOutput`, `BlockAndTintGetter`, `RenderSectionRegion`
or `BlockStateModel` at all. A render callback would "run" and draw nothing, with no error.

Even a perfect Mixin implementation injects into methods that are never called. Enabling this
would mean a new per-block geometry seam in Horizon's mesher plus a per-frame extra-geometry
hook — the render audit sized it at roughly 4–6 weeks, and it would not be a wiring change.

## What was fixed (all general, no mod-specific branches)

| Fix | Tests |
|---|---|
| `@Inject` callbacks that cancel / set a return value now really run | `mixin_inject_behaviour` (revert-proof), `mixin_inject` |
| `@Invoker`/`@Accessor` reach private, inherited, superclass and static targets | `mixin_accessors` — 5 fail at baseline, 11 pass |
| Entrypoint abandonment is recorded and reported, not swallowed | — |
| `TimeZone` host boundary (utc, offsets, the private initializer) | `a_mod_formatting_a_date_reaches_no_tzdata_native` |
| `net/minecraft/stats/Stats` synthesized (was in one list, not the other) | `a_synthesized_static_is_the_type_its_own_code_builds` |
| `census_interprets` no longer says yes for a class it cannot load | `a_class_the_loader_cannot_find_is_not_interpreted` |
| Docs corrected where they overstated coverage | — |

Each test was verified by reverting the fix and watching it fail.

## Two bugs my own fixes introduced, caught by the suite

Worth recording because they show the tests working:

- A first `TimeZone` boundary answered `getID` with a String and intercepted `SimpleTimeZone`.
  Four `block_entities` screen tests failed — a zone-shaped String where a zone object was
  expected.
- A blanket `Reflection.getClassAccessFlags` answering "public" broke the Legendary Tooltips
  frame test, because reflection acts on that answer.

Neither survived. The TimeZone boundary was narrowed to what the JDK actually needs.

## Still open

- **`modmenu_screen`: 2 tests fail deterministically at this commit** (pre-existing, unchanged).
  The Website button renders `"_"` even though `modmenu.website` → `"Website"` is loaded and
  available; the wheel does not move the list. Not root-caused.
- `@Redirect`, `@ModifyArg`, `@ModifyVariable`, `@WrapOperation`, `@At(INVOKE)`, MixinExtras
  local capture, `@Overwrite`: still not applied, now reported as such.
- `java.util.Properties.store`/`load` are not modelled, so a config-using mod's file is written
  through real JDK bytecode. With the time-zone boundary the write now completes, but whether
  the entries land correctly is untested.
- The render seam, as described above.

## Process note

An earlier round of eight agents drove this machine into OOM (three concurrent `--release`
builds, one of them vendoring a second copy of the workspace). The recovery is documented in
`.modanalysis/AGENT-MEMORY-PROTOCOL.md`: `-j2`, never vendor, one test binary at a time. The
later rounds ran under it without incident.
