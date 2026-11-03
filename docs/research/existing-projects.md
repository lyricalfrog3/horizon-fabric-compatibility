# Existing projects and what HFC reuses

## Horizon (`workspace/horizon`)

- **`horizon-port-pack`** — the only content port pipeline in the project and the one behind
  the launcher's mod browser and modpack importer. HFC calls `check::check` per jar and copies
  its `carry/` output. HFC deliberately does not reimplement carry rules, recipe upgrades,
  block templates, namespace detection or manifest writing.
- **`horizon-mods`** — manifest parsing, content validation, mod discovery. HFC follows its id
  and namespace rules and its flat `horizon-mods/` layout.
- **`horizon-mixins` / native mods** — the supported code extension path. Native mods are
  source bundles compiled by the launcher, pinned to a commit or a stable hook API, and treated
  as trusted native code. This is the only acceptable target for future code compatibility.
- **Launcher** — owns instance home directories, the mod browser installation path and
  `horizon-mods.txt`. HFC adds one pre-launch call there rather than inventing a new loader.

## Fabric Loader / Fabric API

Fabric Loader defines the metadata, entry point, nested-jar, dependency, access-widener and
Mixin contracts. HFC implements the metadata/version-range subset needed for analysis.

Fabric API is not a single library: it is a large set of modules (lifecycle events, registry
callbacks, networking, transfer, rendering, screens, data generation, resource loading).
Supporting it means implementing the underlying Minecraft behaviour in Horizon and exposing
equivalent capabilities, not porting a jar.

## SpongePowered Mixin

Mixin transforms Java classes at load time. It is tightly coupled to the JVM's class identity,
method descriptors and class bytes. HFC reads mixin configs for reporting; applying them is not
possible without a Java runtime and Minecraft class bytes. Future work maps common Mixin
patterns to native hook points or generated Rust, and reports the rest as unsupported.

## Java/JVM tooling evaluated

| Project | Useful for | Why not v1 |
|---|---|---|
| OpenJDK / HotSpot | Real Java execution | No Minecraft classes; full-privilege code; heavy |
| GraalVM Native Image | AOT Java | Closed-world reflection/class loading; no Minecraft API |
| GraalVM JVMCI | JIT reuse | Still needs a JVM runtime and classes |
| ASM | Class-file analysis/transformation | Analysis only; no API/behaviour |
| Wasmtime / Component Model | Sandboxed generated code | No Java object model/class loader by itself |
| TeaVM / Doppio / CheerpJ | JVM-in-WASM/browser experiments | Incomplete, heavy, licensing; still no Minecraft classes |

No third-party repositories were cloned just to evaluate them; the conclusion follows from
their documented execution models and from Horizon's missing Java surface. Horizon itself and
`horizon-mixins` were cloned because source inspection was necessary.

## What HFC adds that did not exist

- Multi-jar Fabric metadata and version-range resolution (upstream reads ids only).
- Duplicate id, conflict, break and cycle diagnostics.
- A per-jar content-addressed cache keyed by Horizon/API/pipeline/platform/carry profile.
- Safe ZIP pre-validation before the port pipeline runs.
- Automatic install, marker-based cleanup, state and `horizon-mods.txt` management.
- A launcher pre-launch hook with an explicit "not installed, do nothing" path.
- Reports that separate *carried content* from *runtime compatibility*.
