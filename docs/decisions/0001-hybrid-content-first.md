# ADR 0001 — Hybrid, content-first compatibility with a host-side bootstrap

- Status: accepted
- Date: 2026-11-02
- Horizon: `26c83240d6d2e3bfbda5314c9f8d1274fba0ec1a` (`VERSION_ID` 26.3)

## Context

Horizon is a Rust implementation of Minecraft with no JVM and no Java Minecraft classes. Its
two mod kinds are:

- content mods: inert data/assets loaded at startup;
- native mods: Rust source bundles compiled by the launcher, explicitly **not sandboxed**.

Fabric jars contain Java bytecode, entry points and Mixin patches against Minecraft Java
classes. They cannot run as-is, and a content mod cannot inspect sibling jars, spawn a
process or generate other mods: it is data, read after launch.

The product requirement is nevertheless drop-in: ordinary Fabric jars in the normal mods
folder, no manual conversion, originals untouched.

## Decision

1. **Do not embed a JVM in v1.** It would not solve the missing Minecraft Java surface, it
   would add a full-privilege code path, and it conflicts with Horizon's security model.
2. **Implement HFC as a trusted, host-side pre-launch bootstrap**, not a content mod. The
   launcher runs it when `horizon-mods/HorizonFabricCompat/hfc.toml` exists. This is the
   smallest host integration that can read the filesystem and write generated mods.
3. **Reuse `horizon-port-pack`** for content conversion. HFC adds metadata, version ranges,
   dependency resolution, safety validation, caching, installation and reporting around it.
4. **Install generated content as ordinary folder mods** (`horizon-mods/<id>/`) carrying a
   `.hfc-generated.json` marker. Never modify originals; never delete a folder HFC did not
   create.
5. **Separate "installable" from "runtime-ready"** in the data model and reports. Content can
   be carried even when the mod's Java code cannot run; HFC says which is which.
6. **Defer code compatibility to generated native mods** built through the real
   `horizon-mixins` / native hook API, with the existing trust and review model. No fake
   Minecraft Java API, no bytecode interpreter, no silent execution.

## Architecture

```text
horizon-mods/
  HorizonFabricCompat/hfc.toml
  *.jar                         (originals, read-only)
        │
        ▼
  launcher pre-launch hook  ── hfc bootstrap --mods <mods>
        │
        ▼
  discovery + safe ZIP scan (hfc-fabric)
        │
        ▼
  fabric.mod.json / version ranges / dependency graph
        │
        ├──────────────► report.json / report.md
        ▼
  reusable Horizon check+carry (horizon-port-pack)
        │
        ▼
  content-addressed cache (hfc-cache)
        │
        ▼
  install horizon-mods/<id>/ + marker + horizon-mods.txt
        │
        ▼
  Horizon content loader (inert data)
```

There is no path from HFC to executable code in v1.

## Consequences

Positive:

- Safe: no Java execution, no decompilation, no new sandbox to attack.
- Immediately useful for content mods and honest about everything else.
- Reuses the pipeline that Horizon's own mod browser uses.
- Fast repeat launches through the cache.
- A clear, testable foundation for later API and native-port phases.

Negative / costs:

- Most gameplay mods are content-only or not portable until later phases.
- Requires a launcher hook and a separate `hfc` executable; it is not a single `.hmod`.
- `horizon-port-pack` rejects ids outside `[a-z0-9_]{1,64}` and jars older than 1.21; HFC
  reports those rather than working around them.
- Nested-jar content is not carried separately in v1.
- A future code path must still solve Mixin semantics, API surface, behavioural testing and
  user trust.

## Rejected alternatives

- **Embedded JVM (A):** no Minecraft classes, full-privilege execution, large runtime.
- **Bytecode AOT/WASM (B):** closed-world/dynamic-loading mismatch; still no API.
- **Pure automatic native porting now (C):** too large for a first slice and untestable at
  scale without per-mod oracles.
- **Making HFC a content mod:** it cannot perform filesystem discovery or generation.
- **Recursive mod loading in Horizon:** would change the engine's security and layout rules
  for everyone; a launcher hook is smaller and reversible.

## Verification

- Unit tests for version ranges, metadata, ZIP safety, graph resolution, cache invalidation,
  installation markers and reports.
- End-to-end tests with synthetic jars: install, cache hit, JAR-change invalidation, library
  skip, broken jar, dry run.
- Launcher hook tests with a fake `hfc` executable.
- The generated folder is Horizon's real content format; this was checked against
  `horizon-port-pack` output and `docs/mods.md`, not simulated.
