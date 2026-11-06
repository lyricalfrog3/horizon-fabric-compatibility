# Handoff: a compatibility census over a corpus of Fabric mods

You are building a measurement tool for the experimental Fabric runtime (`crates/hfc-runtime`) in
**Horizon Fabric Compatibility** at `/home/daniel/Desktop/horizon-rust-fabric`. Read the root
`AGENTS.md`, [`docs/testing.md`](../testing.md) and
[`docs/architecture/fabric-runtime.md`](../architecture/fabric-runtime.md) first.

## Why

So far the runtime has grown one mod at a time: load a mod, find what breaks, fix the general
path. That works, but it gives no answer to "what should we fix next to help the most mods?" A fix
for a gap that 40 mods hit is worth more than one that only the mod in front of us hits, and today
nothing tells the two apart.

The census answers that question with data. It runs a folder of real mods through the runtime
and ranks what is missing **by how many mods it affects**:

- Java/Minecraft methods and classes the interpreter could not resolve.
- Fabric API events mods register listeners on that the host never dispatches.
- Mixin targets mods declare, and whether the runtime applies them.
- Entrypoints that fail, exceed the budget, or take too long.
- Calls on the runtime's native stand-in classes (`ItemStack`, `Player`, `Level`, ...) that a mod
  makes, a static list of where a stand-in may lack a method.

