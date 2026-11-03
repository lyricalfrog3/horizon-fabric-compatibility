# Roadmap

Ordered by dependency, not ambition. Each phase has an exit gate; a phase that has not met its
gate does not get advertised as compatible. Dates are deliberately absent.

## v1 — content-first vertical slice (current)

**Done**

- Host-side discovery, metadata, dependency graph, defensive ZIP scanning.
- Conversion through Horizon's existing `horizon-port-pack` check/carry pipeline.
- Content-addressed cache, atomic install, markers, cleanup, `horizon-mods.txt`.
- JSON + Markdown reports; launcher hook; CLI (`install`, `bootstrap`, `scan`, `status`).
- 56 HFC tests (including the experimental runtime) + 3 launcher hook tests; generated
  folders are read by Horizon's own `horizon-mods` loader; full workspace tests and
  formatting pass.
- Research notes, ADRs, architecture, compatibility matrix, this roadmap.

**Known limits**

- HFC's conversion pipeline has no Java/Mixin/entrypoint behaviour, no Fabric API and no
  generated native mods.
- An experimental in-client interpreter (`crates/hfc-runtime`) runs selected entrypoints;
  it is outside this v1 gate and does not provide screens, Mixins or general gameplay
  compatibility. See [fabric-runtime.md](architecture/fabric-runtime.md).
- No real-game launch test for carried content, no real-mod fixture, no parity measurements.
- No lock file; launcher hook is synchronous.

**Exit gate:** a player can install HFC, drop a content-heavy jar, press Play, and see the
carried blocks/items in a world, with a report that matches what appeared. *Not yet verified.*

## v1.1 — make the slice trustworthy

1. **Real launch verification.** Run Horizon with a generated mod; assert registry/asset
   loading. Record the exact Horizon commit and platform.
2. **Real-mod corpus.** A small, licensed set of pinned jars (or a user-provided local corpus)
   exercised manually, with reports checked against observed content. Never commit
   third-party binaries without permission.
3. **Report UX.** Surface `report.md` from the launcher (open report / per-mod details), not
   just one log line.
4. **Performance.** Measure cold/warm on 1/10/50 jars; add a progress line for cold runs; cap
   or stream the synchronous hook so a large first launch does not look frozen.
5. **Lock file.** One bootstrap per mods folder; second process waits or exits.
6. **Cache pruning.** `hfc cache prune` / `--prune`, keeping entries referenced by the current
   state.
7. **Host validation.** Verify generated folders with Horizon's own loader before enabling
   them; install as disabled and enable only after validation if the loader exposes a check.
8. **Platform CI.** Linux + Windows + macOS for scanner/cache/install; launcher hook on each.

**Exit gate:** real-game demonstration, measured performance, no untested platform claims.

## v2 — reusable Fabric compatibility layer

Do not implement per-mod patches. Build adapters against Horizon's public mod API, each with
its own compatibility version and tests:

1. Lifecycle events (`onInitialize`, client/server tick, world load).
2. Item and block registration (including properties/components that map to Horizon).
3. Recipes, loot tables, tags (already content; now behaviourally complete).
4. Block states and block entities.
5. Inventories, containers, menus and screens.
6. Networking (`RemoteEvent`/`RemoteFunction` analogues).
7. Entity registration and behaviour.
8. World generation adapters.
9. Client events, rendering interfaces, keybinds.
10. Configuration APIs.

Each adapter is a **native Horizon mod** or engine capability. It must run in Horizon's own
security model. Since native mods are unsandboxed, adapters ship as reviewed, signed code —
never generated per jar at launch.

**Exit gate:** a mod composed only of adapted APIs runs without per-mod code changes, proven
by behavioural tests.

## v3 — automatic code compatibility

The hard question is how to run a jar's Java logic without shipping a JVM and without claiming
fake compatibility. Candidate paths, in order of preference:

1. **Static analysis → native port generation.** Read class structure and bytecode (no
   decompilation of third-party source, no execution), classify methods, and generate Rust
   source against v2 adapters for a supported subset. Compile with the launcher's native-mod
   pipeline. Human-reviewable source, deterministic output, cache keyed by jar + API +
   generator + compiler + platform.
2. **WASM/component lowering.** Compile a restricted JVM subset to WASM (e.g. via an existing
   AOT compiler) and run it in a sandbox with host imports for the v2 adapters. Requires a
   sandboxed runtime Horizon does not have today; would be a host capability, not a mod.
3. **Embedded JVM.** Only if (1) and (2) cannot cover a class of mods that matters. A JVM is a
   large attack surface and must be sandboxed (process isolation, no ambient filesystem or
   network), with explicit user consent. Never embed Iris/Iris-like mods merely to claim
   compatibility.

Decision gate before implementing (1): a representative mod corpus, a classification of what
fraction is mechanically portable, and a generated-code safety story (no arbitrary native code
from an untrusted jar running unsandboxed). Until that gate passes, code stays out of scope.

**Exit gate:** behavioural parity tests on the corpus, with unsupported constructs rejected,
not silently stubbed.

## v4 — optional execution environment

Only if justified by v3 data. Sandboxed, opt-in, clearly labelled as running Java, with its
own resource limits and no implicit access to the player's files, network or credentials.
Separate from content conversion and from native mods.

## Shader track (parallel, independent)

Follow [architecture/shaders.md](architecture/shaders.md): inventory/report → renderer feature
gaps → simple post-pass translation → materials/sky → options → behavioural image comparison.

**Exit gate for any claim:** the pack runs through Horizon's shader API and matches reference
images on fixed scenes; GLSL parsing or translation alone is not compatibility.

## Cross-cutting work

| Area | Work |
|---|---|
| Compatibility API | Publish the adapter surface and a stability policy; breaking changes bump `COMPAT_API` and invalidate cache |
| Cache | Add dependency fingerprint + generator/compiler versions for code ports; prune; per-entry integrity |
| Security | Threat model review for generated native code; signing/provenance for adapters; fuzzing the scanner |
| Diagnostics | Machine-readable issue codes already exist; add suggested fixes and per-feature confidence |
| Packaging | How HFC ships with Horizon (bundled binary, updater, version compatibility), without making the launcher depend on a network download at Play time |
| Licensing | Generated ports derive from the original jar's assets; respect the mod's licence and do not redistribute jars |

## Non-goals

- Running arbitrary Java with full OS privileges.
- Downloading mods or dependencies automatically.
- A fake Minecraft/Fabric API that compiles but does nothing.
- Claiming compatibility from metadata, compilation or a single screenshot.
- Modifying or redistributing original jars.
- Reimplementing Horizon's carry pipeline inside HFC.
