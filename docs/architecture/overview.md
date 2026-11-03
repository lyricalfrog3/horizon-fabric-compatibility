# Architecture overview

## Shape

HFC is a **host-side pre-launch pipeline**. It is not a content mod and it is not loaded by
the game. This is forced by Horizon's design: a content mod is inert data read after startup,
and native mods are unsandboxed Rust compiled by the launcher. Nothing inside the engine can
see sibling jars before the content loader runs.

```text
                    ┌─────────────────────────────────────────────┐
                    │  launcher (Horizon, one small hook)         │
                    │  marker present? → run `hfc bootstrap`      │
                    └──────────────────────┬──────────────────────┘
                                           │
              ┌────────────────────────────▼────────────────────────────┐
              │ hfc-bootstrap                                            │
              │                                                          │
  horizon-mods│  discover *.jar ──► hfc-fabric::scan_path (safe zip)     │
   (siblings) │       │                    │                            │
              │       │                    ▼                            │
              │       │        fabric.mod.json + version ranges          │
              │       │                    │                            │
              │       │                    ▼                            │
              │       │        dependency graph / duplicates / cycles    │
              │       │                    │                            │
              │       │         ┌──────────┴───────────┐                │
              │       │         ▼                      ▼                │
              │       │   blocked/report        hfc-cache lookup        │
              │       │                               │                 │
              │       │                     miss ─────┤                 │
              │       │                               ▼                 │
              │       │              horizon-port-pack::check          │
              │       │              (reused content pipeline)         │
              │       │                               │                 │
              │       │                     hit ──────┤                 │
              │       │                               ▼                 │
              │       │                     stage → atomic commit       │
              │       │                               │                 │
              │       ▼                               ▼                 │
              │  install horizon-mods/<id>/ + marker + horizon-mods.txt│
              │  write state.json + report.json + report.md            │
              └────────────────────────────────────────────────────────┘
                                           │
                                           ▼
                         Horizon content loader (data/assets only)
```

## Crates

| Crate | Responsibility | Never does |
|---|---|---|
| `hfc-core` | limits, errors, SHA-256, atomic writes, constants | mod logic |
| `hfc-fabric::version` | Fabric version/range semantics | guess unknown comparisons |
| `hfc-fabric::metadata` | parse/validate `fabric.mod.json` | load classes |
| `hfc-fabric::archive` | safe zip census, nested jars, mixin targets | extract entries, run code |
| `hfc-fabric::graph` | providers, issues, deterministic order | decide install policy |
| `hfc-cache` | keyed staging/commit/lookup | call the port pipeline |
| `hfc-bootstrap` | orchestration, conversion, install, cleanup, report | execute Java |
| `hfc-runtime` | experimental in-client Fabric bytecode interpreter and event bridge | claim general Fabric compatibility or replace a JVM |
| `hfc-cli` | `install`/`scan`/`bootstrap`/`status` | hold policy |
| launcher `fabric_compat` | marker detection, binary discovery, one subprocess | parse mods |

## Run sequence

1. `Bootstrap::open(mods_dir)` returns `None` when `HorizonFabricCompat/hfc.toml` is absent.
   The launcher does nothing in that case.
2. Load `hfc.toml`; load `state.json` tolerantly (a corrupt file is a note, not a crash).
3. `discover` finds direct-child `.jar` files, skipping hidden files, `.disabled` and
   directories. The configured `max_mods` caps one run.
4. `scan_path` validates the archive and reads metadata/census. A malformed jar is recorded in
   `skipped`; one bad file does not stop the run.
5. `resolve` builds providers (top-level and nested), built-in environment versions, issues and
   a dependency-first order.
6. For each installable top-level mod in order:
   - reject ids outside `[a-z0-9_]{1,64}` (Horizon cannot load them);
   - look up the cache; on miss run `horizon_port_pack::check::check` in a staging dir and
     commit;
   - read the carry verdict. `No` → report `not portable`, install nothing;
   - reject a namespace already owned by another mod;
   - install `carry/` as `horizon-mods/<id>/` with a marker.
7. Remove marker-bearing folders whose source jar disappeared/changed.
8. Save `state.json`, update `horizon-mods.txt`, write both reports.
9. The launcher starts the game. Horizon reads the generated content once.

A failure in one mod is isolated: it is reported, other mods continue. A failure writing the
report or the mods list is fatal for the run (the launcher displays it), but the game is still
started by the launcher.

## Data model

`ModNode` keeps two independent answers:

- `installable`: the jar passed structural checks, has a carry-safe id and no conflict/cycle
  that prevents installation;
- `runtime_ready`: Fabric's dependency contract is satisfied and the mod does not declare code
  HFC cannot run.

A typical content mod with an entry point is `installable = true`, `runtime_ready = false`,
and its content is carried. A library with only entry points is `not portable`. A duplicate
id or a conflict blocks installation. This is surfaced verbatim in `report.md`.

## Generated layout

```text
horizon-mods/
  HorizonFabricCompat/
    hfc.toml               user-visible switch
    state.json             last installed set (not authoritative)
    report.json            machine-readable run result
    report.md              human-readable run result
    cache/<ab>/<sha256>/<key>/
      entry.json           full cache key + timestamp
      check.json           horizon-port-pack verdict
      carry/               folder content mod
  ruby/                    generated folder mod
    horizon-mod.toml
    assets/ data/
    .hfc-generated.json    marker: id, source file, source SHA-256, HFC/API version
  ruby.jar                 original, untouched
```

The game reads only `horizon-mods/ruby/`. The compatibility folder is not a mod: it has no
`horizon-mod.toml`, so the loader ignores it.

## Launcher hook

`launcher/src/fabric_compat.rs`:

- `installed(home)` checks for `horizon-mods/HorizonFabricCompat/hfc.toml`.
- `bootstrap(home)` finds `HFC_BIN`, `hfc` beside the launcher, or `hfc` on `PATH`, then runs
  `hfc bootstrap --mods <home>/horizon-mods` with the instance home as working directory.
- The hook is called after `instances::apply`/`modpacks::apply_profile` and before
  `backend::spawn`, so the game folder and profile mods are already correct.
- Success appends one line to the preparation log; failure is shown but does not prevent the
  launch (the game starts with the previously installed mods).

This is the smallest host capability that satisfies the requirement. It does not grant mods
any new privilege: HFC reads untrusted archives as data and writes only its own generated
content.

## See also

- [runtime.md](runtime.md) — launch sequence, failure behaviour, uninstall switches.
- [compatibility.md](compatibility.md) — metadata, version ranges, classification.
- [caching.md](caching.md) — cache key, atomic commit, invalidation.
- [security.md](security.md) — trust boundaries and hostile-archive handling.
- [shaders.md](shaders.md) — the separate shader-pack track.
