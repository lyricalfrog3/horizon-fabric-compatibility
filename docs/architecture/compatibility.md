# Compatibility model

HFC keeps two separate questions apart, because conflating them is how compatibility tools
start lying:

- **Can the jar's content be carried into Horizon?** Answered by Horizon's own carry pipeline.
- **Is the mod's Fabric behaviour satisfied?** Answered conservatively by HFC's analysis.

A mod can be installable and useful while its behaviour is not satisfied. The report says
both, per mod, in plain language.

## What is read

`fabric.mod.json` (schema 1) is parsed in full. Nothing else in the jar is interpreted as code.

| Fabric field | Use |
|---|---|
| `id`, `version`, `name`, `description` | identity, reports, dependency providers |
| `authors`, `contributors`, `contact`, `license` | report only |
| `environment` | `server` is warned: Fabric server entrypoints are not run |
| `entrypoints` | counted; presence means code HFC cannot run |
| `mixins` | configs read as **JSON** for `package`/`target` (census and report only) |
| `accessWidener` | presence means code/mixin-level access; file existence checked |
| `jars` | declared nested jars scanned one level (256 max) and used as dependency providers |
| `depends`, `recommends`, `suggests`, `conflicts`, `breaks` | dependency graph and diagnostics |
| `custom`, `icon` | preserved as raw JSON for provenance, never interpreted |

The archive is also censused by path (`assets/`, `data/<ns>/…`, blockstates, models, textures,
lang, recipes, loot tables, tags, worldgen, structures) so the report explains what content
exists before conversion. Census is descriptive; it does not claim the carry pipeline supports
every file it sees.

## Version semantics

HFC implements Fabric Loader's comparison rules directly (`hfc-fabric::version`):

- up to any number of numeric components, compared left to right, missing components are 0;
- SemVer pre-release ordering, including Fabric's empty pre-release as the earliest;
- `=`, `>`, `>=`, `<`, `<=`, `^`, `~`, X-ranges (`1.20.x`);
- whitespace/comma means AND within a range string; a JSON array means OR;
- build metadata (`+…`) ignored for ordering.

If either side is not comparable, the result is `Match::Unknown`, never `Yes`. Unknown
dependency ranges produce a **warning** and `runtime_ready = false`; unknown conflict/break
ranges are treated as a possible conflict and block the declaring side. Guessing is always
reported.

## Classification

Each `ModNode` carries two booleans:

| `installable` | `runtime_ready` | `status()` | Meaning |
|---|---|---|---|
| true | true | `ready` | no code signals, dependencies satisfied; only content exists to carry |
| true | false | `content-only` | content can be carried, Fabric behaviour is not provided |
| false | any | `blocked` | structural problem; content is not installed by HFC |

`runtime_ready` is true only when the archive passed structural checks, the mod declares no
entrypoints/mixins/access widener, and every `depends` entry resolves with `Match::Yes`.
`server` environment forces it false. Missing/conflicting dependencies and cycles force it
false. A cycle additionally clears `installable` because HFC refuses to pick an arbitrary
order.

The run result (`ModStatus`) is a different axis — what happened in this run: `installed`,
`reused`, `would install`, `not portable`, `blocked`, `failed`, `scanned`. A `content-only`
mod is normally `installed`; a structurally valid mod can still be `not portable` when the
carry pipeline finds nothing it can carry (common for pure libraries).

## Dependency resolution

- Providers are top-level mods and nested jars; built-ins are `minecraft`, `fabricloader`,
  `quilt_loader` (same version as Fabric Loader) and `java`.
- `depends` on a missing provider is an **error** and clears `runtime_ready`.
- A declared version mismatch is an **error**; an incomparable range is a **warning**.
- `conflicts`/`breaks` are symmetric: if A declares a conflict with installed B, **both** are
  marked uninstallable. Picking one side silently would change behaviour.
- Cycles are detected after topological sorting; the report names one cycle. All members are
  blocked and excluded from the install order.
- `recommends` missing is `Info`; `suggests` is accepted and ignored (as Fabric does).
- Duplicate ids: a top-level jar wins over a nested provider, then the lexically first file
  wins. The winner stays installable; the loser is an error plus a `skipped` entry. This is
  deterministic, not "last one wins".
- `order` is dependency-first, then deterministic (BTreeMap/queue order), and contains only
  installable top-level mods. Nested providers satisfy dependencies but are not installed as
  sibling folders; their assets travel with the parent jar.

## Additional install rules

These are Horizon constraints rather than Fabric ones:

- Generated folder ids must match `[a-z0-9_]{1,64}`. `fabric-api` cannot be a Horizon folder
  mod, so such jars are analyzed and then blocked with an actionable message.
- Two content mods may not own the same namespace. The first in dependency order keeps it;
  the second is blocked and the collision is reported. HFC does not merge namespaces.
- A target folder without HFC's `.hfc-generated.json` is the player's and is never replaced.
  The mod is reported as blocked.
- The carry verdict `No` means `not portable`; nothing is installed.

## What "not supported" means

The following are detected and reported, never emulated:

| Feature | v1 behaviour |
|---|---|
| Java entry points (`main`, `client`, `server`, …) | counted; not run; `runtime_ready = false` |
| Mixins | configs parsed for targets; never applied; `runtime_ready = false` |
| Access wideners | presence detected; not applied |
| Fabric API modules | treated as ordinary dependencies; a missing one is an error, a present one only satisfies metadata — its Java API is not implemented |
| Client screens, keybinds, HUD | not carried; assets may still be carried |
| Networking, entities, block entities, menus | not carried; data referencing them may be incomplete |
| World generation / structures | passed to the carry pipeline; only what it supports is carried |
| Shader packs (Iris/OptiFine) | separate subsystem, not part of jar conversion — see [shaders.md](shaders.md) |
| Resource packs inside a mod | carried only where the pipeline supports the paths |

"Content-only" is the honest label for most real mods today. It means the player gets the
blocks/items/recipes/textures that Horizon can load, without the Java behaviour that made them
work in Minecraft.

## Versioning the contract

`COMPAT_API` (`0.1`) identifies the meaning of a generated port and its report. `PIPELINE_VERSION`
identifies the conversion/install format. Both are embedded in cache keys, generated markers
and reports, so:

- changing either invalidates old cache entries instead of silently reusing them;
- a future HFC can identify ports produced by an older contract;
- no generated port is assumed valid across a compatibility API change.

The API is deliberately versioned before there is any code-port API, so the first generated
native mods inherit a key format that can evolve without guessing.
