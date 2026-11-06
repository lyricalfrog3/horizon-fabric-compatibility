# Handoff: differential tests of the interpreter against a real JVM

You are adding a conformance test suite for the bytecode interpreter in `crates/hfc-runtime` in
**Horizon Fabric Compatibility** at `/home/daniel/Desktop/horizon-rust-fabric`. Read the root
`AGENTS.md`, [`docs/testing.md`](../testing.md) and
[`docs/architecture/fabric-runtime.md`](../architecture/fabric-runtime.md) first.

## Why

Every mod runs on the interpreter (`interp.rs`, `builtins.rs`, `num_builtins.rs`, ...). When it
gets basic Java semantics wrong (an exception that is not caught where Java catches it, a `finally`
that does not run, integer overflow, string switch hashing, a lambda capturing the wrong value), the
mod does not fail loudly. It computes something wrong, and the bug shows up far away as "this mod
almost works". The design note already lists places like that: exceptions answered with
defaults, Kotlin null checks that continue, `TimeUnit.convert` missing.

A differential test pins the interpreter to the real spec: compile small Java programs, run each
on the machine's real JVM and in the interpreter, and require identical output. The JVM is the
oracle, so expected values are never derived from the code under test.

## What is on this machine

- `java` 21 (`/usr/lib/jvm/java-21-openjdk-amd64/bin/java`). There is **no `javac` binary**, but
  the `jdk.compiler` module is present (`java --list-modules | grep jdk.compiler`). Compile with
  `java -m jdk.compiler/com.sun.tools.javac.Main --release 21 -d <out> <files>`.
- Check for `jdk.jartool` the same way if you need a jar. Writing the jar from Rust with the
  `zip` crate is also fine; check whether `hfc-runtime` already depends on it (the loader reads
  jars, so it likely does).
- The interpreter needs the JDK's own classes (`java/lang/String`, `StringBuilder`, collections).
  Use the Minecraft runtime's `lib/modules` jimage (`tests/common/mod.rs` `image()`), the same as
  the other real-classpath tests, or the system JDK's
  `/usr/lib/jvm/java-21-openjdk-amd64/lib/modules`. Pick one, and say which in the test's doc
  comment. Prefer the Minecraft runtime when it exists, since that is what mods get at play time.
  Then the JVM side should use the same Java major version if possible, or the comparison should
  avoid behaviour that changed between versions.

Find the Java executable through `JAVA_HOME` first, then `PATH`. When no Java with
`jdk.compiler` is found, the test **fails with a clear message** unless the env var
`HFC_SKIP_JVM_DIFF=1` is set, in which case it prints that it skipped. Do not silently pass.
Mirror how the other real-classpath tests treat a missing Minecraft (`docs/testing.md`), and
document the choice there.

## Layout

```
crates/hfc-runtime/tests/jvm/            # Java sources, committed (our own code, no third-party)
    Harness.java                         # the JVM-side driver
    cases/Arith.java
    cases/Exceptions.java
    ...
crates/hfc-runtime/tests/jvm_diff.rs     # the Rust test
crates/hfc-runtime/tests/jvm/known_divergences.toml
```

### Case convention

Each case is a class in package `hfc.cases` with:

```java
public final class Exceptions {
    public static String run() {
        StringBuilder out = new StringBuilder();
        // ... many small checks, each appending a labelled line:
        out.append("finally_runs=").append(finallyRuns()).append('\n');
        return out.toString();
    }
}
```

- `run()` must be deterministic: no time, no hash codes of objects (`Object.hashCode` identity),
  no `HashMap` iteration order over identity-hashed keys, no threads, no randomness without a
  fixed seed, no system properties. String and boxed-Integer hash codes are deterministic and
  allowed.
- Catch exceptions inside the case and append `ExceptionClass: message`, so the output compares
  them too.
- One concern per line, with a label, so a divergence names exactly what differs.

`Harness.java` takes class names and prints, for each one, `=== <class>` followed by its `run()`
output or `UNCAUGHT <exception class>: <message>`.

### The Rust test (`jvm_diff.rs`)

1. Compile `tests/jvm/**/*.java` into a temp dir (`CARGO_TARGET_TMPDIR` or a tempdir), once per
   test process.
2. Run `java -cp <out> hfc.Harness <all case classes>` and parse the sections.
3. For each case, build an interpreter with the JDK image plus the compiled classes. Find how the
   loader takes a directory or jar: `FabricBridge::add_jar` and the `Interpreter`'s class-loading
   API in `loader.rs`. If only jars load, zip the output dir. Then call
   `execute_method("hfc/cases/<Name>", "run", "()Ljava/lang/String;", &[])` and read the Java
   string out of the heap (`heap.get_string`). Turn on `trace_missing` and include `missing` in
   the failure message.
4. Compare line by line. On a mismatch, print the case, the label, JVM value, interpreter value,
   and missing symbols.
5. **Known divergences:** `known_divergences.toml` lists `case.label` entries the interpreter is
   known to get wrong today, each with a one-line reason. A listed label that diverges is
   reported but does not fail. A listed label that now **matches** fails the test with "remove
   it from known_divergences.toml", so the list can only shrink and nothing is hidden. Any
   unlisted divergence fails. This keeps the suite green while it documents real bugs, without
   weakening any assertion. The list is the bug list.

