# Contributing

Thanks for helping. This project only works if its claims are true, so the rules below are
strict. A pull request that breaks one of them will be asked to change, however useful it is
otherwise.

## Before you start

- Read [docs/architecture/fabric-runtime.md](docs/architecture/fabric-runtime.md) before
  changing the runtime, and [docs/testing.md](docs/testing.md) before adding tests.
- Check open issues and pull requests so you don't duplicate someone's work. For anything
  bigger than a small fix, open an issue first and describe the approach.
- The repository has two tracks: the **HFC installer** (`crates/hfc-*` except
  `hfc-runtime`) and the **experimental runtime** (`crates/hfc-runtime` plus client hooks in
  the Horizon checkout). Don't mix their claims: the installer never executes mod code.

## The rules

### 1. Never fake compatibility

A mod whose entrypoint returns without an error is not "working". A listener that runs but
returns default values is partial at best. When you say a mod works, say what is actually
rendered, dispatched or stored, and back it with a test or a live run.

### 2. No mod-specific code

When a mod exposes a missing feature, fix the **shared path** so every mod using that API
benefits:

- an opcode or interpreter semantic,
- a Fabric API event,
- a Java or Minecraft class or method builtin,
- a config, resource or data format,
- a host boundary (files, rendering, registries, the desktop).

Never branch on a mod id, a mod's package or class name, a specific config field, a jar
filename or a version. Library adapters for APIs that many mods share (for example JamLib
events) are fine; adapters for one mod's own classes are not. Tests may load a single mod
to reproduce a bug, but the fix belongs in the general path.

### 3. Prefer real bytecode

Execute the mod's real bytecode, and the real JDK and Minecraft bytecode where possible.
Add a builtin only at a host boundary, or for a class the runtime deliberately replaces. A
builtin must have real semantics: a stub that returns a fixed value to make output look
right (for example a serializer that always returns `"{}"`) is faking compatibility.

### 4. Don't weaken checks

No skipped assertions, swallowed interpreter errors, silent fallbacks or fabricated
Minecraft classes in tests that claim real-classpath coverage. If the local Minecraft
installation or the test jars are missing, a test must skip or fail clearly, never pass.

### 5. Nothing third-party in Git

Never commit Minecraft jars or assets, decompiled sources, prepared resources, downloaded
Fabric jars, generated mod folders, saves, launcher accounts, session tokens or `.env`
files. Local runtime fixtures live in `test-mods/`, which Git ignores (see
[test-mods/README.md](test-mods/README.md)).

### 6. Keep test runs away from real player data

Use a temporary game directory for live tests. Never write into your real worlds,
`~/.local/share/horizon*` or `~/.local/bin`.

## Making a change

1. Reproduce the problem with a test that runs the mod's real code, and watch it fail.
2. Fix the shared path.
3. Run the formatter and the tests that cover your change:

   ```sh
   cargo fmt --all -- --check
   cargo test -p hfc-runtime --test <file>   # the file your change touches
   cargo test --workspace                    # before you open the pull request
   ```

   The full runtime suite loads the whole Minecraft classpath and is heavy; while
   iterating, run one test file with `-j2 -- --test-threads=1`.
4. Update the docs when behaviour changes: the runtime design note for runtime work, and
   the README's "What works" table only for something a test or live run verifies.
5. If you change `ENTRYPOINT_BUDGET` in `crates/hfc-runtime/src/bridge/mod.rs`, record the
   measured entrypoint costs in the runtime design note.

## Commits and pull requests

- Keep pull requests small and focused on one change.
- Write commit messages that say what changed and why.
- Commit runtime and installer changes in this repository. Changes to the Horizon client
  belong in the Horizon checkout (`workspace/horizon`), which has its own history and its
  own rules in its `AGENTS.md`.
- If an AI agent wrote the change, the commit ends with that agent's `Co-Authored-By:` line.
- In the pull request, list what you verified and how (test names, or what you saw in the
  live client), and anything you did not verify.

## Security

The interpreter is not a sandbox: a mod's code runs inside the game process with the
player's privileges. If you find a way for a jar to do something it should not (reach
outside the game directory, run a program, read secrets), report it privately to the
maintainer instead of opening a public issue.
