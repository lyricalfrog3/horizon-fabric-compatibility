# Compatibility matrix

This is the honest state of the first vertical slice (HFC `0.1`, compatibility API `0.1`,
pipeline `1`, Horizon `26.3`). "Supported" means HFC has an automated test that exercises the
path; "delegated" means the path exists in Horizon's carry pipeline and HFC calls it, but HFC's
own end-to-end tests do not cover every content type.

The matrix covers the HFC installer and its carried content. It does **not** cover the
experimental in-client interpreter in `crates/hfc-runtime`; that runtime has its own
[design note](architecture/fabric-runtime.md) and its own limits (no Mixins, no general
Fabric API, and only a partial replayed GUI: Mod Menu's list opens and filters, but mod icons
are placeholder tiles, scissor clipping is not replayed, and some description text is missing).

## Player-facing result

| Mod kind | What the player gets | Runtime behaviour |
|---|---|---|
| Pure data/content jar (no entrypoints, mixins or access widener) | generated content folder, enabled | content loads; `ready` |
| Content mod with Java registration code (the common case) | blocks/items/models/textures/lang/recipes/tags that the carry pipeline supports | content loads; **Java code does not run**; `content-only` |
| Library / code-only mod | nothing installed | `not portable` (or blocked on a missing dependency) |
| Client UI/keybind/HUD mod | assets may be carried; the installer provides no UI, keys or HUD (the separate experimental runtime can replay some screens) | `content-only` at best |
| Shader pack (Iris/OptiFine) | not handled by jar conversion | see [shaders.md](architecture/shaders.md) |
| Mod with an id Horizon cannot load (e.g. `fabric-api`) | analyzed, not installed | blocked with a rename message |
| Broken/non-Fabric jar | analyzed, reported, skipped | untouched |

`runtime_ready` never means "the Java mod works". It means HFC found no code signals and every
declared dependency resolved. For nearly every real gameplay mod it is false, and the report
says why.

## Content

| Capability | Status | Evidence / notes |
|---|---|---|
| Blockstates, block/item models, textures | **supported** | E2E synthetic jar installs and reuses a folder mod |
| Language files | **supported** | Carried by the pipeline; E2E changes one jar and reconverts |
| Item model definitions (`items/`) | delegated | Pipeline writes them for 1.21.1→26.3 |
| Blocks and items | delegated | Pipeline census + carry; E2E fixture exercises a block/item |
| Recipes (1.21.1→26.3 upgrade) | delegated | Pipeline upgrades; custom/NeoForge recipes may be kept or moved to `outside/` |
| Loot tables | delegated | Pipeline upgrades and can add missing self-drops from oracle facts |
| Tags (block/item/… and conventions) | **supported** (basic) | E2E fixture carries a block tag |
| Advancements | delegated | Pipeline rewrites criteria where possible |
| Worldgen and structures | delegated | Pipeline upgrades known types; unsupported ones are reported |
| Equipment assets | delegated | Pipeline writes armour/equipment assets |
| Sounds, fonts, particles, GUIs | not supported as behaviour | Raw assets may be copied; nothing consumes them without code |
| Resource packs inside a jar | partial | Only paths the carry pipeline accepts; no pack semantics |
| Files outside the mod namespace | not installed | Pipeline keeps them in `outside/` for review |

"Delegated" is not a weaker claim about HFC's plumbing, but it is a weaker **test** claim: the
port pipeline has its own large test suite (`horizon-port-pack`: 107 unit + 37 integration
tests in this checkout) and HFC reuses it unchanged. HFC's job is to hand it the jar and
install the result safely.

## Metadata and dependency analysis

| Capability | Status |
|---|---|
| `fabric.mod.json` schema 0/1, object or first-element array | supported |
| `id`, `version`, `name`, `description`, authors, contributors, contact, licence | supported |
| Environment (`client`/`server`/`*`, string or array) | supported; server-only is warned |
| Entry points (all types, string/object/array forms) | parsed and counted; never run |
| Mixin configs (string/object forms), targets read as JSON | supported for census; never applied |
| Access widener declaration | detected and existence-checked; never applied |
| Nested jars (`jars`, `META-INF/jars`) | one level scanned; dependency providers; not separate folders |
| Depends / recommends / suggests / conflicts / breaks | supported |
| Fabric version ranges (`=`, `>`, `>=`, `<`, `<=`, `^`, `~`, X, AND/OR) | supported |
| Incomparable versions | `Unknown`, reported, never guessed |
| Duplicate ids | deterministic winner (top-level, then lexical); loser reported |
| Missing dependency / version mismatch | error, `runtime_ready = false` |
| Conflicts / breaks | both sides blocked |
| Dependency cycles | detected, cycle named, members blocked |
| Multi-jar folder with mixed good/bad jars | supported; one bad jar does not stop the run |
| Built-in providers (`minecraft`, `fabricloader`, `quilt_loader`, `java`) | supported from `hfc.toml` |

## Host integration

| Capability | Status |
|---|---|
| Launcher detects `HorizonFabricCompat/hfc.toml` | implemented + 3 hook tests |
| Launcher runs `hfc bootstrap --mods <home>/horizon-mods` | implemented; binary compiles |
| No marker → no work | implemented + test |
| Failure shown but launch continues | implemented + test |
| Binary discovery (`HFC_BIN`, sibling, `PATH`) | implemented |
| Generated folder read by Horizon's `horizon-mods` loader | implemented + E2E (`Package::read`, `Content::read`) |
| Full game launch with a generated mod | **not yet tested here**; requires a Horizon install and a real jar |
| Windows/macOS launch | not tested; code is platform-neutral, launcher builds differ |
| Concurrent launches | atomic but unlocked; duplicate work possible |

## Security

| Property | Status |
|---|---|
| No Java execution, decompilation or class loading by HFC | by construction; the scanner reads data only (the separate `hfc-runtime` is not part of this boundary) |
| No JAR modification or redistribution | by construction + E2E byte-identical assertion |
| Path traversal, absolute/drive paths, duplicate entries | rejected + unit tests |
| Size, entry, uncompressed, metadata, nested-depth limits | enforced + unit tests |
| Malformed zip/JSON/TOML | per-jar skip, run continues + tests |
| Only HFC-marked folders removed/replaced | installed marker check + tests |
| Stale cache reuse | impossible by key; verified on lookup + tests |
| Atomic writes and interrupted-run recovery | implemented + tests |
| Generated code / native mods | none in v1 |
| Shader packs | not handled |

## Explicitly not compatible in the installer path (v1)

- Fabric Loader entry points, Mixins, access wideners and Fabric API.
- Client screens, keybinds, HUD, rendering hooks and client events; the experimental runtime
  has a partial screen host, but it is not part of the installer and makes no general claim.
- Networking, entities, block entities, menus, inventories as *behaviour*.
- Anything that requires Minecraft Java classes or a JVM.
- Iris/OptiFine shader packs.
- Automatic generation or compilation of native Horizon mods.
- Claims of parity based on metadata alone.

## How to extend this matrix

A row moves to **supported** only with a test that demonstrates it end to end, not because an
API exists. Add the smallest owned jar fixture to `crates/hfc-bootstrap/tests/` (or a dedicated
fixture crate), assert the installed output, and update this table. A row that needs a new
renderer or engine capability stays "not supported" until that capability exists.