It also gives a before/after number for every fix ("this change took 12 mods from 3 missing
symbols to 0"), which makes progress honest and visible.

**The census measures, it does not claim.** A mod with zero missing symbols is not "working".
Its init simply hit nothing the runtime lacks. The report must say so in its header (root
`AGENTS.md`, "Never fake compatibility").

## Ground rules

- **The corpus is supplied by the user.** Do not download mods, and do not scan `~/Downloads`
  or other personal folders. The census takes a folder path argument. The default is
  `test-mods/corpus/`, which the root `.gitignore` already covers (`test-mods/*.jar`; check that
  the subfolder is covered too, and add `test-mods/corpus/` to `.gitignore` if not). Never commit
  jars.
- **Commit a manifest instead.** Optionally add `test-mods/corpus.toml`, listing for each jar its
  file name, mod id, version, source URL and SHA-256, so the user can rebuild the same corpus.
  The census can print the SHA-256 lines to make this easy.
- **Keep runs out of player data.** Each run uses a fresh temporary game dir
  (`tempfile::tempdir()` or `std::env::temp_dir()` plus a unique name) passed to
  `FabricBridge::set_game_dir`. Mods write their config files there, never into
  `~/.local/share/horizon*`.
- **No mod-specific code** in the tool or in the fixes it leads to.
- The interpreter is not a sandbox. The corpus is untrusted code running in-process, so treat the
  folder accordingly (the user chooses what goes in it).

## What to build

### Where

An example binary: `crates/hfc-runtime/examples/census.rs`. It is not a test, because it needs a
user corpus and takes minutes. Reuse the Minecraft-discovery helpers from the tests with
`#[path = "../tests/common/mod.rs"] mod common;` (`minecraft_dir`, `version_jar`, `jars`,
`image`, `bridge_with_vanilla`). If that module has test-only items that don't compile in an
example, move the discovery functions to a small `pub` module the tests and the example both use.
Do not duplicate them.

```sh
cargo run --release -p hfc-runtime --example census -- \
    --mods test-mods/corpus --out <dir> [--resources workspace/horizon/.local/horizon-client/26.3] \
    [--compare <old census.json>] [--alone | --together]
```

- Without a Minecraft installation, exit non-zero with a clear message. Do not run on
  fabricated classes.
- `--resources` points at the client's prepared resources for `FabricBridge::load_registries`
  (registry ids and tags). When it is absent, run anyway and note in the report that registry
  lookups answered from an empty snapshot.

### Two run modes

- `--together` (default): every jar in one bridge, the way a player's mods folder runs. One
  bridge, one `init()`.
- `--alone`: each mod in its own fresh bridge with only its declared dependencies from the same
  folder. This is what attributes a missing symbol to the mod that hit it. Resolve dependencies
  with `crates/hfc-fabric`, which already reads `fabric.mod.json` and builds the dependency graph.
  Reuse it, do not re-parse. A mod whose dependency is not in the folder is reported as
  `skipped: missing dependency <id>`. Fabric API, Fabric Loader and `minecraft` count as present.

Run `--alone` with a thread per mod only if `FabricBridge` is `Send`. Otherwise run sequentially.
Correctness matters more than speed here.

### What to record per mod (per entrypoint where possible)

Turn on `interp.trace_missing = true` (and `trace_calls` for caller context) before `init()`.

1. **Entrypoints.** For each entrypoint: class, kind (main/client), result (`ok` or the error
   text), instructions used, wall time, and whether it ran out of budget (`ENTRYPOINT_BUDGET`).
   `FabricBridge::init` runs them in a loop (`bridge/mod.rs`, around the
   `for entry in &sorted_entrypoints` loop). Add an optional recorder: a
   `pub census: Option<CensusLog>` field on the bridge that, when `Some`, gets
   `(entrypoint, result, steps, elapsed, missing_range)` pushed after each entrypoint, where
   `missing_range` is the slice of `interp.missing` added during it. Keep it `None` by default so
   the game pays nothing. This is the only change to the runtime the census needs; keep it small.
2. **Missing symbols.** `interp.missing` entries look like `class <name>` or
   `<owner>.<name><descriptor>` (see `interp.rs` `note_missing`, ~line 1628), optionally followed
   by ` <- caller` frames. Strip the caller suffix for grouping, keep one example caller per
   symbol, and count occurrences.
3. **Events registered but not dispatched.** After `init()`, walk the heap for Fabric API event
   objects with listeners. The runtime represents an `Event` as an instance with a `listeners`
   array (see how `events.rs` reads `UseBlockCallback.EVENT`). The way to find them generically is
   to iterate the static fields the interpreter has initialized (find the statics map in
   `interp.rs`) and select values whose object has a non-empty `listeners` array (or a non-null
   `listener` for JamLib-style events). Record `owner_class.FIELD` and the listener count.
   - Compare against the events the host dispatches. Add to the bridge a
     `pub const DISPATCHED_EVENTS: &[&str]` listing every `owner.FIELD` that `events.rs` and the
     other bridge files call (grep `resolve_static_field(` in `crates/hfc-runtime/src/bridge/`).
     Keep it next to the dispatch code so it stays in sync, and add a unit test that greps nothing
     but checks the list has no duplicates. If you can, make the dispatch code itself use the
     constants so they cannot drift.
4. **Mixins.** For each mixin config the mod declares (`fabric.mod.json` `mixins`), list each
   mixin class, its targets, and its injection kinds (`@Inject`, `@Redirect`, `@ModifyArg`,
   `@Overwrite`, accessor/invoker, interface). `crates/hfc-runtime/src/mixin.rs` already parses
   these. Find its data structures and reuse them. Then record whether the runtime applies each
   one: what `mixin.rs` does with it (applied, applied partially such as HEAD-only, or ignored).
   If `mixin.rs` does not track this, add an outcome enum it fills in as it processes injectors.
   Also mark whether the target class is one the interpreter runs as bytecode, or one Horizon
   implements natively in Rust, where no mixin can ever apply. Use the stand-in list from step 5:
   a class in that list, or any `net/minecraft` gameplay class the client never interprets, counts
   as native. Say in the report that this classification is approximate.
5. **Static references to native stand-ins.** Read every class file in the mod jar (the loader
   already parses classes; see `class.rs`/`loader.rs`) and collect `Methodref`/
   `InterfaceMethodref` constant-pool entries whose owner is a class the runtime replaces with a
   native object. Get that list from `NativeObject::class_name()` implementations in
   `minecraft.rs` and the other `*_builtins.rs`: generate it with a `pub fn native_classes() ->
   &'static [&'static str]` next to them, maintained by hand with a unit test that checks every
   listed class has a `NativeObject` impl if that is easy, otherwise just a comment. Record
   `(owner, name, descriptor)` and the referring mod. These are calls that, if reached, run
   against a stand-in that may not implement them. The census cannot know whether a builtin
   covers it (builtins are if-chains), so the report lists them for a human or agent to check.
6. **What was registered:** counts of registered blocks and items (`registered_blocks()`, the
   items accessor), creative tab entries, key mappings and screens, from the bridge's existing
   accessors. This is a sanity number, not a claim.

### Output

Write to `--out` (default a new folder under the system temp dir, printed at the end. Not the
repo, unless the user passes a path):

- `census.json`: everything above, machine-readable, with a `version` field, the HFC git
  commit (`git rev-parse HEAD` if available, else `"unknown"`), the corpus manifest
  (file, mod id, version, sha256), the run mode, and per-mod records. Its schema goes in a doc
  comment at the top of `census.rs`.
- `census.md`: the human report. Sections:
  1. Header: corpus size, mode, commit, date, and the sentence "Zero missing symbols means init
     hit nothing the runtime lacks; it does not mean the mod works."
  2. **Top missing symbols by number of mods affected** (top 50): symbol, #mods, #hits, one
     example caller, mod ids.
  3. **Top missing classes/owners** (the same, grouped by owner class): good for "implement
     this whole class".
  4. **Events mods listen to that the host never dispatches**, ranked by #mods.
  5. **Mixin targets** ranked by #mods, with an applied/ignored/native column.
  6. **Stand-in methods referenced**, ranked by #mods.
  7. **Entrypoint failures and budget overruns**, grouped by first error line.
  8. Per-mod table: id, version, entrypoints ok/total, #missing, #undispatched events,
     #mixins applied/total, init time.
- With `--compare old.json`: an extra first section, **Changes since <old commit>**: symbols
  that disappeared or appeared, mods whose missing count changed, and entrypoints that went from
  error to ok or the other way.

### Tests

- Unit tests (no Minecraft needed): missing-symbol normalization (caller stripping, grouping by
  owner), ranking order, the `--compare` diff, and JSON round-trip of the schema. Use
  hand-written `census.json` inputs, which contain no Minecraft classes.
- One real-classpath smoke test in `crates/hfc-runtime/tests/` that runs the census library
  functions (put the logic in a module the example calls, so a test can call it too) over
  `test-mods/` itself with `--alone`. It should assert that Mod Menu's entrypoints are recorded,
  that `ItemTooltipCallback.EVENT` is not reported as undispatched (it is dispatched), and that
  JSON is written. It returns early when Minecraft or the jars are absent, like the other
  real-classpath tests. Do not assert on missing-symbol counts, since they change with every fix.

## Done when

- `cargo run --release -p hfc-runtime --example census -- --mods test-mods --alone` runs over the
  pinned test mods and writes `census.json` and `census.md`.
- `cargo test -p hfc-runtime --lib` and the new smoke test pass, and `cargo fmt --check -p
  hfc-runtime` is clean.
- `docs/testing.md` gets a short "Census" section: what it measures, how to run it, where the
  corpus goes, and the warning that it does not measure working behaviour.
- The runtime change is limited to the opt-in `census` recorder, `DISPATCHED_EVENTS`,
  `native_classes()` and, if needed, mixin outcome tracking. The game pays nothing when the
  census is off.

## Then

Use the report as the backlog. Each fix takes the top-ranked item, fixes the **general** path
(an opcode, a builtin at a host boundary, an event dispatch), re-runs the census with
`--compare`, and quotes the change in its commit message.
