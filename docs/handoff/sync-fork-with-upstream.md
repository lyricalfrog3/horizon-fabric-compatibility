# Handoff: bring the Horizon fork up to date with upstream, keeping our changes

You are working in **Horizon Fabric Compatibility** at `~/Desktop/horizon-rust-fabric`.
The Horizon client is the git submodule `workspace/horizon`, our fork `lyricalfrog3/horizon`, branch
`hfc`. Upstream Horizon moves fast (10,584 commits in the 7 days after our fork point). Your job:
merge upstream's `main` into `hfc`, keep every HFC change working, push the result to the fork, and
move the root repository's submodule pointer.

Read first: the root `AGENTS.md`, `workspace/horizon/AGENTS.md` / `CLAUDE.md`, and upstream's
`CONTRIBUTING.md` and `docs/layers.md` (both changed a lot recently).

## The two remotes (do not mix them up)

Inside `workspace/horizon`:

| Remote | URL | Use |
|---|---|---|
| `origin` | upstream `horizon-rust/horizon` | **read only: fetch from it, never push to it** |
| `fork` | `git@github.com:lyricalfrog3/horizon.git` | our fork; the only place you push |

The root repository's `origin` is `lyricalfrog3/horizon-fabric-compatibility` (HFC itself).

State when this was written (verify, it may have moved):

- Fork base: upstream `26c83240d`. Our `hfc` tip: `464372de1`. Root `main` pins that commit.
- A dry-run (`git merge-tree`) of `origin/main` into `hfc` conflicted in exactly these 17 files:
  `crates/horizon-client/src/{advancements,mod_content,mod_content_tests}.rs`,
  `crates/horizon-client/src/game.rs`,
  `crates/horizon-client/src/game/{agent,control,lan_actions,machines,mods,native_hooks,options,screens,startup_menu,stations}.rs`,
  `crates/horizon-client/src/lan/menu.rs`, `crates/horizon-mods/src/content.rs`, `docs/mods.md`.
  Expect different or more files by the time you run it.

## Rules

- **Merge, never rebase or force-push.** The root repository pins our commit hashes; rewriting them
  breaks the pin. The result must be a fast-forward of `fork/hfc`.
- **Never push to `origin`** (upstream) inside the submodule, and never open a PR there.
- **Keep both sides.** A conflict is not "take theirs" or "take ours". Upstream's change must land,
  and ours must still do what it did. When upstream moved or split code (they are splitting `game.rs`,
  `creatures.rs` and others into modules), re-apply our edit where that code now lives, by intent.
- **No weakening to get green.** Don't delete, skip or loosen a test, and don't remove one of our
  features to resolve a conflict. If a feature genuinely can't survive, stop and report.
- **Don't touch `Cargo.toml`/`Cargo.lock` by hand** beyond keeping our entries (below). If the lock
  conflicts, take upstream's and let cargo re-add ours.
