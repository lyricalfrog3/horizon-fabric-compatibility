# Horizon: current state (evidence from source)

Research date: 2026-11-02.

## Exact revisions used

| Repository | Commit | Date | Role |
|---|---|---|---|
| `horizon-rust/horizon` | `26c83240d6d2e3bfbda5314c9f8d1274fba0ec1a` | 2026-11-02 | Engine, content loader, port pipeline, launcher |
| `horizon-rust/horizon-mods` | `b22a9c42a3848ef410f00e381b505b3d12a742aa` | 2026-11-02 | Official mods, reference native/content mods |
| `horizon-rust/horizon-mixins` | `ddd9700b21997c95d95ac6ee5ecafe3830cf622a` | 2026-11-01 | Native mixin tooling (shallow clone) |

Horizon's game version constant is `horizon_core::version::VERSION_ID == "26.3"`
(`workspace/horizon/crates/horizon-core/src/version.rs`). HFC records this version in
cache keys and reports.

The launcher is **inside** the Horizon repository (`launcher/`), not a separate project. It
declares its own Cargo workspace (`launcher/Cargo.toml`), so it is built independently.

## The two mod kinds

From `docs/mods.md`:

| Kind | File | Content | Runs |
|---|---|---|---|
| Content mod | `.hmod` or a folder with `horizon-mod.toml` | Data and assets only: blocks, items, recipes, loot, tags, models, textures, structures | Read once when the game starts |
| Native mod | `.hnmod` | A `horizon-mixins` bundle: Rust that patches the game's source before compilation | In a launcher-built game |

Key facts verified in source:

- Fabric/NeoForge jars do not run as-is. Horizon has no Minecraft Java classes for their code
  to call or patch.
- Content mods are **not code**. The manifest has `capabilities = ["registries"]`, API `0.2`,
  and unknown manifest keys are refused.
- Native mods are **not sandboxed**: "Native mods are native code with the player's rights"
  (`docs/native-mods.md`). They are trusted through a first-install notice, a red build
  warning and registry review.
- Native mods build against a pinned Horizon commit or a stable set of native hook points
  (`NATIVE_HOOKS_API`, only ever additive).

Implication for HFC: there is no existing safe container in which to run Fabric bytecode.
Adding one would be a new engine subsystem with a new security model, not a mod.

## How content mods are found

- The game reads `horizon-mods/` **once at startup**.
- Only **direct children** are loaded: a `.hmod` file or a directory containing
  `horizon-mod.toml`. `horizon-mods::find` is non-recursive.
- `horizon-mods.txt` sits beside `options.txt` (the game/instance folder, i.e. the parent of
  `horizon-mods`) and holds `mod.<id>:true` / `mod.<id>:false`.
- Two enabled content mods may not own the same namespace. The first by file name loads; the
  other is refused with a log message (`docs/mods.md`, `crates/horizon-client-render/src/mod_list.rs`).
- Content mod ids are `[a-z0-9_]{1,64}` (`horizon-mods` manifest rules, also enforced by
  `horizon-port-pack::carry`).

Implication: HFC must install generated mods as **direct child folders** of `horizon-mods`,
with carry-safe ids, and must enable them in the list file. A fabricated id (`fabric-api` with
a dash) cannot be installed; HFC reports it.

## Existing port pipeline: `crates/horizon-port-pack`

This is the reusable asset in the repository. HFC does not duplicate it.

- `jar::read` reads a jar's metadata (`fabric.mod.json`, `mods.toml`/`neoforge.mods.toml`,
  `mcmod.info`) and takes a name census. Fabric dependencies are read as **ids only**;
  version ranges are discarded upstream.
- `check::check(jar, out)` is the single-jar path behind the launcher's mod browser:
  writes `out/check.json` and `out/carry/`; verdicts are `Works`, `Partly`, `No`.
  - It reads `.class` bytes and runs `classes::census` to describe code features (events,
    block entities, entities, Mixins). This is static class-file inspection, not
    decompilation, and never executes the jar.
  - It treats jars for Minecraft 1.20 or older as unsupported ("Horizon ports mods made for
    1.21 and later").
  - It carries only `assets/` and `data/`, upgrades recipes/loot/etc. from 1.21.1 to 26.3, and
    writes `horizon-mod.toml` with `capabilities = ["registries"]`.
  - `carry::carry` rejects ids that are not `[a-z0-9_]{1,64}`.
- `pipeline::loadable/portable/carry/port` implement the larger modpack workflow.
- Reports are JSON files in the work folder; `docs/modpacks.md` describes the measured ATM10
  pipeline (495 jars, tiers, dependency order).

What is already incremental upstream: file-level work folders and resumable stages. There is
no per-jar content-addressed cache keyed by Horizon/API versions; HFC adds that layer.

## Launcher invocation points

- `launcher/src/browser/java.rs` installs a browsed mod by calling check → carry →
  `horizon-mods/<id>` and enabling `mod.<id>:true`. Its `copy_dir`/`set_enabled` helpers are
  the same behaviour HFC needs.
- `launcher/src/main.rs::start_client` starts preparation; the `Msg::Session` arm builds the
  game command, applies the instance/profile (`instances::apply`, `modpacks::apply_profile`),
  then calls `backend::spawn`.
- For a pack/profile, `modpacks::apply_profile` sets the process current directory to the
  profile home and passes mods with `--dev-mod`. For own instances, `instances::apply` sets
  the instance home. For the default instance the current directory is unset and the game
  home is `backend::game_home()`.

HFC's hook is placed after the instance/profile is applied and before `backend::spawn`, using
the command's current directory (falling back to `game_home()`) as the game folder.

## Constraints that shaped the design

1. **A content mod cannot do the job.** It is data read after the game starts; it cannot scan
   sibling jars, spawn processes or write generated mods. A launcher/host step is required.
2. **No JVM exists, and native mods are unsandboxed.** HFC must not introduce a second,
   weaker code-execution path. Content-only conversion adds no executable code.
3. **Code mods are source-level Rust, not portable bytecode.** Any future automatic code
   compatibility must generate native bundles through the real SDK/hook points and must be
   treated as native code by the existing trust model.
4. **Upstream ids and layout are strict.** Generated folders must be direct children and
   carry-safe; HFC must not invent recursive loading.
5. **The pipeline already knows how to carry content.** Reimplementing it would be both
   duplication and a correctness risk.
