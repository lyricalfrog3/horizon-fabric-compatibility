# ADR 0002 — Per-jar content cache and marker-scoped installation

- Status: accepted
- Date: 2026-11-02

## Context

The pipeline must not reconvert every jar on every launch. It must also never reuse a
conversion made for a different Horizon/API/pipeline/platform, and an interrupted run must not
leave a half-written generated mod that the game later loads.

Generated mods live in the player's mods folder beside their own mods. Deleting the wrong
directory is unacceptable.

## Decision

### Cache

One cache entry = one jar converted for one set of inputs. The directory key includes:

- JAR SHA-256 (content identity);
- Horizon version (`horizon_core::version::VERSION_ID`);
- HFC compatibility API (`COMPAT_API`);
- conversion pipeline version (`PIPELINE_VERSION`, bumped when formats/rules change);
- target platform (`OS-ARCH`);
- a carry profile string (target Minecraft version and options that affect output).

`entry.json` repeats the full key. A lookup requires the directory key, a valid `entry.json`
whose fields all match, and an existing `check.json`. Anything else is a miss. This means an
old entry cannot be reused after an invalidating change; it simply stops matching.

The entry is built in a private staging directory and committed with a directory rename. If a
concurrent run committed first, that entry is used. `sweep_staging` removes staging
directories abandoned for more than 24 hours.

Dependency versions do **not** enter the conversion key in v1. Conversion is per-jar and
pack-independent: `check::check` carries a jar's own assets/data and does not consult other
mods. Dependency/namespace decisions are recomputed every run at install time. When code
generation is added, dependency and API versions will enter a *code* cache key.

### Installation

- Target is `horizon-mods/<carry id>/` (a direct child, as Horizon requires).
- The folder is copied to a hidden sibling, the `.hfc-generated.json` marker is written, then
  the folder is swapped into place. A failed copy leaves the old folder intact.
- Replacement is allowed only when the existing folder carries a marker with the same mod id.
  A user folder with the same name is left untouched and reported.
- Cleanup enumerates only folders with a valid marker and removes those whose source jar is
  gone or whose hash no longer matches.
- `state.json` records what was installed but is not authoritative: markers are. A corrupt
  state file is noted and treated as empty; cleanup still works from markers, and stale
  `horizon-mods.txt` entries from the old state are disabled when known.

## Consequences

- Repeat launches touch no conversion work and usually no files.
- A changed jar costs one conversion; other jars are unaffected.
- Horizon/API/pipeline/platform upgrades naturally invalidate; no migration code.
- The cache grows by one entry per jar version. A future `hfc cache prune` can remove
  unreferenced entries; the current CLI reports size and correctness is unaffected.
- The launcher must run HFC with a writable mods folder, which it already has for its own
  mod browser installs.
