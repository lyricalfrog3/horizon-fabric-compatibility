# Handoff: next runtime work, in order

There are four handoffs. Work through them **one at a time, in this order**. For each one, read
the file in full, do it, check its "Done when" list, commit, and only then open the next.

Before the first one, read the root `AGENTS.md`, [`docs/testing.md`](../testing.md) and
[`docs/architecture/fabric-runtime.md`](../architecture/fabric-runtime.md) once. For client
code, also read `workspace/horizon/AGENTS.md` and `workspace/horizon/CLAUDE.md`.

| # | File | What | Size | Touches client? |
|---|---|---|---|---|
| 1 | [block-use-review-fixes.md](block-use-review-fixes.md) | Fix 10 review findings on block use, ticks and the facts cache | large, do per finding | yes (findings 1, 2) |
| 2 | [fabric-state-persistence.md](fabric-state-persistence.md) | Save Fabric block ticks with the world (vanilla scheduler); atomic config writes | medium | yes |
| 3 | [jvm-differential-tests.md](jvm-differential-tests.md) | Test the interpreter against the real JVM; list known divergences | medium | no |
| 4 | [compat-census.md](compat-census.md) | Rank what is missing across a mod corpus the user supplies | medium | no |

## Why this order

1. **Review fixes first.** They are correctness bugs in code that is already committed, one of
   them an item duplication. Something may already be in progress: the client submodule has a
   `can_remove_blocks` hook (finding 2). Run `git status` in the root and in `workspace/horizon`,
   and skip what is done.
2. **Persistence next.** It builds on the same tick path that finding 10 touches, so do it while
   that code is fresh. It is the last known "looks working but is not" gap for ticking blocks.
3. **JVM tests before the census.** The census ranks missing features. The JVM tests catch
   features that exist but compute the wrong answer, which the census cannot see. Fix the
   interpreter's base first, so later census-driven fixes build on correct semantics.
4. **Census last**, because it decides what comes after these four. It needs a mod folder from
   the user: ask for one, and never download mods or scan personal folders.

## How to work each one

- Split the file into the steps it lists. Make one change, run only the test file named for it,
  commit. Do not batch several findings into one commit.
- Commands: `cargo fmt --check -p hfc-runtime`,
  `cargo test -p hfc-runtime --test <the file>`, and for client changes
  `(cd workspace/horizon && cargo build --profile fast -p horizon-client --no-default-features)`.
- Client changes: commit in `workspace/horizon`, push to remote `fork` branch `hfc`, then commit
  the submodule pointer at the root. Never push to upstream `horizon-rust/horizon`.
- Every commit ends with the agent's `Co-Authored-By:` line.

## Rules that matter in all four

- **Never fake compatibility.** If something cannot be carried out for real, report it as failed
  instead of pretending it worked.
- **No mod-specific code.** Fix the general path, never a branch on a mod id, class or jar.
- **Never weaken a test.** No skipped asserts, no fabricated Minecraft classes. A missing
  Minecraft install or JVM makes a test return early or fail clearly, never pass.
- **Keep runs out of player data.** Use temp game dirs and `mktemp -d` homes for live runs.
- If something is out of scope, write it in that handoff's notes and move on.

## When stuck

If a step's design does not fit the code you find (a function moved, a type differs), do not
force it. Write down what you found, choose the smallest change that keeps the rule the step
protects, and say so in the commit message. If a decision belongs to the user (corpus contents,
pushing, a behaviour change players will see), stop and ask.

## Finishing

After each handoff: update its section in `docs/architecture/fabric-runtime.md` as the file asks,
and tick it off here:

- [x] 1 review fixes
- [x] 2 persistence
- [x] 3 JVM differential tests
- [x] 4 census (then use its report as the backlog)
