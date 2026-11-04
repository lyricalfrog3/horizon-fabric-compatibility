# Documentation

Start with the [README](../README.md) for the player-facing flow and the
[compatibility matrix](compatibility-matrix.md) for what actually works today.

## Architecture

| Page | What it answers |
|---|---|
| [overview.md](architecture/overview.md) | what runs, in what order, in which process |
| [runtime.md](architecture/runtime.md) | launcher hook, failure behaviour, turning HFC off |
| [compatibility.md](architecture/compatibility.md) | metadata, version ranges, classification, dependency rules |
| [caching.md](architecture/caching.md) | cache key, atomic commit, invalidation |
| [security.md](architecture/security.md) | trust boundaries, hostile-archive handling, residual risks |
| [shaders.md](architecture/shaders.md) | the separate Iris/OptiFine shader track |
| [fabric-runtime.md](architecture/fabric-runtime.md) | the experimental in-client Fabric bytecode interpreter, its budget, fixes and limits |

## Decisions

| ADR | Decision |
|---|---|
| [0001](decisions/0001-hybrid-content-first.md) | host-side hybrid, content first, no embedded JVM in v1 |
| [0002](decisions/0002-cache-and-install.md) | content-addressed cache and marked folder installs |

## Research

| Page | What was investigated |
|---|---|
| [horizon-current-state.md](research/horizon-current-state.md) | Horizon's mod architecture, port pipeline, launcher flow, exact commits |
| [fabric-architecture.md](research/fabric-architecture.md) | Fabric Loader metadata, lifecycle, Mixins, Fabric API |
| [java-runtime-research.md](research/java-runtime-research.md) | JVM embedding, AOT/WASM, bytecode analysis options |
| [existing-projects.md](research/existing-projects.md) | prior Fabric-on-other-engine efforts and lessons |
| [initial-plan.md](research/initial-plan.md) | the original project prompt (historical) |

## Delivery

| Page | What it covers |
|---|---|
| [roadmap.md](roadmap.md) | phases, exit gates, non-goals |
| [testing.md](testing.md) | test levels, current inventory, gaps |
| [compatibility-matrix.md](compatibility-matrix.md) | per-capability honest status |
