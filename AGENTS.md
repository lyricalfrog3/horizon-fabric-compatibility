# Horizon Fabric Compatibility: agent notes

This repository has two separate tracks. Do not mix their claims.

| Track | Path | What it does |
|---|---|---|
| HFC installer | `crates/hfc-*` except `hfc-runtime` | Host-side discovery, analysis, content conversion, cache, install and reports. Does not execute mod Java code. |
| Experimental Fabric runtime | `crates/hfc-runtime` plus `workspace/horizon` client hooks | Interprets selected Fabric entrypoints in the client with a custom bytecode interpreter and Java/Minecraft builtins. Not general Fabric compatibility. |
| Nested Horizon checkout | `workspace/horizon` | The client and launcher. Has its own Git history and its own `AGENTS.md`; read that before editing client code. |
| Nested references | `workspace/horizon-mods`, `research/horizon-mixins` | Separate Git checkouts; the root repository ignores them. |

Read [docs/architecture/fabric-runtime.md](docs/architecture/fabric-runtime.md) before changing
the runtime. Read [docs/testing.md](docs/testing.md) before adding tests.

## Non-negotiable rules

- **Never fake compatibility.** A mod whose entrypoint returns without error is not "working".
  A listener that runs but returns default values is partial at best. Document what is actually
  rendered, dispatched or stored.
- **No mod-specific compatibility code.** When a mod exposes a missing general path, fix the
  shared path: an opcode, a Fabric API event, a Minecraft class/method builtin, a config or
  resource format, or a host boundary. Do not branch on a mod id, a mod's package or class
  name, a specific config field, a jar filename or a version. The same code must work for any
  mod that uses that API. Tests may load one mod to reproduce a bug, but the fix belongs in
  the general path.
  Library adapters for APIs shared by many mods (for example JamLib events, Iceberg tooltip
  events) are allowed; adapters for one mod's own classes are not. Existing per-mod shims
  (`Config$HungerLevel`, `Config$ExperienceType`, `RightClickHarvestPlatform`,
  `CropBlockAccessor`, all in `crates/hfc-runtime/src/shims.rs`) are debt: generalize them
  when touching that path, and do not add another one.
- **Do not weaken checks to make a test pass.** No skipped assertions, swallowed interpreter
  errors, silent fallbacks or fabricated Minecraft classes in tests that claim real-classpath
  coverage.
- **Do not commit game or third-party binaries.** Minecraft jars, assets, prepared resources,
  downloaded Fabric jars and generated mod folders are ignored. Local runtime fixtures live in
  `test-mods/` and are never committed; see `test-mods/README.md`.
- **Keep scratch runs out of player data.** Use a temporary game directory and `horizon-ctl`
  for live tests. Do not write into the user's real world, `~/.local/share/horizon*` or
  `~/.local/bin`.
- **No secrets or session files.** Never read, print or commit launcher accounts, session
  tokens or `.env`/`.dev.vars` values.

## Build and test

The root workspace references the separate `workspace/horizon` checkout through path
dependencies; check that checkout out at the pinned commit (see
`docs/research/horizon-current-state.md`) before building or running the whole workspace.

Root workspace (HFC crates, including the runtime):

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo test -p hfc-runtime
cargo fmt --check -p hfc-runtime
cargo build --release -p hfc-cli
```

Horizon client (separate workspace):

```sh
cd workspace/horizon
cargo build --profile fast -p horizon-client --no-default-features
```

`--no-default-features` avoids the `audio` and `controllers` dependencies, which need ALSA
and libudev development files on Linux. The runtime tests use the machine's Minecraft
installation when present; if it is absent, those tests must skip or fail clearly, not pass on
fabricated classes.

## Runtime conventions

- Prefer executing the mod's real bytecode. Add a builtin only at a host boundary (I/O,
  registry data, rendering) or for a class the runtime deliberately replaces.
- `ENTRYPOINT_BUDGET` in `crates/hfc-runtime/src/bridge/mod.rs` is 10M. It exists to stop a
  mis-decoded branch, not to police legitimate initialization. If you change it, record the
  measured entrypoint costs in `docs/architecture/fabric-runtime.md`.
- Classes whose static fields are synthesized must be in both
  `Interpreter::has_synthetic_statics` and `create_static_field_val`. Otherwise the first
  `Getstatic` rebuilds Minecraft's real registry and stalls the render thread (measured at
  ~34 s before the fix).
- Tooltip and screen coverage is partial. `COLOREXT` is dispatched; `GATHER`, `POSTEXT`,
  `ScreenEvents` and `ClientTickEvents` are not. Do not describe Mod Menu or Legendary
  Tooltips as working until their UI/frame paths are actually rendered.
- The interpreter is not a sandbox. Treat every jar as untrusted code running in-process.

## Repository layout

| Path | What |
|---|---|
| `crates/hfc-core` | Limits, errors, hashing, atomic writes |
| `crates/hfc-fabric` | Jar discovery, `fabric.mod.json`, version ranges, dependency graph |
| `crates/hfc-cache` | Content-addressed conversion cache |
| `crates/hfc-runtime` | Experimental Fabric bytecode interpreter and bridge |
| `crates/hfc-bootstrap` | Conversion run loop and install/state/reports |
| `crates/hfc-cli` | `hfc` command line |
| `docs/` | Architecture, decisions, research, roadmap, testing, compatibility matrix |
| `workspace/horizon` | Horizon client/launcher checkout (separate Git repository) |
| `test-mods/` | Local pinned Fabric jars for runtime tests (jars ignored by Git) |

## Git

The root repository tracks HFC source and docs, not the nested checkouts. Commit client-adapter
changes inside `workspace/horizon`; commit runtime/HFC changes at the root. Build output,
generated mods, saves, caches and local jars are ignored.

## Attribution

Commits made by an agent end with the agent's `Co-Authored-By:` footer. The nested Horizon
checkout has its own attribution rules in `workspace/horizon/AGENTS.md`; follow them there.
