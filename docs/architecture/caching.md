# Caching

One cache entry is **one jar converted once**. The entry directory is named from every input
that can change the conversion, so a stale entry is never found instead of being reused. The
cache lives inside the compatibility installation:

```text
horizon-mods/HorizonFabricCompat/cache/
  <first two hex chars of the jar SHA-256>/
    <full jar SHA-256>/
      <key id>/
        entry.json        full key + creation time (written last)
        check.json        horizon-port-pack verdict / ports / skipped features
        carry/            the generated folder content mod
```

## The key

`KeyContext` (`crates/hfc-cache/src/lib.rs`) has six inputs:

| Input | Value in v1 | Why |
|---|---|---|
| jar SHA-256 | content hash of the original jar | any byte change reconverts; mod version is covered by content |
| Horizon version | `horizon_core::version::VERSION_ID` (`26.3`) | carry output targets this engine |
| compatibility API | `hfc_core::COMPAT_API` (`0.1`) | bump when generated layout/metadata semantics change |
| pipeline version | `hfc_core::PIPELINE_VERSION` (`1`) | bump when scanner, carry or install formats change |
| platform | `std::env::consts::OS` + `ARCH` | output can differ by platform |
| carry profile | `mc=<compat.minecraft>` plus future carry options | a different target Minecraft version is a different conversion |

The key id is the first 32 hex characters of SHA-256 over the five non-jar fields (separated by
`U+001F`). `entry.json` records the **full** key, and `lookup` re-checks every field; the short
id is only a directory name, so even a truncated-hash collision cannot produce a wrong hit.

### Dependencies are not in the key

The requirement lists dependency versions. In v1 conversion is **per jar**: the carry pipeline
reads one jar and never looks at its neighbours, so a dependency version cannot change that
jar's output. Dependency resolution and namespace ownership are recomputed on every run from
the jars actually present, and installation is keyed by source SHA-256. Putting dependency
versions into the cache key would only force needless reconversion when an unrelated
dependency updates.

This changes the day code ports are generated: a generated native mod embeds calls into the
compatibility API and may depend on the exact API surface of its dependencies. At that point
the key gains a dependency fingerprint (id + version of every resolved dependency) and the
generated-code compiler version. The field is `carry_profile` today, so the format can grow
without invalidating existing entries.

## Lifecycle

1. **Lookup.** `Cache::lookup(jar_sha256, context)` reads `entry.json`, verifies every key
   field, the entry format, and the presence of `check.json`. A missing, malformed, tampered or
   incomplete entry is a miss.
2. **Staging.** On a miss (or `--force`), `Cache::staging()` creates
   `cache/.hfc-staging-<pid>-<nanos>/`. Abandoned staging directories older than 24 hours are
   swept first; a younger one may belong to a concurrent run and is left alone.
3. **Convert.** `horizon_port_pack::check::check(jar, staging)` writes `check.json` and
   `carry/` there. A conversion error deletes the staging directory and is reported for that
   mod; the cache is unchanged.
4. **Commit.** `entry.json` is written atomically **inside staging**, then staging is renamed
   to the key directory. Rename is atomic on the same filesystem. If another process committed
   the same key first, the loser discards its staging and returns the winner's entry. If an
   interrupted earlier commit left a directory without a valid entry, it is removed and
   replaced. The commit verifies the new entry with `lookup` before returning.
5. **Install.** The carry output is copied into `horizon-mods/<id>/` with its own staging
   directory, marker and atomic swap (`install.rs`). The cache entry stays immutable.

An interrupted process can therefore leave only:

- an abandoned `.hfc-staging-*` directory (swept after 24 hours);
- an uncommitted entry directory (replaced on the next commit);
- the old installed folder (the install swap restores it on failure).

It can never leave a half-written `entry.json` that verifies.

## Invalidation

| Change | Effect |
|---|---|
| jar edited/replaced | new SHA-256 → new directory, old entry unreachable |
| HFC release changes output | bump `PIPELINE_VERSION` → all keys no longer match |
| compatibility contract changes | bump `COMPAT_API` → all keys no longer match |
| Horizon update | new `VERSION_ID` → reconvert on next launch |
| OS/architecture change | new platform → reconvert |
| target Minecraft version changes in `hfc.toml` | new carry profile → reconvert |
| cache disabled (`compat.cache = false`) | reconvert every run, still committed (reported as `disabled`) |
| `--force` | skip lookup once; commit replaces as usual |

Old entries are not deleted automatically. They are inert, content-addressed and only cost
disk; `hfc status` reports entry count and total size. A future `hfc cache prune` can drop
entries not referenced by the current state without risk, because folders carry their own
markers.

## Concurrency

Entries are immutable after commit; two processes converting the same jar race only to rename
the same target, and the loser keeps the winner's verified entry. Installed folders use a
per-process staging name and an atomic swap, so the last writer wins cleanly. There is no lock
file yet: two simultaneous launches may duplicate work and briefly replace the same generated
folder, but cannot produce a partially written one. See
[security.md](security.md#residual-risks).

## Not cached (yet)

- Parsed `fabric.mod.json` and the dependency graph. They are cheap (one pass over the central
  directory plus one small JSON file) and depend on the whole folder, not one jar. If scanning
  ever becomes measurable, a per-jar metadata cache keyed by SHA-256 can sit beside this one.
- Generated code ports, compiled native mods, WASM, shader binaries and GPU pipelines. Those
  subsystems do not exist in v1; each will need its own key and its own invalidation rules.
