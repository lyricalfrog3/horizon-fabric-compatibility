# JVM exception propagation (`exception-propagation`): resolved

Merged to `main` on 2026-11-09.

## What the branch does

- Exceptions propagate as on a JVM; a failed `<clinit>` raises `ExceptionInInitializerError`, later uses `NoClassDefFoundError` (`interp.rs`, `ensure_initialized`). A failure caused by a call the runtime cannot resolve keeps the partial statics and is logged instead.
- Virtual dispatch resolves a runtime string to `java/lang/String`, so `CharSequence` interface calls (`subSequence`) run the real implementation.
- The `TextColor.fromLegacyFormat` shim is removed; the real bytecode returns `null` for formatting-only codes.
- `TextInputManager.applyTextInputArea` is a host-boundary builtin (no-op). It passes the focused text field's rectangle to the window system's IME through SDL and LWJGL's `MemoryStack`; there is no window or IME here. Without it the Mod Menu search box's render threw a `NullPointerException` and aborted the whole screen. The rest of `TextInputManager` runs its real bytecode.
- `registered_items`: Farmer's Delight wild-rice `half` is `["upper","lower"]`, which is vanilla `DoubleBlockHalf` order (checked against the 26.3 jar). The old order came from a removed placeholder fallback.
- `modmenu_screen` HTTP test sets a URI before `build()`, as the JDK requires.
- `BuiltInRegistries.acquireBootstrapRegistrationLookup` helper (`builtins.rs`).

## Test status (`cargo test --release -p hfc-runtime --no-fail-fast`)

All pass except two `modmenu_screen` tests that also fail on `main` before this branch:

- `a_fabric_key_mapping_opens_its_screen_and_the_host_replays_it` (mod list and widgets not rendered)
- `the_mod_list_scrolls_with_the_wheel_and_the_scrollbar` (wheel does not move the list)

Both are open work; Mod Menu must not be described as working.

## Known leftovers

- An `unreachable pattern` warning at `interp.rs` (`_ => false` after the `JavaObject` arms) pre-exists.
- LWJGL `Library.<clinit>` still fails with `UnsatisfiedLinkError` (no native libraries); it is recorded as a failed initializer. Any further LWJGL use needs its own general host boundary, not a name-based special case.
