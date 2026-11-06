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
| `hfc-runtime` (bridge, RCH, real classpath, JImage, diagnostics, mixin menu widgets, Mod Menu screen, Legendary tooltip) | 20 |
| **HFC total** | **64** |
| Launcher `fabric_compat` (`cargo test --lib fabric_compat`) | 3 |

`cargo test --workspace` also runs the tests of the path-dependency crates under
`workspace/horizon`; those are Horizon's own suites, not HFC tests.

The runtime suite also opens Mod Menu's own `ModsScreen` through its registered Fabric key
mapping, replays its `GuiGraphicsExtractor` draws, checks the rendered labels and a registered
mod icon's PNG bytes, and closes the screen with Escape. Companion tests click the search box,
type a query, verify the list filters down and the character is drawn, clear it with Backspace,
click Done, and scroll an overflowing list with both the wheel and a scrollbar drag. Two
further tests run the widget-adding mixin callbacks for the title and pause menus, assert each
produces a `Mods` widget, that the widget renders, and that clicking it installs `ModsScreen`.
Those tests skip when the pinned jars or a Minecraft install are absent.

`tests/legendary_tooltips.rs` loads Fabric API, Iceberg, Prism and Legendary Tooltips, lets
the mod write and load its default config, then drives the tooltip calls the client makes.
It asserts that an epic item returns the frame's corner blits sampling
`legendarytooltips:textures/gui/tooltip_borders.png`, that GATHER added the item-model and
padding components, that the title component was padded by Legendary's HEAD injector, that
the name separator and `GuiDraw::Item` model request were recorded, and that a common item
selects no frame but still returns non-transparent background and border colours. A third
test dispatches two tooltip frames with real time between them and checks that the shine
timer advanced.

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

## JVM differential tests

`crates/hfc-runtime/tests/jvm_diff.rs` pins the interpreter to the Java specification: each class
in `crates/hfc-runtime/tests/jvm/hfc/cases/` has a `run()` that returns deterministic labelled
lines (`name=value`), `hfc.Harness` runs every case on the machine's real JVM, and the Rust test
runs the same class in a fresh interpreter over the Minecraft runtime's `lib/modules` image and
compares the output line by line. Expected values always come from the JVM, never from the code
under test.

- The machine needs `java` (from `JAVA_HOME`, else `PATH`) with the `jdk.compiler` module, and a
  Minecraft installation for the JDK image. There is no `javac` binary on every machine: the test
  runs `java -m jdk.compiler/com.sun.tools.javac.Main`. It does not pass `--release` (a `-jre`
  package has no `ct.sym`); the classes are compiled for the running JDK.
- With `HFC_SKIP_JVM_DIFF=1`, a missing Java or Minecraft prints that it skipped instead of
  failing. Without it, the test fails with a clear message: a silent pass would hide the suite.
- `crates/hfc-runtime/tests/jvm/known_divergences.toml` lists `Case.label` entries the
  interpreter is known to get wrong, each with a one-line reason. A listed label that diverges is
  reported but does not fail; a listed label that starts matching (or is gone) fails, so the list
  can only shrink. Any unlisted divergence fails, with the interpreter's missing symbols.
- To add a case, add a class under `hfc.cases` with the `run()` convention above; it is picked up
  by the glob. Keep it deterministic (no time, identity hash codes, unordered maps over
  identity-keyed objects, threads or unseeded randomness) and give every line a label.
- A divergence is fixed in the interpreter's general path (an opcode, a JDK builtin, a host
  boundary), never in the case or the comparison. Remove its label from the list in the same
  commit.

## Census

`crates/hfc-runtime/examples/census.rs` measures the experimental Fabric runtime over a folder of
real mods: missing Java/Minecraft symbols, Fabric events mods listen to that the host never
dispatches, mixin targets and whether they apply, entrypoint failures, and calls to classes the
runtime replaces with native stand-ins. It writes `census.json` and `census.md` and, with
`--compare old.json`, a before/after section, so a fix can be chosen by how many mods it unblocks
and its effect can be quoted.

```sh
cargo run --release -p hfc-runtime --example census --     --mods test-mods/corpus --out /tmp/census [--resources <prepared resources>]     [--compare /tmp/old/census.json] [--alone | --together]
```

- The corpus is supplied by the user; the tool never downloads mods or scans personal folders.
  `test-mods/*.jar` is ignored by Git, so `test-mods/corpus/` can hold local jars; never commit
  them. A checked-in `test-mods/corpus.toml` may list file names and SHA-256s so the same corpus
  can be rebuilt.
- `--alone` runs each mod in its own bridge with its dependencies (one bridge per mod is what
  attributes a missing symbol to the mod that hit it). A missing dependency skips the mod with
  the reason. `--together` (the default) runs one bridge like a player's mods folder; events,
  mixins and registered counts are shared then, while entrypoints and their missing symbols are
  still attributed by class.
- Every run uses a fresh temporary game directory; mods' config files never reach player data.
- **It measures, it does not claim.** Zero missing symbols means init hit nothing the runtime
  lacks; it does not mean the mod works. Calls on native stand-ins are listed for a human to
  check against the builtins.
- The recorder (`FabricBridge::enable_census`) is off in the game, so it pays nothing there. The
  smoke test (`tests/census.rs`) runs the census over `test-mods/` and returns early without
  Minecraft or the pinned jars.

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

The repository's `.cargo/config.toml` sets `RUST_MIN_STACK` so test threads can hold the interpreter's host-stack recursion (100 Java frames) in an unoptimized build; without it Mod Menu and the classpath test overflow the default 2 MB.
