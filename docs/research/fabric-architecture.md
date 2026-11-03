# Fabric: what a mod expects

Based on Fabric's published specification and loader documentation. HFC implements only the
metadata and version-range parts; the runtime parts are listed so the boundary is explicit.

## `fabric.mod.json` (schema version 1)

Mandatory: `schemaVersion`, `id`, `version`. ids are `^[a-z][a-z0-9-_]{1,63}$` in the
specification; HFC accepts a leading digit too because real jars predate the rule, but never
uppercase, whitespace, a path separator or an empty id.

Fields HFC reads and preserves:

- `name`, `description`, `authors`/`contributors` (string or `{name, contact}`), `contact`,
  `license` (string or array), `icon`.
- `environment`: `client`, `server`, `*`, or an array of those.
- `entrypoints`: map of entry point kind → string, object (`{adapter, value}`), or array of
  either. The default adapter is `main`; Kotlin, ModMenu and others use named adapters.
- `mixins`: array of config paths or `{config, environment}` objects.
- `accessWidener`: a path to an `.accesswidener` file.
- `jars`: array of `{file}` nested jars under `META-INF/jars/`.
- `depends`, `recommends`, `suggests`, `conflicts`, `breaks`: maps of mod id → version range.

Unknown top-level keys are tolerated, as the specification requires. A metadata file may also
be a JSON array whose first element is used (older Loader behaviour).

HFC validates mandatory fields and rejects a `schemaVersion` newer than 1. Everything else is
read as data; no class is loaded.

## Version ranges

Fabric compares versions with a superset of Semantic Versioning:

- Any number of numeric components, compared left to right; missing components are zero.
- A pre-release (`-alpha.1`) sorts before its release; an empty pre-release (`1.2-`) sorts
  before every other pre-release.
- Operators `=`, `>`, `>=`, `<`, `<=`, `^` (next major), `~` (next minor, else next major).
- X-ranges: `1.20.x`, `1.*`.
- A string is a set of AND-ed comparators separated by spaces and/or commas.
- A JSON array is OR-ed (`[">=1.21", "<1.20"]`).
- `*` means any version.

HFC returns three answers: `Yes`, `No`, `Unknown`. `Unknown` is used for non-comparable
versions or syntax HFC cannot parse; it is never treated as success. The resolver reports an
`unresolved-version` warning and marks the mod not runtime-ready.

## Entry points and lifecycle

Fabric Loader instantiates entry point classes and calls them at lifecycle stages
(`preLaunch`, `main`, `client`, `server`). Fabric API builds on this with events: registry
callbacks, server/client tick, player join/leave, block interactions, screen events, rendering,
networking, configuration. None of this can run in Horizon without a Java runtime or an
equivalent native implementation of every class the mod touches.

HFC detects entry points and reports their count. It does not execute them.

## Mixins

SpongePowered Mixin transforms Minecraft classes at load time using bytecode injection
(`@Inject`, `@Redirect`, `@ModifyArg`, `@Overwrite`, accessors, etc.). A mixin config names a
package and a list of mixin classes plus optional `client`/`server` lists. Targets may be
explicit (`target`) or derived from the mixin class name.

HFC reads mixin configs as JSON, extracts declared targets for the report, and stops. Mixins
are deeply coupled to Java class identity and method descriptors; they are the hardest part of
any Java-to-native strategy.

## Access wideners

An access widener is a text file that widens (or makes transitive/mutable) access to Minecraft
classes, methods and fields. HFC verifies that a declared widener exists in the jar and reports
it; applying one is meaningful only to a Java runtime.

## Nested jars

Fabric Loader loads jars listed in `jars` as if they were separate mods, including their own
`fabric.mod.json`. HFC follows declared nested jars one level (a nested jar's own nested
declarations are reported, not read), records their
metadata, hash and census, and lets them satisfy dependencies. A nested mod is **not**
installed as a sibling content folder in v1: the outer jar is the unit the carry pipeline
sees, so content inside nested jars may not be carried. The report says this explicitly.

## Dependencies

- `depends`: required; missing or out-of-range prevents the Fabric mod from loading.
- `recommends`: optional but expected; HFC reports absence.
- `suggests`: informational.
- `conflicts`: cannot coexist with the matching version.
- `breaks`: cannot coexist with the matching version of a specific mod.

HFC's resolver distinguishes:

- **installable**: the jar can be carried as content;
- **runtime-ready**: HFC believes Fabric's dependency contract is satisfied and the mod's code
  behaviour is actually available.

A content jar with a missing `fabric-api` dependency is installable but not runtime-ready.
That distinction is deliberate and appears in every report.

## What this means for Horizon

Fabric mods are not self-contained programs. They are patches and callbacks against a specific
Java implementation of Minecraft. Parsing metadata is necessary but nowhere near sufficient
for compatibility. A compatibility layer needs either a Java runtime plus a reimplementation
of Minecraft's Java surface, or native reimplementations of enough API and behaviour that mod
code can be regenerated. HFC starts with the part that is unambiguously safe and useful:
content extraction through Horizon's existing pipeline.
