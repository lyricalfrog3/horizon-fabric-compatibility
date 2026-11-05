# Runtime test mods

These jars are **local test fixtures** for `crates/hfc-runtime` and are not committed. The
root `.gitignore` excludes `*.jar` here. Download the pinned versions into this folder before
running the runtime tests:

| File | Mod |
|---|---|
| `fabric-api-0.165.0+26.3.jar` | Fabric API |
| `Iceberg-26.3-fabric-1.4.3.1.jar` | Iceberg |
| `LegendaryTooltips-26.3-fabric-1.6.3.jar` | Legendary Tooltips |
| `Prism-26.3-fabric-1.1.3.jar` | Prism |
| `jamlib-fabric-2.3.1+26.3.x.jar` | JamLib |
| `modmenu-21.0.0.jar` | Mod Menu |
| `placeholder-api-3.2.0+26.3.jar` | Placeholder API |
| `rightclickharvest-fabric-4.6.2+26.3.x.jar` | Right Click Harvest |
| `emerald-tools-justfatlard-2.2.1.jar` | Emerald Tools |
| `emerald-armor-justfatlard-2.1.1.jar` | Emerald Armor |

The bridge and RCH tests also need a Minecraft installation at
`~/.local/share/horizon-launcher/minecraft` (or `HORIZON_MINECRAFT_DIR`). The real-classpath
tests skip when it is absent; they must not invent classes to simulate it.

```sh
cargo test -p hfc-runtime
```

Do not add these files to Git. If a test needs a new jar, add the filename and its exact
source/version here, and keep the test's assertions about real behavior rather than a stub.
