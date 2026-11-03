# Security model

HFC's inputs are files the player downloaded from the internet. The design assumption is that
**every jar is hostile**. The HFC installer does not execute Java, load classes, run Mixins,
follow entry points, or evaluate archive content as code. Archives are parsed as data with
hard limits; Horizon's own carry pipeline then produces inert content.

The separate experimental `hfc-runtime` **does** load and interpret mod bytecode in the
client process. It is not a sandbox and is not part of the installer's trust boundary; see
[fabric-runtime.md](fabric-runtime.md). Only run jars there that the player already trusts.

## Trust boundaries

| Component | Trust | Why |
|---|---|---|
| Horizon content mod (`.hmod` folder) | inert data | Horizon reads JSON/PNG/NBT/OGG after startup; no code is loaded |
| Horizon native mod (`.hnmod`) | **fully trusted, unsandboxed** | compiled as Rust and run with the player's privileges (Horizon `docs/native-mods.md`) |
| `hfc` / `hfc-bootstrap` | trusted host program | installed by the player, runs in the launcher process tree |
| Fabric jars in `horizon-mods/` | **untrusted** | arbitrary third-party archives |
| Generated mods | trusted output | written by HFC from carry output, marked `.hfc-generated.json` |

HFC adds no new privilege to content mods. It does not give the game filesystem access it did
not have, and it does not turn a content mod into a native mod. The only host capability it
requires is the launcher's existing ability to start a program before the game.

## Assets to protect

1. The player's files outside `horizon-mods/` (documents, saves, credentials, the launcher).
2. The original Fabric jars: read-only in HFC; never modified, renamed or deleted.
3. The player's own mod folders: never removed or overwritten, even on a name collision.
4. Game availability: a malicious or broken jar must not prevent the game from starting.
5. Integrity of the cache and of `horizon-mods.txt`: no stale or partial output may be loaded.

## Threats and mitigations

| Threat | Mitigation | Where |
|---|---|---|
| Zip bomb / huge archive | 512 MiB jar cap checked before opening; 2 GiB total uncompressed cap from central-directory sizes; 65,536 entry cap | `hfc-fabric::archive` |
| Oversized metadata | 8 MiB cap per `fabric.mod.json` / mixin config; read through a limited reader | `archive.rs`, metadata read |
| Path traversal / absolute paths / drive letters | every non-directory entry name validated (`/`, `\`, `:`, NUL, `.`/`..`, empty parts) **before** anything is read; directories skipped | `hfc_core::safe_archive_path` |
| Symlink escape | HFC itself never extracts entries. `discover` uses `DirEntry::file_type` (does not follow symlinks), so a symlinked jar is skipped. The carry pipeline validates entry names again and writes inside its staging directory | `discover`, `horizon-port-pack::carry` |
| Duplicate entries (zip confusion) | raw central-directory walk detects duplicate names before the zip crate collapses them; any duplicate is a malformed archive | `archive::duplicate_entry_name` |
| Nested-jar recursion | one level of declared nested jars, at most 256, nested bytes capped; their own nested declarations are reported but not read | `archive.rs` |
| Malformed JSON / TOML / zip | parse errors become a per-jar `skipped` entry or `JarScan::error`; one bad jar does not abort the run | scanner, bootstrap |
| Malicious metadata strings | JSON-encoded in reports; Markdown table cells escape `|` and strip newlines; no HTML rendering | `report.rs` |
| Half-written cache / mod folder | temp file + fsync + rename for files; staging directory + rename for cache entries and installed mods; old folder restored on failure | `hfc_core::atomic_write`, `hfc-cache`, `install.rs` |
| Stale execution of old code | cache key includes jar hash, Horizon version, compatibility API, pipeline version, platform, carry profile; no key match means reconvert | `hfc-cache` |
| Replacing a user's folder | install refuses to touch a directory without HFC's `.hfc-generated.json`; removal re-checks marker and id | `install.rs` |
| A jar hijacking another mod's namespace | namespace collision blocks the second mod and is reported | `hfc-bootstrap` |
| Failure stopping Play | launcher reports the error and still starts the game with the previous install | `launcher/src/fabric_compat.rs` |

## What HFC deliberately does not do

- It does not decompile, disassemble or execute `.class` files. The carry pipeline reads class
  **structure** (constant-pool/class names) for its feature census, which is static inspection.
- It does not follow URLs, download dependencies, or install Fabric API on the player's behalf.
- It does not run `hfc` from a jar, from the mods folder, or from a path supplied by a mod.
  The launcher uses `HFC_BIN`, then a sibling `hfc`, then `PATH` — never a path inside
  `horizon-mods/`.
- It does not mark a mod runtime-compatible because its metadata parsed. `runtime_ready` stays
  false for every mod with code signals.

## Trusting the reused pipeline

HFC calls the same `horizon_port_pack::check::check` that Horizon's pack importer uses. That
pipeline verifies entry names while carrying (including `..`, empty parts, `\` and `:`), and
only writes inside the output directory it is given. HFC adds the archive-wide limits before
the call because the carry pipeline is per-entry oriented and does not impose a total
uncompressed budget. The cache and install directories are private to HFC and are created by
HFC, not by the jar.

## Residual risks

- **Resource use.** A 512 MiB jar that expands to 2 GiB can still cost time and disk. Caps make
  this bounded, not free. `max_mods` (default 512) bounds one run.
- **Carry-pipeline bugs.** HFC treats the pipeline as trusted code; a bug there is a Horizon
  vulnerability, not something HFC can sandbox. Reports name the pipeline version so a bad
  conversion can be invalidated by bumping it.
- **TOCTOU on the original jar.** HFC hashes the jar, then the pipeline reopens it by path. A
  local attacker who can rewrite the file between the two can make the cache key disagree with
  the converted bytes. This requires write access to the mods folder, which already implies
  control of the game; it is noted rather than defended against.
- **No inter-process lock.** Two game launches at once share the cache and mods folder. Writes
  are atomic and entries are immutable, so the failure mode is duplicated work, not corruption;
  a lock file is planned.
- **Reports are not a security boundary.** They are readable text for the player. HFC never
  executes report content.
