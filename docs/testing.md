# Testing strategy

The test suite is deliberately layered: fast unit tests for hostile-input parsing, integration
tests that build owned jars in a temporary folder, and host integration tests for the launcher
hook. No third-party mod jar is downloaded or committed for the HFC conversion tests.

The separate experimental runtime (`crates/hfc-runtime`) is different: when the pinned jars
are present in `test-mods/` and a Minecraft installation is available, its tests execute
third-party bytecode on the interpreter. Those jars are local test fixtures only and are not
in the repository. See [fabric-runtime.md](architecture/fabric-runtime.md).

## Levels

| Level | What it proves | Where | Status |
|---|---|---|---|
| Unit | version ranges, metadata, ZIP safety, graph, cache, atomic writes | `crates/*/src` | implemented |
| Integration | discovery → scan → resolve → carry → install → cache reuse → cleanup | `crates/hfc-bootstrap/tests/end_to_end.rs` | implemented |
| Host | marker detection, subprocess arguments, failure reporting | `workspace/horizon/launcher/src/fabric_compat.rs` | implemented |
| Horizon loader | generated folder is a real content mod and its block/files load | `horizon-mods::Package::read` + `Content::read` in E2E | implemented |
| Horizon game launch | the mod appears in a running world | Horizon client | **not yet automated** |
| Fabric runtime | entrypoint execution, RCH, tooltip dispatch, real JDK/Minecraft bytecode | `crates/hfc-runtime/tests` | implemented; needs local jars/install |
| Behavioral parity | carried content matches the original mod's behaviour | dev-only oracle | **not implemented** |

Compatibility tests may use a pinned mod to reproduce a failure, but the code under test must
be the general path: the opcode, Fabric API event, Minecraft builtin, config/resource format
or host boundary that the mod uses. A test that passes only because the runtime checks that
mod's id or class name is not coverage and must not be added.

## Current inventory

`cargo test --workspace` (HFC crates plus the path dependencies they compile):

| Suite | Tests |
|---|---:|
| `hfc-core` (paths, atomic writes, limits) | 3 |
| `hfc-fabric` (version, metadata, archive, graph) | 18 |
| `hfc-cache` (keying, commit, recovery, prune, stats) | 6 |
| `hfc-bootstrap` unit (config, state, report, install, ids, carry verification) | 12 |
| `hfc-bootstrap` end to end | 5 |
| `hfc-runtime` (bridge, RCH, real classpath, JImage, diagnostics) | 12 |
| **HFC total** | **56** |
| Launcher `fabric_compat` (`cargo test --lib fabric_compat`) | 3 |

`cargo test --workspace` also runs the tests of the path-dependency crates under
`workspace/horizon`; those are Horizon's own suites, not HFC tests.

The end-to-end tests cover:

1. **Happy path.** A synthetic jar with `fabric.mod.json`, blockstate, block/item models,
   texture and tag is installed as `horizon-mods/ruby/` with a marker, enabled in
   `horizon-mods.txt`, reported; the original jar is byte-identical. Horizon's own
   `horizon-mods` loader reads the folder, finds the registries capability and the declared
   block, and loads its assets.
2. **Warm cache.** A second run reports `reused` + `hit` and does not reconvert.
3. **Invalidation.** Changing the jar produces `installed` + `miss`, replaces the folder, and
   leaves no stale sibling.
4. **Mixed folder.** A content mod installs, a code-only library is `not portable`, a
   non-zip is skipped, and the run still succeeds.
5. **Multiple mods / duplicate ids.** Two ids install together; a third jar with a duplicate id
   is reported as skipped, not silently chosen by file order.
6. **Dry run.** Reports are written; no mod folder and no `horizon-mods.txt` are created.
7. **Cache prune.** Removing jars from `horizon-mods` and pruning safely sweeps unreferenced cache
   shards, cleans up empty directories, and reclaims disk space.

Unit tests include hostile fixtures built in memory: traversal paths, duplicate entries,
oversized metadata, nested depth, malformed JSON, duplicate ids, missing/mismatched
dependencies, conflict symmetry, cycles, unknown version comparisons, interrupted cache
entries and marker-less folders.

## Running

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo build --release -p hfc-cli

# The Fabric runtime alone, including its bridge and real-classpath tests.
cargo test -p hfc-runtime
cargo fmt --check -p hfc-runtime

# The launcher is a separate workspace.
cd workspace/horizon/launcher
cargo test --lib fabric_compat
cargo check --bin horizon-launcher
```

`hfc` itself can be smoke-tested without a game:

```sh
mkdir -p /tmp/hfc-demo/horizon-mods
target/release/hfc install --mods /tmp/hfc-demo/horizon-mods
# put a jar in /tmp/hfc-demo/horizon-mods
target/release/hfc scan      --mods /tmp/hfc-demo/horizon-mods
target/release/hfc bootstrap --mods /tmp/hfc-demo/horizon-mods
target/release/hfc status    --mods /tmp/hfc-demo/horizon-mods
```

`scan --strict` and `bootstrap --strict` exit 2 when findings include an error, so CI can fail
on a regression in analysis without parsing English.

## Fixture rules

- Fixtures are **owned synthetic jars** written by the test into a temp directory. They are not
  copies of third-party mods and contain no third-party code.
- A fixture should contain the minimum metadata and one content feature per row it is meant to
  prove. Keep them small; the scanner limits are tested separately.
- Never add a downloaded jar to the repository. If a real mod is needed for a bug report, ask
  the reporter for its SHA-256 and reproduce with a synthetic archive first.
- The exception is local runtime testing: pinned Fabric jars may live in `test-mods/` on a
  developer machine, which `.gitignore` excludes. `test-mods/README.md` lists the expected
  files. Runtime tests that need them must skip or fail clearly, never substitute fabricated
  classes for the real classpath.
- Tests must not depend on the network, a game install or a particular OS. Unix-only tests are
  marked `#[cfg(unix)]` (the launcher script tests).

## What is not tested (and why it matters)

- **A real Horizon launch.** The hook compiles and its subprocess contract is tested, and the
  generated folder is read by Horizon's own `horizon-mods` loader (manifest, capabilities,
  block declarations, asset paths). No test starts `horizon-client` and renders the world. That
  remains the largest gap before claiming the feature works for players.
- **A real Fabric mod.** Synthetic jars exercise HFC's plumbing; they do not prove that a
  popular mod's specific content ports correctly. That is the carry pipeline's test surface
  plus manual compatibility runs.
- **Behavioral parity.** No block behaviour, inventory transition, recipe result, entity or
  worldgen output is compared against the Java mod. HFC therefore does not claim it.
- **Windows/macOS.** Path and rename code uses `std`, but no CI run on those platforms is
  recorded.
- **Performance.** Cold/warm timings are not benchmarked. The design predicts a one-time cost
  per new jar and near-zero warm cost; that prediction should be measured on a real pack.
- **Concurrency.** Atomicity is reasoned and unit-tested at the entry level; no stress test
  runs multiple bootstraps against one folder.

## Gaps to close next

1. A committed, licensed real-mod fixture set (or a script that fetches pinned jars by hash in
   CI with the user's consent); generated folders are already validated by Horizon's loader.
2. A launch smoke test: install a synthetic content mod, start a headless/dedicated Horizon
   process if one exists, assert the mod's block is registered.
3. Performance harness: N jars, cold then warm, report wall time and cache size.
4. Concurrency stress: two bootstraps, same cache, assert a single valid entry and a single
   installed folder.
5. Property tests for version-range parsing and archive-path validation.
