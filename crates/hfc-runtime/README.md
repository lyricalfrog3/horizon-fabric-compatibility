# hfc-runtime

Experimental in-client Fabric bytecode interpreter used by the Horizon client. This is **not**
the HFC installer and does not claim general Fabric compatibility: it executes selected
entrypoints, dispatches a small set of events, and supplies Java/Minecraft builtins.

It is not a JVM and not a sandbox. Mod bytecode runs in the client process.

- Design, measurements and current limits: [../../docs/architecture/fabric-runtime.md](../../docs/architecture/fabric-runtime.md)
- Agent rules for this repository: [../../AGENTS.md](../../AGENTS.md)
- Local test jars (not committed): [../../test-mods/README.md](../../test-mods/README.md)

```sh
cargo test -p hfc-runtime
cargo fmt --check -p hfc-runtime
```

Known gaps include Java screens/widgets, Mixins, `ScreenEvents`/`ClientTickEvents`, and the
Iceberg `GATHER`/`POSTEXT` tooltip frame path.
