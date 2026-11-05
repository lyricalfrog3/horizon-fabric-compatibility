<div align="center">

# Horizon Fabric Compatibility

**Run Fabric mods on [Horizon](https://github.com/horizon-rust/horizon), the Minecraft: Java Edition client rewritten in Rust.**

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/rust-stable-orange.svg)
![Minecraft](https://img.shields.io/badge/minecraft-26.3-62B47A.svg)
![Status](https://img.shields.io/badge/status-experimental-yellow.svg)

[What works](#what-works) · [How it works](#how-it-works) · [Getting started](#getting-started) · [Contributing](#contributing) · [Docs](docs/README.md)

</div>

---

Horizon has no JVM, so ordinary Fabric `.jar` files can't run on it as they are. This project
closes the gap in two ways:

- **The HFC installer** reads Fabric jars as data and converts what Horizon can already
  carry (blocks, items, models, textures, recipes, loot tables, tags, worldgen) into
  Horizon content mods. It never executes mod code.
- **The experimental runtime** (`hfc-runtime`) is a Java bytecode interpreter inside the
  client. It runs selected Fabric entrypoints and their real bytecode against Rust
  implementations of the Java, Minecraft and Fabric API classes they call.

> [!WARNING]
> This is experimental. The runtime is **not** general Fabric compatibility, does not
> support Mixins in general, and **is not a sandbox**: every jar runs as untrusted code
> inside the game process. Only install mods you trust.

## What works

Everything below is checked by a test that runs the mod's own bytecode, and in the live
client where noted. A mod not listed here should be treated as unsupported.

| Mod | What is verified | Live client |
|---|---|:---:|
| **Right Click Harvest** | Right-click harvesting and replanting; `rightclickharvest.json5` written with comments, matching the real game's file | ✅ |
| **Veinminer** | Breaking one ore breaks the connected vein (Kotlin coroutines, `PlayerBlockBreakEvents.BEFORE`) | ✅ |
| **Mod Menu** | Title and pause menu buttons, the mod list with icons, search and scrolling; link dialogs that open the browser or copy to the clipboard; `modmenu.json` byte-identical to the real game's | ✅ |
| **Legendary Tooltips** (Iceberg, Prism) | Its textured nine-slice tooltip frame, rarity-matched colors and shadow; the tooltip body from the mod's real component list (centred title, name separator, item-model request); `legendarytooltips.toml` written with comments, allowed values and ranges | ✅ frame, title, separator, model panel |
| **Emerald Tools** | Its five tools, registered in code, become Horizon items with vanilla-formula tool stats, textures, names and recipes; `/give`, creative search and the tabs the mod chose (Fabric `CreativeModeTabEvents`) find them | ✅ (in vanilla's Tools and Combat tabs) |
| **JamLib, Iceberg, Placeholder API, Fabric API** | Load and initialise as libraries for the mods above | ✅ |

Known gaps are tracked in [the compatibility matrix](docs/compatibility-matrix.md) and the
[runtime design note](docs/architecture/fabric-runtime.md). Notably: Legendary's item model is
drawn flat and does not rotate (the HUD renderer has no 3D model transform), and its shine
is not checked live; mod log messages print their `{}` placeholders
unfilled. Editing a config file and restarting works in
the live client (checked with Mod Menu), but reading an existing file back has no test yet.

## How it works

```text
            Fabric .jar files in horizon-mods/
                           │
          ┌────────────────┴────────────────┐
          ▼                                 ▼
   HFC installer (host side)       hfc-runtime (in the client)
   reads jars as data              interprets mod bytecode
          │                                 │
   content → Horizon content mods   Java / Minecraft / Fabric API
   + report.md / report.json        classes answered in Rust; the
                                    client performs host actions
                                    (rendering, files, links, clipboard)
```

The runtime prefers executing a mod's real bytecode, JDK and Minecraft classes included.
It adds a Rust builtin only at a host boundary (I/O, registry data, rendering, the desktop)
or for a class it deliberately replaces. The design is in
[docs/architecture/fabric-runtime.md](docs/architecture/fabric-runtime.md), and the reasoning
behind the content-first installer in
[docs/decisions/0001-hybrid-content-first.md](docs/decisions/0001-hybrid-content-first.md).

## Getting started

### Requirements

- A stable Rust toolchain.
- A checkout of the Horizon client at `workspace/horizon`, at the commit pinned in
  [docs/research/horizon-current-state.md](docs/research/horizon-current-state.md).
- For runtime tests: a local Minecraft 26.3 installation and the pinned Fabric jars in
  `test-mods/` (see [test-mods/README.md](test-mods/README.md)). Neither is committed.

### Build and test

```sh
cargo test --workspace            # HFC crates and the runtime
cargo fmt --all -- --check
cargo build --release -p hfc-cli  # the hfc installer
```

The Horizon client is a separate workspace:

```sh
cd workspace/horizon
cargo build --profile fast -p horizon-client --no-default-features
```

`--no-default-features` drops the audio and controller dependencies, which need ALSA and
libudev development files on Linux.

> [!TIP]
> The full runtime suite loads the whole Minecraft classpath and is heavy. While working,
> run one test file at a time, for example
> `cargo test -j2 -p hfc-runtime --test config_files -- --test-threads=1`.

### Using the installer

```text
horizon-mods/
├── HorizonFabricCompat/      ← the compatibility installation (hfc.toml)
├── fabric-api.jar            ← ordinary Fabric jars, untouched
├── appleskin.jar
└── ...
```

1. Install HFC once: `hfc install --mods <game folder>/horizon-mods`.
2. Drop Fabric jars beside `HorizonFabricCompat`.
3. Press Play. The launcher runs `hfc bootstrap` when `HorizonFabricCompat/hfc.toml` exists.
4. HFC converts each supported jar into a content mod, enables it, and writes
   `HorizonFabricCompat/report.md` and `report.json` saying what was carried for each mod.

Other commands: `hfc scan`, `hfc status`, `hfc prune`. The launcher finds `hfc` via
`HFC_BIN`, beside itself, or on `PATH`.

## Repository layout

| Path | What |
|---|---|
| `crates/hfc-core` | Limits, errors, hashing, atomic writes |
| `crates/hfc-fabric` | Jar discovery, `fabric.mod.json`, version ranges, dependency graph |
| `crates/hfc-cache` | Content-addressed conversion cache |
| `crates/hfc-bootstrap` | Conversion run loop, install, state and reports |
| `crates/hfc-cli` | The `hfc` command line |
| `crates/hfc-runtime` | Experimental in-client Fabric bytecode interpreter |
| `docs/` | Architecture, decisions, research, roadmap, testing, compatibility matrix |
| `test-mods/` | Local pinned Fabric jars for runtime tests (ignored by Git) |
| `workspace/horizon` | Horizon client checkout (separate repository, ignored) |

## Contributing

Contributions are welcome: bug reports, a mod that fails to load, missing builtins, docs.
Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request. The short version:

- **Never fake compatibility.** A mod whose entrypoint returns without an error is not
  "working". Say exactly what is rendered, dispatched or stored.
- **No mod-specific code.** Fix the shared path (an opcode, a Fabric API event, a Java or
  Minecraft builtin, a file format, a host boundary). Never branch on a mod id, package,
  class name, jar name or version.
- **Don't weaken checks to make a test pass.** No skipped assertions, swallowed errors or
  fabricated classes in tests that claim real-classpath coverage.
- **Never commit game or third-party binaries**, saves, secrets or session files.
- **Test what you claim**, with the mod's real bytecode, and keep scratch runs out of real
  player data.

## License

[MIT](LICENSE). Minecraft is a trademark of Mojang Studios; this project is not affiliated
with Mojang or Microsoft. Fabric mods keep their own licenses and are never redistributed
here.
