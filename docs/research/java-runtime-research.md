# Java execution and compilation options

The question is not "can Java run on a computer" but "can *this mod* run against Horizon".
A Fabric mod is compiled against Minecraft Java classes, calls them, and often rewrites them
with Mixin. Any strategy must supply those classes and their behaviour, not just a JVM.

## A — Embedded JVM (OpenJDK/HotSpot)

What it gives: real class loading, reflection, `invokedynamic`, exceptions, JNI, and Mixin's
bytecode transformation. Mature and fast.

What it costs:

- A JVM is not a sandbox. Mod code would run with the user's full rights unless HFC invents a
  process boundary, a restricted class loader, a security manager (deprecated/removed in
  modern JDKs) or an OS sandbox. That is a new security model, contrary to Horizon's policy.
- Every Minecraft class the mod touches must exist with compatible binary names, fields,
  method descriptors and semantics. Horizon is Rust and has none of them.
- Bridging Rust world state into Java objects at 60+ FPS is a large, permanent FFI surface.
- Mixin needs the original Minecraft class bytes. Horizon does not have them and cannot ship
  Mojang code.
- Distribution: bundling a JDK adds ~200 MB and a second runtime to support.

Verdict: technically capable, architecturally wrong for v1. It solves execution but not
compatibility, and it weakens the security model.

## B — Java bytecode compilation (AOT/JIT, native or WASM)

- **GraalVM Native Image** is closed-world: dynamic class loading, reflection, JNI and agents
  need explicit configuration or substitution. Fabric/Mixin mods are dynamic by nature.
  Native Image also does not provide Minecraft's classes or behaviour.
- **JVMCI/JIT reuse** compiles methods but still requires a JVM object model and runtime.
- **WASM** has no Java object model or class loader. Compiling JVM bytecode to WASM requires a
  full runtime (TeaVM, Doppio-style interpreters, CheerpJ) and still needs the Minecraft API.
  WASM's sandbox is attractive, but the compatibility gap remains huge.
- **ASM/bytecode libraries** can analyze and transform classes; that is useful for feature
  detection, not for recreating missing classes.

Verdict: compilation changes the execution format, not the missing API or Mixin semantics.
Useful later for analysis and for sandboxed generated code, not a v1 solution.

## C — Automatic native porting

Translate mod behaviour into Rust/Horizon mods, either by agents or by a compiler. This is the
only approach that produces something Horizon can actually run safely and quickly, and it is
the direction Horizon's own modpack pipeline already takes (`docs/modpacks.md`: carry-over for
data, native ports for logic, behavioural oracles for validation).

Costs: no general Java-to-Rust compiler exists for this domain; Mixin patches are arbitrary
code; correctness must be measured per mod; automatic generation without tests would violate
the project's "never claim compatibility without functional testing" rule.

Verdict: the correct long-term direction, too large for the first slice.

## D — Hybrid

Combine:

1. **Content extraction** through Horizon's existing pipeline (safe, mechanical, immediately
   useful).
2. **Native reimplementations of shared Fabric/Minecraft APIs** as Horizon SDK capabilities
   and hook points, reused by many mods.
3. **Generated native mods** for code behaviour, through the real `horizon-mixins` toolchain
   and native hook points.
4. **Java only as a development-time oracle**, never as the shipped runtime, where policy
   allows it.

This preserves Horizon's security model (shipped code is content or reviewed native code),
reuses the existing port pipeline, and makes progress measurable mod by mod.

## Comparison

| Criterion | A JVM | B Compile | C Native port | D Hybrid |
|---|---|---|---|---|
| Content/assets mods | needs JVM + classes | needs JVM + classes | carry | **carry now** |
| Minecraft Java API gap | must reimplement | must reimplement | reimplement per capability | reimplement progressively |
| Mixins | works if classes exist | very hard | rewrite as native hooks | native hooks |
| Runtime overhead | JVM + FFI | compiled but runtime still needed | native | native |
| Security | full user rights unless sandboxed | sandbox possible for WASM | native trust model | **native trust model** |
| Dynamic loading | yes | limited/closed-world | no | launcher-generated, restart |
| Effort to first value | very high | very high | high | **low for content** |
| Maintainability vs Horizon updates | fragile FFI | fragile | follows SDK versions | follows SDK versions |

## Recommendation

Adopt **D**, with v1 restricted to content. Do not embed a JVM. Do not claim Java execution.
Build the metadata/dependency/cache/install layer now, then add reusable API capabilities and
generated native mods in later phases. The ADR records this decision.
