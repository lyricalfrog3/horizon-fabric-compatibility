# Horizon Fabric Compatibility (HFC)

Drop-in Fabric mod support for [Horizon](https://github.com/horizon-rust/horizon): put
ordinary Fabric `.jar` files in the game's normal `horizon-mods` folder, launch the game, and
HFC discovers, analyzes, converts what Horizon can actually carry, caches the result and
writes a report. The conversion pipeline never modifies, decompiles or executes original
jars.

> **Status: first vertical slice (content-only).** HFC installs an HFC-generated Horizon
> **content** mod from a Fabric jar. It does **not** run Fabric/Java code, Mixins, entry
> points, Fabric API, client UI or networking. The report says so per mod; HFC never claims
> runtime compatibility it has not demonstrated. See
> [the compatibility matrix](docs/compatibility-matrix.md).
>
> **Separate experimental track.** `crates/hfc-runtime` is an in-client bytecode interpreter
> for selected Fabric entrypoints, wired into Horizon under `workspace/horizon`. It is not
> part of the HFC installer and is not covered by the compatibility claims above. It runs
> Right Click Harvest, dispatches tooltip callbacks, and replays Mod Menu's own screen from
> a recorded draw list (mod icons, search, scrolling, the title and pause menu buttons its
> mixins add). It is not general Mixin support, a general widget engine, gameplay or
> networking compatibility, and the interpreter is not a sandbox. See
> [the runtime design note](docs/architecture/fabric-runtime.md).

## The user experience

```text
horizon-mods/
├── HorizonFabricCompat/      ← the compatibility installation (hfc.toml)
├── fabric-api.jar            ← ordinary Fabric jars, untouched
├── farmers-delight.jar
├── appleskin.jar
└── ...
```

1. Install HFC once: `hfc install --mods <game folder>/horizon-mods`.
2. Drop Fabric jars beside `HorizonFabricCompat`.
3. Press Play. The launcher runs `hfc bootstrap --mods <mods>` when
   `horizon-mods/HorizonFabricCompat/hfc.toml` exists.
4. HFC converts each supported jar into `horizon-mods/<id>/` (a folder content mod), enables
   it in `horizon-mods.txt`, and writes
   `horizon-mods/HorizonFabricCompat/report.md` / `report.json`.

There is no separate conversion step for the player.

## What it does today

- Safe discovery of every `.jar` directly in the mods folder.
- Full `fabric.mod.json` (schema 1) parsing: id, version, name, authors, licence, side,
  entry points, mixins, access widener, nested jars, all five dependency maps.
- Fabric version-range evaluation: `=`, `>`, `>=`, `<`, `<=`, `^`, `~`, X-ranges, arrays
  as OR, space/comma as AND. Incomparable versions are `Unknown`, never guessed.
- Multi-mod dependency graph with deterministic dependency-first order, duplicate-id
  detection, missing/mismatched dependencies, conflicts/breaks, cycles, built-in providers
  (`minecraft`, `fabricloader`, `java`).
- Defensive ZIP handling: traversal/absolute-path rejection, entry/uncompressed-size caps,
  nested-jar depth caps, malformed metadata, duplicate central-directory names.
- Content conversion through Horizon's existing `horizon-port-pack` check/carry pipeline:
  blocks, items, models, textures, language files, recipes, loot tables, tags, worldgen,
  structures. No duplicate porting code.
- Content-addressed cache keyed by JAR SHA-256 + Horizon version + compatibility API +
  pipeline version + platform + carry profile; atomic entry commit; stale/incomplete
  entries are misses.
- Atomic installation with a `.hfc-generated.json` marker; HFC only ever removes or
  replaces its own folders.
- State, machine-readable report and Markdown report; `horizon-mods.txt` kept in sync.
- Launcher hook: one small module, no marker → no work; failure is reported but does not
  block the launch.

## What the HFC installer does not do (yet)

- Execute Java bytecode, entry points or Mixins as part of conversion.
- Implement Fabric API or Minecraft Java classes for carried content.
- Generate native Rust mods / `.hnmod` bundles automatically.
- Carry code behaviour, client screens, networking, entities or block entities.
- Translate shader packs (a separate subsystem; see the roadmap).

The experimental `hfc-runtime` can interpret selected entrypoints in the client, but it does
not provide Java screens, Mixins, general Fabric API coverage or UI compatibility.

## Build and test

```sh
cargo test --workspace
cargo fmt --all -- --check
cargo build --release -p hfc-cli
```

The launcher hook is a separate workspace:

```sh
cd workspace/horizon/launcher
cargo test --lib fabric_compat
cargo check --bin horizon-launcher
```

Test coverage and known gaps are in [docs/testing.md](docs/testing.md).

The workspace reuses the local Horizon checkouts under `workspace/` for
`horizon-core` and `horizon-port-pack` (exact commits are in
[docs/research/horizon-current-state.md](docs/research/horizon-current-state.md)).

Run the tool:

```sh
target/release/hfc install   --mods "/path/to/game/horizon-mods"
target/release/hfc scan      --mods "/path/to/game/horizon-mods"
target/release/hfc bootstrap --mods "/path/to/game/horizon-mods"
target/release/hfc status    --mods "/path/to/game/horizon-mods"
target/release/hfc prune     --mods "/path/to/game/horizon-mods"
```

The launcher finds `hfc` via `HFC_BIN`, beside itself, or on `PATH`.

## Repository layout

| Path | What |
|---|---|
| `crates/hfc-core` | Limits, errors, hashing, atomic writes, shared constants |
| `crates/hfc-fabric` | Jar discovery/scan, `fabric.mod.json`, version ranges, dependency graph |
| `crates/hfc-cache` | Content-addressed conversion cache |
| `crates/hfc-bootstrap` | Run loop, conversion, install/state, reports |
| `crates/hfc-cli` | `hfc` command line |
| `crates/hfc-runtime` | Experimental in-client Fabric bytecode interpreter (not part of the installer) |
| `docs/` | [Research, ADRs, architecture, roadmap, compatibility matrix](docs/README.md) |
| `workspace/horizon` | Horizon checkout; contains the scoped launcher hook |
| `workspace/horizon-mods` | Reference mods checkout |
| `research/horizon-mixins` | Mixin tooling reference |

## Design in one paragraph

Horizon has no JVM. Its content mods are inert data and assets; its native mods are Rust
source bundles compiled by the launcher with the user's privileges. A content mod therefore
cannot scan sibling jars or generate other mods. HFC is a trusted **host-side pre-launch
step**, not a content mod: it inspects untrusted archives as data, reuses Horizon's own carry
pipeline for content, and installs ordinary content folder mods. Code compatibility is
deliberately deferred to generated native mods through Horizon's supported SDK and hook
points. The separate `hfc-runtime` experiment interprets selected Fabric entrypoints in the
client without a JVM; it does not change what the HFC installer carries. See
[docs/decisions/0001-hybrid-content-first.md](docs/decisions/0001-hybrid-content-first.md).
