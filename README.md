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
| **Legendary Tooltips** (Iceberg, Prism) | Its textured nine-slice tooltip frame, rarity-matched colors and shadow; the tooltip body from the mod's real component list (centred title, name separator, the item model rotating in 3D); the shine that restarts per hovered stack; `legendarytooltips.toml` written with comments, allowed values and ranges, and read back | ✅ |
| **Emerald Tools** | Its five tools, registered in code, become Horizon items with vanilla-formula tool stats, textures, names and recipes; `/give`, creative search and the tabs the mod chose (Fabric `CreativeModeTabEvents`) find them | ✅ (in vanilla's Tools and Combat tabs) |
| **Emerald Armor** | Its four armor pieces, registered in code with `Item.Properties.humanoidArmor`, become Horizon items with vanilla-formula durability, armor and toughness, a single stack, and an equippable slot; its worn-armor model and textures (under `assets/minecraft`) are carried | ✅ (worn and rendered) |
| **Modern Decorations** | Its 344 blocks, registered in code, become Horizon blocks from facts read by running the mod's real block code: states, hardness, light, sounds, outline and collision shapes; they render with the jar's models, drop through its loot tables, and come with their items. Placement runs the mod's own `getStateForPlacement` and `setPlacedBy` (facing, multi-block furniture); right-click runs the block's own `useWithoutItem`/`useItemOn` (a shower head toggles its water); scheduled block ticks run (they are saved in the chunk's vanilla `block_ticks`, so a running shower resumes after a reload) with the particles they send; and the 130 storage blocks are 27-slot containers with the mod's own stack limit, per-face hopper rules, items and drops | ✅ placing, facing, storage, use |
| **Farmer's Delight** | Its 91 items and 132 of its 135 blocks, registered in code, load from facts read by running the mod's own block code, outline and collision shapes included, per facing and per `bites` where the block has them (the canvas signs are vanilla `WallSignBlock` shapes, which are cached in a map keyed by `Direction`); foods are edible (`Item.Properties.food` exports `minecraft:food`/`minecraft:consumable`), and the creative tab the mod builds in code runs its own item generator and appears in the creative inventory | ✅ placing, eating, own tab |
| **JamLib, Iceberg, Placeholder API, Fabric API** | Load and initialise as libraries for the mods above | ✅ |

Known gaps are tracked in [the compatibility matrix](docs/compatibility-matrix.md) and the
[runtime design note](docs/architecture/fabric-runtime.md). Notably:

- Blocks: a mod block's own right-click code (`useWithoutItem`/`useItemOn`) runs with vanilla's
  sneak and pass rules, applying the states, drops, removals, ticks and particles it makes;
  scheduled block ticks run (a lit shower head sprays) and are saved with the chunk; and a
  container keeps the mod's stack limit and per-face hopper rules. A use that changes the held
  stack (`shrink`, `consume`, `setItemInHand`) is refused rather than leaving the item behind.
  Ticking block entities (`getTicker`), neighbour updates and per-stack slot rules
  (`canPlaceItem`) don't run, and blocks whose block entity is not a vanilla container stay
  decoration: Farmer's Delight's cooking pot and cutting board place but open no menu, and
  three of its crop blocks (cabbages, onions, rice panicles) are left out. Reading a mod's
  block facts and shapes makes its first start slower (23.6 s with Modern Decorations; later
  starts reuse the cache and take 9.8 s, the mod's own entrypoint, against about 3 s without
  it). Shape reading is memoized per `VoxelShape` object and bounded per block, and a block
  whose classes do not replace `getShape`/`getCollisionShape` is read once for all its states:
  Farmer's Delight's 2,580 states went from 0.64 s to 0.25 s of shape reading. Modern
  Decorations' 13 s is mostly its three `electric_guitar_*` blocks (~39M instructions each,
  the mod's own `Shapes.join` calls).
- Mods that carry blocks or entities around (Carry On) and most `Mixin`-heavy gameplay mods do
  not work.
- Mod advancements are granted and their tabs sit beside vanilla's; the whole tree is sent on
  the first packet so every root's tab exists from the start.
- Mod log messages print their `{}` placeholders unfilled.

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
                                    (rendering, particles, ticks, files,
                                    links, clipboard)
```

The runtime prefers executing a mod's real bytecode, JDK and Minecraft classes included.
It adds a Rust builtin only at a host boundary (I/O, registry data, rendering, the desktop)
or for a class it deliberately replaces. What a JVM's bootstrap does for the JDK, it does
explicitly: it installs the JDK's own `JavaLangAccess` when `SharedSecrets` initializes,
because `EnumMap`/`EnumSet` read an enum's constants through it, and Minecraft caches rotated
block shapes in `EnumMap`s keyed by `Direction`. Without it those maps are empty — the shapes
come back null and face-keyed lookups miss.

The JDK's native methods are answered at the same kind of boundary. `java.io.File` is the
JDK's own bytecode over the runtime's file system, which is the game folder (relative paths
resolve there, the folders above it exist, nothing outside it is listed or written);
`System`'s properties (`user.dir`, separators, encodings), `System.identityHashCode`, a
monotonic `nanoTime`, `Runtime.availableProcessors`, `String.intern` and the float bit
conversions come from the host with the values a JVM reports. Mods' config files are read and
written through it.

The runtime's own lists, maps and sets behave as the JDK classes they stand for: whatever the
collection builtins do not answer runs as `AbstractList`/`AbstractSet`/`AbstractMap` bytecode
(`toString`, `listIterator`, `equals`, `List.sort`), `TreeMap`/`TreeSet` and the JDK's
`Collectors` and comparators run as their own bytecode, and `Stream.collect` uses the
collector's supplier/accumulator/finisher. `BuiltInRegistries` take registrations (`key()`,
`register`, `createIntrusiveHolder`, `getKey`), and `Block.asItem()`/`new ItemStack(item)` work
for registered block items, so creative-tab generators record their items. On the test mods the
census's unresolved symbols went from 55 to 18 with the JDK slice and to 3 with this one, and the
interpreter's known divergences from a real JVM from 32 to 16. The design is in
[docs/architecture/fabric-runtime.md](docs/architecture/fabric-runtime.md), and the reasoning
behind the content-first installer in
[docs/decisions/0001-hybrid-content-first.md](docs/decisions/0001-hybrid-content-first.md).

## Getting started

### Requirements

- A stable Rust toolchain.
- The Horizon client, a Git submodule at `workspace/horizon`: the
  [lyricalfrog3/horizon](https://github.com/lyricalfrog3/horizon) fork, branch `hfc`, which
  carries the client hooks the runtime needs. Clone with
  `git clone --recursive git@github.com:lyricalfrog3/horizon-fabric-compatibility.git`, or run
  `git submodule update --init` in an existing clone.
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

Three runtime tools check more than a single mod:

```sh
# Compile small Java programs and compare the interpreter with the real JVM.
cargo test -p hfc-runtime --test jvm_diff
# Rank what the runtime lacks across a folder of mods, by mods affected.
cargo run --release -p hfc-runtime --example census -- --mods test-mods/corpus --alone
# Time a cold read of a mod set's block facts and shapes (RUST_LOG=debug: one line per block).
cargo run --release -p hfc-runtime --example block_profile -- \
    test-mods/fabric-api-0.165.0+26.3.jar test-mods/corpus/FarmersDelight-26.3-3.6.30+refabricated.jar
```

`tests/jvm/known_divergences.toml` is the maintained list of interpreter gaps; the census
writes `census.json` and `census.md` and accepts `--compare` for a before/after.

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
| `workspace/horizon` | Horizon client (submodule: the `lyricalfrog3/horizon` fork, branch `hfc`) |

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