- **Never run whole test suites** (it froze the user's PC). One crate or filter at a time with
  `nice -n 19 … -j2 … -- --test-threads=1`.
- **Never read or list `~/Downloads`** or other personal folders. Test jars are in `test-mods/`.
- **Never write** to `~/.local/share/horizon*` or `~/.local/bin`; scratch games use a `mktemp -d` dir.
- Commits end with `Co-Authored-By: <your model> <noreply@anthropic.com>`.

## Steps

1. **Preflight.** `cd workspace/horizon`. The tree must be clean (`git status`; `.cheapag/` untracked
   is fine). `git fetch origin && git fetch fork`. Confirm `git rev-parse HEAD` equals `fork/hfc`
   (push first if not, and say so). Make a safety tag: `git tag hfc-before-sync-$(date +%F)` (local only).
2. **Work on a branch.** `git switch -c hfc-sync`. Merge: `git merge origin/main` (no `--no-commit`
   tricks, no strategy options that auto-pick a side).
3. **For every conflicted file, learn our side first.** `git diff 26c83240d fork/hfc -- <file>` is
   exactly what we changed; `git log fork/hfc --format='%h %s' -- <file>` says why. What our changes are:
   - **`game/fabric_bridge/**`, `launcher/src/fabric_compat.rs`** (new files, rarely conflict): the
     Fabric bridge and launcher hook. Keep untouched.
   - **Glue calls in engine files** (`game.rs`, `native_hooks.rs`, `lan_actions.rs`, `machines.rs`,
     `screens.rs`, `startup_menu.rs`, `stations.rs`, `options.rs`, `lan/menu.rs`, `advancements.rs`,
     `agent.rs`, `control.rs`): small calls into `fabric_bridge`, for example the Mod Menu pause/title
     widgets and screen mouse/char input in `game.rs`, tooltip and right-click/destroy/placement
     hooks in `native_hooks.rs`, the opaque tooltip background in `stations.rs`, the container title in
     `lan_actions.rs`, mod advancements in `advancements.rs`.
   - **Content format** (`horizon-mods/src/content.rs`, `mod_content.rs`, `mod_content_tests.rs`,
     `docs/mods.md`, `docs/modpacks.md`, plus `horizon-items/src/items.rs`,
     `horizon-client-render/src/containers.rs`, `horizon-world/src/level/*`): block `shapes` by any
     property plus `collision`, `can_occlude`, the block `container` key, `tab_entries`, equipment
     assets under `assets/minecraft`, the loot `condition` fix, `Class::ModContainer`, namespace
     declarations. All of this must survive; see `docs/mods.md` upstream-of-ours for each.
   - `horizon-client/Cargo.toml` depends on `../../../../crates/hfc-runtime` (path); keep it.
   Resolve by keeping both. If upstream replaced the function or hook our call sat in, check what it
   became (`git log origin/main -S'<old name>'`) and attach our call to the new place. If upstream
   changed a hook's signature (see `native-hooks.json`, `docs/native-mods.md` API notes), adapt our
   call.
4. **Compile.** From `workspace/horizon`:
   `nice -n 19 cargo build -j2 --profile fast -p horizon-client --no-default-features`.
   Fix errors by adapting our code to upstream's new APIs, never by dropping features.
5. **Check upstream's own rules.** Upstream says CI enforces the layer rules (`docs/layers.md`) and
   public-tree checks. Our `horizon-client` depends on a crate outside their workspace, so find how
   their checks treat that (`tools/check_public_tree.py`, `xtask`, anything named layers) and run the
   cheap ones. Report anything they reject; don't change their rules.
6. **Tests, one at a time** (all filtered, `nice -n 19`, `-j2`, `--test-threads=1`):
   - `horizon-client --no-default-features --lib fabric_bridge::content`
   - `horizon-mods --lib content::`
   - `horizon-client-lib --lib mod_content::tests::` (the `#[ignore]`d slow ones only if you changed
     content-root code and a prepared root exists)
   - `horizon-items --lib` with a loot filter
   - Root, from `~/Desktop/horizon-rust-fabric`, each file on its own, `--release` for blocks:
     `cargo test -p hfc-runtime --test registered_items`, `--test registered_blocks`,
     `--test legendary_tooltips`, `--test config_files`, `--test rch_test`.
   Also run the tests of the upstream crates you had conflicts in, filtered to those modules.
7. **Live check** in a scratch game (never the user's). Short dir (the socket path must be short):
   ```sh
   T=$(mktemp -d /tmp/hfcm.XXXX); mkdir -p $T/game/horizon-mods
   cp test-mods/fabric-api-0.165.0+26.3.jar test-mods/mdm-3.1-fabric-26.3.jar \
      test-mods/emerald-tools-justfatlard-2.2.1.jar test-mods/emerald-armor-justfatlard-2.1.1.jar $T/game/horizon-mods/
   export HORIZON_HOME=$T/home XDG_DATA_HOME=$T/xd XDG_CONFIG_HOME=$T/xc HORIZON_NO_AUDIO=1 HORIZON_LAUNCHER_NO_SELF_UPDATE=1
   C="workspace/horizon/target/fast/horizon-ctl --socket $T/c.sock"
   $C launch --world W --seed 42 --creative --wait --offscreen \
     --resources ~/.local/share/horizon-launcher/versions/nightly-2026-11-01-c9d474d/.local/horizon-client/26.3 \
     --game-dir $T/game --home $T/home
   ```
   Check, with `give`, `use-item`, `interact`, `block`, `click-slot`, `screenshot`: `mdm:set_1_cabinet`
   places facing the way you look, opens a 27-slot "Storage" screen and keeps and drops its items;
   an `emerald_*` tool is in the creative tabs; armor is wearable; plain dirt still places (a past bug
   was every block failing to place). Quit and `rm -rf $T`. If the prepared resources version
   (`nightly-2026-11-01-…`) no longer matches the new build, say so rather than guessing a path.
8. **Commit the merge** on `hfc-sync` (the merge commit; describe the conflict choices in its body,
   with the Co-Authored-By line), then `git push fork hfc-sync:hfc`. It must be a fast-forward; if it
   is rejected, stop and report, do not force.
9. **Move the pointer in the root.** `cd ~/Desktop/horizon-rust-fabric`,
   `git add workspace/horizon`, commit "Move the Horizon client to the upstream-synced hfc" (with
   Co-Authored-By), `git push origin main`. Update `docs/research/horizon-current-state.md`: add the
   new upstream commit and date, and the new fork commit, to the revisions table.
10. **Report**: the new upstream base commit, the list of conflicts and how each was resolved, what you
    had to adapt, test and live results, anything dropped or not verified, and any upstream change
    that obsoletes part of our fork (for example a hook that now does what our glue does).

## If it goes wrong

- Mid-merge and stuck: `git merge --abort` returns to `hfc`. After committing, the safety tag
  `hfc-before-sync-<date>` is the way back; nothing is lost because `fork/hfc` is untouched until step 8.
- The root `main` keeps pinning the old good commit until step 9, so a failed sync never breaks it.
- Don't try to be clever with `git rerere` or `-X ours/theirs`; a wrong auto-resolution silently
  removes a feature.

## Done means

`fork/hfc` contains upstream `main` as of your fetch plus all our changes; the build, the listed tests and
the live check pass (or the gaps are reported); the root's submodule pointer and revisions doc are
updated and pushed; nothing was pushed to upstream.
