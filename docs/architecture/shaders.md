# Shader compatibility (separate subsystem)

Shader packs are **not** part of Fabric jar conversion and are not in the v1 vertical slice.
They are documented separately because the plan requires a distinct architecture and roadmap;
conflating them with mod content conversion would produce false compatibility claims.

## What Horizon has

Horizon ships its own shader API (`crates/horizon-shader-api`, API 0.1, experimental) and its
own pack format:

```text
shaderpacks/<pack>/horizon-shaders.toml
shaderpacks/<pack>/<stage>.wgsl
```

Stages are `sky`, `material`, `weather` and up to eight ordered `post` passes. Packs read a
fixed `frame` block, optional declared textures, tags and material ids. They declare no
bindings and no entry points; Horizon prepends a prelude and calls the stage functions. Packs
are checked (`horizon-shader check`), can be shipped as `.hshader`, hot-reload twice a second,
and run under Options → Horizon Settings → Shader Packs.

Crucially, Horizon's renderer still owns culling, batching and meshing. A pack changes how
things look, not how the world is drawn. There is no general-purpose render-graph API exposed
to packs.

## What Iris/OptiFine packs assume

Iris/OptiFine packs are GLSL programs organised around a pipeline Horizon does not currently
expose:

| Iris/OptiFine concept | Horizon equivalent today |
|---|---|
| `gbuffers_*` programs writing multiple render targets | none (material stage returns one lit colour) |
| `shadow`, `shadowcomp`, `prepare`, `deferred`, `composite*` passes | post passes only, no custom render targets or depth prepass control |
| custom framebuffers and `colortexN` | declared textures are read-only inputs; no writable targets |
| `uniform` blocks and per-frame uniforms | fixed `frame` struct |
| `#include`, macros, `#ifdef` permutation keys | `shared` files; no preprocessor |
| pack options and profiles | Horizon options and pages (different schema) |
| blend/alpha/depth state, draw buffers | fixed by Horizon's passes |

A GLSL→WGSL source translation can cover arithmetic, texture sampling and simple post
effects. It cannot by itself create missing renderer features: multiple render targets, shadow
maps, deferred lighting, custom depth or blend state. **Translation is not compatibility.**

## Boundaries

- No Iris, OptiFine or Sodium code is embedded or executed. That would import a Java mod
  runtime and still not match Horizon's renderer.
- No shader pack is treated as supported because its GLSL parsed. Support requires the pack to
  run and produce the intended image through Horizon's own pipeline.
- Shader conversion is not part of `hfc bootstrap`; `horizon-mods/` is a mod folder, while
  packs live in `shaderpacks/` beside `options.txt`. A future HFC command may bridge them, but
  it is a distinct feature with its own cache and report.

## Candidate architecture

```text
shaderpacks/<pack>/ (Iris/OptiFine)
        │
        ▼
static pack inventory (no execution)
  shaders/*.glsl|vsh|fsh, shaders.properties, lang, textures
        │
        ▼
GLSL preprocessor + parser (defines, includes, uniforms, samplers)
        │
        ▼
feature classification
  supported stage / needs renderer feature / unsupported
        │
        ├── translatable subset ──► WGSL for Horizon stages ──► horizon-shader check
        │                                                        │
        └── needs features ──► explicit report: not supported    ▼
                                                     shaderpacks/<pack>-hfc/
```

The translator, if built, operates on a parsed AST, validates with Horizon's checker, and
caches by pack SHA-256 + Horizon shader API + translator version + platform. Generated packs
carry a marker analogous to `.hfc-generated.json`. Original packs are never modified.

## Roadmap (shader-specific)

1. **Inventory and report.** Parse `shaders.properties`, list programs, uniforms, samplers,
   render targets and options. Classify each program. No translation yet. This alone gives an
   honest "will not run" answer.
2. **Renderer feature gap list.** Decide which of shadows, deferred, multiple targets and
   custom blending Horizon can expose without compromising its renderer. Anything not exposed
   is permanently unsupported for packs that need it.
3. **Simple post-pass translation.** Start with packs that only use composite-style full-screen
   passes, one colour input and no shadows. Validate output against reference screenshots on
   fixed scenes.
4. **Materials and sky.** Map a restricted `gbuffers_*` subset onto Horizon's material/sky
   stages once the feature work exists.
5. **Options and packs UI.** Translate pack options into Horizon options; keep profile
   semantics where they map cleanly.
6. **Behavioral comparison.** Fixed camera/scene/time golden images, per stage, on more than
   one GPU class. No pack is marked compatible from compilation or a single screenshot.

Each step is gated on the previous one; none is part of the current release.