Run each case in its own interpreter, so one case's static state cannot leak into another, and
under a step budget (`interp.step_budget = Some(..)`) so a mis-decoded loop fails rather than
hangs.

## Cases to write (first batch)

Write these as separate classes. Every bullet is several labelled lines.

- **Arith:** `int`/`long` overflow, `Integer.MIN_VALUE / -1`, `%` with negatives, shifts with
  counts ≥ 32/64 and negative counts, `>>>` on negatives, `(int)` casts from `double` NaN/±Inf/
  out-of-range, `float`/`double` NaN comparisons (`fcmpl`/`fcmpg`), `-0.0` vs `0.0`, `Math.floorMod`,
  `Math.round` edge values, `char` arithmetic, `byte`/`short` narrowing, `Long.compareUnsigned`,
  `Integer.toBinaryString`, `Double.toString` of awkward values (`0.1+0.2`, `1e21`, `1e-7`).
- **Exceptions:** catch by superclass, nested try/catch/finally order, `finally` overriding a
  return, `finally` running on `break`/`continue`, rethrow, exception in `finally`,
  try-with-resources close order and suppressed exceptions, `ArithmeticException` on `/ 0`,
  `NullPointerException` on a null field read and an invoke, `ArrayIndexOutOfBounds`,
  `ClassCastException`, `NegativeArraySize`, an exception thrown inside a static initializer
  (`ExceptionInInitializerError` and then `NoClassDefFoundError` on second use), and stack
  unwinding through several frames.
- **Strings:** `switch` on strings (including hash collisions such as `"Aa"`/`"BB"`), `+`
  concatenation via `invokedynamic` `makeConcatWithConstants` with every primitive type, `null`
  and `char`, `String.format` with `%d %s %.3f %x %08d`, `intern`/`==` on literals, `split`
  with regex and limit, `StringBuilder` insert/delete/reverse, `chars()`, `compareTo`, `repeat`,
  `strip`, text blocks.
- **Objects:** virtual dispatch through an abstract class and an interface default method,
  `super.` calls, a diamond of default methods resolved with `X.super.m()`, static vs instance
  field shadowing, static init order across two classes, `instanceof` with patterns, records
  (`equals`/`hashCode`/`toString`), enums (`values`, `valueOf`, `ordinal`, a switch on an enum,
  constant-specific bodies), inner and anonymous classes capturing locals and `this`, arrays of
  arrays, `clone()` of arrays.
- **Lambdas:** `Runnable`/`Function`/`Supplier`, method references of all four kinds
  (static, bound, unbound, constructor), capturing effectively final locals in a loop, lambdas
  implementing a generic interface (bridge methods), `Comparator.comparing(..).thenComparing(..)`,
  streams (`map/filter/sorted/collect(Collectors.toList / joining / groupingBy into TreeMap)`).
- **Collections:** `ArrayList`, `LinkedList`, `ArrayDeque`, `TreeMap`, `LinkedHashMap` order,
  `HashMap` with `String`/`Integer` keys (iteration order is deterministic for those),
  `Collections.sort` stability, `List.of` immutability exceptions, `Iterator.remove`, a
  `ConcurrentModificationException`.
- **Switch and control flow:** `tableswitch`/`lookupswitch` (dense, sparse, negative keys),
  switch expressions with `yield`, labelled break/continue, a long loop (10⁵ iterations) that must
  finish within the budget.

Later batches, once the first is green or listed: generics with wildcards and casts, varargs,
`Optional`, `java.util.function` composition, `BigInteger`/`BigDecimal`, `Character` classes,
`StringBuilder` code points, interfaces with static methods, `invokedynamic` for records'
`ObjectMethods`, sealed classes, `instanceof` record patterns, nested generic lambdas, Kotlin-style
patterns compiled from Java (`Objects.requireNonNull` messages).

## Rules

- Never edit an expected value by hand. The JVM produces them all. If the JVM's output depends
  on the version, change the case, not the comparison.
- Do not "fix" a divergence in the test. Fix the interpreter in its general path (an opcode in
  `interp.rs`, a JDK builtin), or list it in `known_divergences.toml` with a reason.
- Do not add builtins that replace JDK classes the interpreter can run as real bytecode from the
  image. Fix the interpreter instead (root `AGENTS.md`, "Prefer executing the mod's real
  bytecode").
- Keep the suite fast: the whole test should run in well under a minute in debug. Compiling is
  the slow part; do it once.

## Done when

- `cargo test -p hfc-runtime --test jvm_diff` compiles the cases, runs both sides and passes,
  with every current divergence listed in `known_divergences.toml` and a reason for each.
- `docs/testing.md` has a "JVM differential tests" section: how they work, how to add a case, the
  `HFC_SKIP_JVM_DIFF` switch, and the rule that the divergence list may only shrink.
- `docs/architecture/fabric-runtime.md` links the divergence list as the interpreter's known
  semantic gaps.
- Follow-up (separate commits): fix divergences one at a time, starting with exceptions, since
  the design note already flags exception handling as a source of wrong behaviour in real mods.
  Remove each label from the list as it is fixed.
