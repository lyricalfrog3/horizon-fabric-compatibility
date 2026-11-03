# Runtime and launch integration

HFC has no runtime of its own. It runs once, before the game process exists, turns Fabric
content into ordinary Horizon folder mods, and exits. The game then loads those folders exactly
as it loads any other content mod.

## Where the hook lives

Horizon's launcher is the only host capable of doing this safely: a content mod is data read
after startup, and a native mod is unsandboxed code compiled by the launcher. The hook is one
module, `launcher/src/fabric_compat.rs`, called from the `Msg::Session` arm of the launcher
update loop, after the instance/profile command line is final and before `backend::spawn`.

```text
player presses Play
        │
        ▼
launcher downloads/checks assets, renews session      (existing behaviour)
        │
        ▼
instances::apply / modpacks::apply_profile            (existing: cwd, HORIZON_HOME, --dev-mod)
        │
        ▼
fabric_compat::bootstrap(home)                        (new)
        │  no horizon-mods/HorizonFabricCompat/hfc.toml → return, zero work
        │  marker present → run `hfc bootstrap --mods <home>/horizon-mods`
        ▼
launcher spawns horizon-client                        (existing)
        │
        ▼
Horizon reads horizon-mods/ once at startup
        │
        ▼
menu / world with generated content mods
```

- `home` is the command's working directory when the launcher set one (own instance or pack),
  otherwise `backend::game_home()`. This matches where the game will look for `horizon-mods`.
  Modpack profiles that launch their carried mods through `--dev-mod` are not HFC's target:
  their mods folder is the pack's staging area, not a player-editable `horizon-mods/`.
- The subprocess inherits the launcher's environment and runs with the player's privileges.
  That is the same trust level as the launcher itself; no mod gains privileges from it.
- Binary discovery is `HFC_BIN` (if it points to a file), then `hfc`/`hfc.exe` beside the
  launcher executable, then `PATH`. HFC is never loaded from `horizon-mods/`.
- The call is synchronous. On a warm cache it is milliseconds; a cold run pays the carry
  pipeline once per new jar. This is the cost of doing conversion before the game reads mods.

## Failure behaviour

| Situation | Result |
|---|---|
| no marker | nothing runs; no log line, no cost |
| marker, no `hfc` binary | one error line in the preparation log; game still starts |
| `hfc` exits non-zero | stderr/stdout shown as the error; game still starts with the previous install |
| one jar fails | that mod is `failed`/`skipped` in the report; other mods continue |
| report/`horizon-mods.txt` cannot be written | `hfc` exits non-zero, launcher reports it, game still starts |

A failed run never leaves a half-written generated folder: installation uses a staging
directory, writes the marker, and renames into place, restoring the previous folder if the
rename fails. A failed run also never deletes a folder it did not create.

## What the game sees

Only two things change in the game folder:

1. `horizon-mods/<id>/` — generated folder content mods, each with `horizon-mod.toml` from the
   carry pipeline and a hidden `.hfc-generated.json` marker.
2. `horizon-mods.txt` beside `options.txt` — `mod.<id>:true` for installed mods and
   `mod.<id>:false` for generated mods that were removed. Unrelated lines are preserved.

The original `.jar` files remain in `horizon-mods/`. Horizon's content loader ignores files
that are not folders with a `horizon-mod.toml`, so the jars are inert to the game; only HFC
reads them. The compatibility folder `HorizonFabricCompat/` has no `horizon-mod.toml` and is
ignored too.

Per-launch state:

- `state.json` records what HFC installed last time, for cleanup and `hfc status`. It is not
  authoritative: every run rebuilds the desired set from the jars it sees.
- `report.json` / `report.md` are overwritten each run.
- cache entries are immutable.

## Idempotence

Re-running with unchanged jars:

- cache lookup hits; no conversion;
- `ensure_installed` sees a matching marker and returns `reused` without copying;
- `horizon-mods.txt` lines are rewritten to the same values;
- report rows show `reused` and `hit`.

Changing one jar changes only that jar's content: its SHA-256 selects another cache entry and
another install pass. Other mods are untouched (unless the change alters dependency resolution
or namespace ownership, which is recomputed and reported).

Removing a jar removes its generated folder **only if it carries HFC's marker** and is no
longer desired. The folder is also turned off in `horizon-mods.txt`, so a leftover copy cannot
stay enabled.

## Turning HFC off

- `compat.enabled = false` in `hfc.toml`: every jar is analyzed and reported, and HFC removes
  its generated folders and disables them. The compatibility folder and cache stay.
- `carry.enabled = false`: same analysis-only behaviour and the same cleanup of HFC-generated
  folders (rows show `scanned`). Re-enabling restores the mods from the cache without
  reconverting.
- Deleting `HorizonFabricCompat/hfc.toml`: the launcher stops running HFC. Already generated
  folders remain on disk and enabled; use `hfc status` / remove the marked folders, or set
  `compat.enabled = false` once before deleting the marker.
- Deleting the whole `HorizonFabricCompat/` folder: HFC is gone together with its cache and
  state; generated mods remain and must be removed by hand.

## Manual invocation

The launcher path is a convenience; everything works from a terminal:

```sh
hfc install   --mods <game>/horizon-mods   # create HorizonFabricCompat + hfc.toml
hfc bootstrap --mods <game>/horizon-mods   # one full run (what the launcher calls)
hfc scan      --mods <game>/horizon-mods   # analyze, install nothing
hfc status    --mods <game>/horizon-mods   # last report + recorded state
```

`bootstrap --dry-run` analyzes and reports without writing; `--force` ignores cache hits.
