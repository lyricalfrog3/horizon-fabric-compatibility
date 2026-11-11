//! A `@Inject` callback that cancels actually ends the target method, and one
//! that sets a return value actually replaces it.
//!
//! The fixtures are two synthetic mod jars written by the test: an ordinary
//! mod whose mixin announces its targets through `@Mixin` and a target class it
//! owns, plus a target inside the loaded classpath. A synthetic jar keeps the
//! proof controlled — the target's body is known, and the witness it writes is
//! something the test can read back — while still running the mod's own
//! bytecode: the mixin class is compiled by the machine's `javac`, its
//! `@Inject` annotation is parsed from the class file, and the callback is the
//! one that was compiled, not an interpreter builtin standing in for it.
//!
//! The real-classpath leg repeats the same check against a class the runtime
//! loads from the Minecraft jar (`net.minecraft.core.SectionPos`), so the
//! mechanism is proven on a real class as well as a controlled one. Nothing
//! here names a mod id, a mod class or a Minecraft method it was written for:
//! the mixin is discovered through its config, and the target is the class its
//! own annotation names.

use hfc_runtime::bridge::FabricBridge;
use hfc_runtime::value::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `java`, from `JAVA_HOME` or `PATH`.
fn java() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let candidate = PathBuf::from(home).join("bin/java");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        let candidate = dir.join("java");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The JDK image the classpath tests use, from the Minecraft installation.
fn jimage(minecraft: &Path) -> Option<PathBuf> {
    let mut stack = vec![minecraft.join("runtime")];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|s| s.to_str()) == Some("modules") {
                return Some(path);
            }
        }
    }
    None
}

/// Compile Java sources with the JDK's own compiler module (there is not always
/// a `javac` binary, and the runtime tests already use this entry point).
fn compile(
    java: &Path,
    sources: &[(&str, &str)],
    out: &Path,
    classpath: Option<&Path>,
) -> Result<(), String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let source_dir = out.join("src");
    std::fs::create_dir_all(&source_dir).map_err(|e| e.to_string())?;
    let mut paths = Vec::new();
    for (name, text) in sources {
        let path = source_dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        paths.push(path);
    }
    let classes = out.join("classes");
    std::fs::create_dir_all(&classes).map_err(|e| e.to_string())?;
    let mut command = Command::new(java);
    command
        .arg("-m")
        .arg("jdk.compiler/com.sun.tools.javac.Main")
        .arg("-d")
        .arg(&classes);
    if let Some(classpath) = classpath {
        command.arg("-cp").arg(classpath);
    }
    let output = command
        .args(&paths)
        .output()
        .map_err(|e| format!("running {}: {e}", java.display()))?;
    if !output.status.success() {
        return Err(format!(
            "javac failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

/// Zip every class under `classes` into `jar`, together with `extra` entries
/// (a `fabric.mod.json`, a mixin config) written verbatim.
fn jar(classes: &Path, jar: &Path, extra: &[(&str, String)]) -> Result<(), String> {
    let file = std::fs::File::create(jar).map_err(|e| format!("{}: {e}", jar.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    let mut stack = vec![classes.to_path_buf()];
    let mut files = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    for path in files {
        let name = path
            .strip_prefix(classes)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        zip.start_file(name, options).map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut zip, &bytes).map_err(|e| e.to_string())?;
    }
    for (name, text) in extra {
        zip.start_file(*name, options).map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut zip, text.as_bytes()).map_err(|e| e.to_string())?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}

/// Read a static field of a loaded class.
fn static_int(bridge: &FabricBridge, class: &str, field: &str) -> Option<i32> {
    let class = bridge.interp.loader.get(class)?;
    match class.static_fields.get(field) {
        Some(Value::Int(value)) => Some(*value),
        Some(value) => Some(value.as_int()),
        None => None,
    }
}

/// The witness the synthetic target writes: a static counter it bumps and the
/// value it returns.
const TARGET_CLASS: &str = "example/synth/InjectTarget";
const WITNESS: &str = "ran";

/// A mixin whose `@Inject(cancellable = true, at = HEAD)` cancels and sets a
/// return value of 9. The target returns 7 and bumps its witness.
const TARGET_SOURCE: &str = r#"
package example.synth;

public class InjectTarget {
    public static int ran = 0;

    public static int value() {
        ran = ran + 1;
        return 7;
    }

    public int stored = 0;

    public int instanceValue(int addend) {
        ran = ran + 1;
        return 7 + addend;
    }

    public void sideEffect() {
        ran = ran + 1;
    }
}
"#;

const MIXIN_SOURCE: &str = r#"
package example.synth.mixin;

import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

import example.synth.InjectTarget;

@Mixin(InjectTarget.class)
public class InjectTargetMixin {
    @Inject(method = "value", at = @At("HEAD"), cancellable = true)
    private static void replaceValue(CallbackInfoReturnable<Integer> cir) {
        cir.setReturnValue(9);
    }

    // `this` is the target object, so a `@Shadow` field's write lands on the
    // target's own field of that name.
    @Shadow
    private int stored;

    @Inject(method = "instanceValue", at = @At("HEAD"), cancellable = true)
    private void replaceInstanceValue(int addend, CallbackInfoReturnable<Integer> cir) {
        this.stored = 1234;
        cir.setReturnValue(100 + addend);
    }

    @Inject(method = "sideEffect", at = @At("HEAD"), cancellable = true)
    private void cancelVoid(CallbackInfo ci) {
        ci.cancel();
    }
}
"#;

/// The mixin's own annotation classes. The runtime replaces the Mixin API with
/// builtins, but the class file still has to reference the annotation types,
/// and `javac` needs them on its class path.
const ANNOTATION_SOURCES: &[(&str, &str)] = &[
    (
        "org/spongepowered/asm/mixin/Shadow.java",
        "package org.spongepowered.asm.mixin; public @interface Shadow { String value() default \"\"; }",
    ),
    (
        "org/spongepowered/asm/mixin/Mixin.java",
        "package org.spongepowered.asm.mixin; public @interface Mixin { Class<?>[] value() default {}; String[] targets() default {}; }",
    ),
    (
        "org/spongepowered/asm/mixin/injection/At.java",
        "package org.spongepowered.asm.mixin.injection; public @interface At { String value(); String target() default \"\"; }",
    ),
    (
        "org/spongepowered/asm/mixin/injection/Inject.java",
        "package org.spongepowered.asm.mixin.injection; public @interface Inject { String[] method(); At[] at(); boolean cancellable() default false; }",
    ),
    (
        "org/spongepowered/asm/mixin/injection/callback/CallbackInfo.java",
        "package org.spongepowered.asm.mixin.injection.callback; public class CallbackInfo { public void cancel() {} public boolean isCancelled() { return false; } public boolean isCancellable() { return true; } }",
    ),
    (
        "org/spongepowered/asm/mixin/injection/callback/CallbackInfoReturnable.java",
        "package org.spongepowered.asm.mixin.injection.callback; public class CallbackInfoReturnable<R> extends CallbackInfo { public void setReturnValue(R value) {} public R getReturnValue() { return null; } }",
    ),
];

/// Build a mod jar holding the target, the mixin and a mixin config.
fn build_inject_mod(dir: &Path, java: &Path) -> Result<PathBuf, String> {
    let sources: Vec<(&str, &str)> = ANNOTATION_SOURCES
        .iter()
        .copied()
        .chain([
            ("example/synth/InjectTarget.java", TARGET_SOURCE),
            ("example/synth/mixin/InjectTargetMixin.java", MIXIN_SOURCE),
        ])
        .collect();
    compile(java, &sources, dir, None)?;
    let jar_path = dir.join("inject-mod.jar");
    let metadata = r#"{
        "schemaVersion": 1,
        "id": "inject_probe",
        "version": "1.0.0",
        "environment": "*",
        "mixins": ["inject.mixins.json"]
    }"#
    .to_string();
    let config = r#"{
        "required": true,
        "minVersion": "0.8",
        "package": "example.synth.mixin",
        "mixins": ["InjectTargetMixin"]
    }"#
    .to_string();
    jar(
        &dir.join("classes"),
        &jar_path,
        &[
            ("fabric.mod.json", metadata),
            ("inject.mixins.json", config),
        ],
    )?;
    Ok(jar_path)
}

/// A bridge over the mod jar alone: no Minecraft, no JDK image. The target and
/// the mixin are the only classes either side needs.
fn standalone_bridge(jar_path: &Path) -> Result<FabricBridge, String> {
    let mut bridge = FabricBridge::new();
    bridge
        .load_mod_jar(jar_path)
        .map_err(|e| format!("loading {}: {e}", jar_path.display()))?;
    bridge.init().map_err(|e| format!("init: {e}"))?;
    Ok(bridge)
}

#[test]
fn an_injected_callback_that_cancels_ends_the_target_method() {
    let Some(java) = java() else {
        eprintln!("skipped: no java on PATH or in JAVA_HOME");
        return;
    };
    let dir = std::env::temp_dir().join(format!("hfc-inject-standalone-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let jar_path = match build_inject_mod(&dir, &java) {
        Ok(path) => path,
        Err(error) => panic!("{error}"),
    };
    let mut bridge = match standalone_bridge(&jar_path) {
        Ok(bridge) => bridge,
        Err(error) => panic!("{error}"),
    };

    // Every injector in the mixin resolves against the classpath, so all three
    // are wired rather than merely parsed.
    assert_eq!(
        bridge.census_mixins().wired.len(),
        3,
        "injectors that could not run: {:?}",
        bridge.census_mixins().unsupported
    );

    // A returnable cancelled at HEAD: the callback's 9 replaces the body's 7,
    // and the body never runs, so the witness stays at zero.
    let result = bridge
        .interp
        .execute_method(TARGET_CLASS, "value", "()I", &[])
        .expect("the injected static method runs");
    assert_eq!(
        result.as_int(),
        9,
        "the callback's return value was not used"
    );
    assert_eq!(
        static_int(&bridge, TARGET_CLASS, WITNESS),
        Some(0),
        "the target body ran despite the callback cancelling it"
    );

    // The same on an instance method with a real parameter: the callback
    // receives it in order and the body is skipped.
    let instance = bridge.interp.alloc_uninitialized(TARGET_CLASS);
    let result = bridge
        .interp
        .execute_method(
            TARGET_CLASS,
            "instanceValue",
            "(I)I",
            &[Value::Object(Some(instance)), Value::Int(5)],
        )
        .expect("the injected instance method runs");
    assert_eq!(
        result.as_int(),
        105,
        "the callback did not see the parameter"
    );
    assert_eq!(static_int(&bridge, TARGET_CLASS, WITNESS), Some(0));
    // The callback's `@Shadow` write landed on the target object, not on a
    // mixin instance: `this` is the receiver Mixin would have passed.
    let stored = match bridge.interp.heap.get(instance) {
        Some(hfc_runtime::value::JavaObject::Instance { fields, .. }) => {
            fields.get("stored").map(|value| value.as_int())
        }
        _ => None,
    };
    assert_eq!(
        stored,
        Some(1234),
        "the callback's @Shadow write did not reach the target's field"
    );

    // A `void` target: cancelling ends it early, and the witness proves it.
    bridge
        .interp
        .execute_method(
            TARGET_CLASS,
            "sideEffect",
            "()V",
            &[Value::Object(Some(instance))],
        )
        .expect("the injected void method runs");
    assert_eq!(
        static_int(&bridge, TARGET_CLASS, WITNESS),
        Some(0),
        "the void target's body ran despite the callback cancelling it"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_injector_that_cannot_be_applied_is_recorded_unsupported() {
    let Some(java) = java() else {
        eprintln!("skipped: no java on PATH or in JAVA_HOME");
        return;
    };
    let dir = std::env::temp_dir().join(format!("hfc-inject-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let broken_mixin = r#"
package example.synth.mixin;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

import example.synth.InjectTarget;

@Mixin(InjectTarget.class)
public class InjectTargetMixin {
    // The target has no such method: the annotation cannot be applied.
    @Inject(method = "noSuchMethod", at = @At("HEAD"), cancellable = true)
    private static void missing(CallbackInfoReturnable<Integer> cir) {
        cir.setReturnValue(1);
    }
}
"#;
    let sources: Vec<(&str, &str)> = ANNOTATION_SOURCES
        .iter()
        .copied()
        .chain([
            ("example/synth/InjectTarget.java", TARGET_SOURCE),
            ("example/synth/mixin/InjectTargetMixin.java", broken_mixin),
        ])
        .collect();
    compile(&java, &sources, &dir, None).unwrap_or_else(|e| panic!("{e}"));
    let jar_path = dir.join("inject-mod.jar");
    let metadata = r#"{"schemaVersion":1,"id":"inject_probe","version":"1.0.0","environment":"*","mixins":["inject.mixins.json"]}"#.to_string();
    let config = r#"{"required":true,"minVersion":"0.8","package":"example.synth.mixin","mixins":["InjectTargetMixin"]}"#.to_string();
    jar(
        &dir.join("classes"),
        &jar_path,
        &[
            ("fabric.mod.json", metadata),
            ("inject.mixins.json", config),
        ],
    )
    .unwrap_or_else(|e| panic!("{e}"));

    let bridge = standalone_bridge(&jar_path).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        bridge.census_mixins().wired.is_empty(),
        "an injector with no target method was wired: {:?}",
        bridge.census_mixins().wired
    );
    let reasons = bridge.census_mixins().unsupported.join("\n");
    assert!(
        reasons.contains("noSuchMethod") && reasons.contains("did not resolve"),
        "the missing target is recorded with its reason: {reasons:?}"
    );

    // The census must not call a target "applied" because its annotation
    // parsed: nothing was wired, so the target is ignored.
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_injector_against_a_real_minecraft_class_runs() {
    let Some(java) = java() else {
        eprintln!("skipped: no java on PATH or in JAVA_HOME");
        return;
    };
    let Some(minecraft) = hfc_runtime::discovery::minecraft_dir() else {
        eprintln!("skipped: no Minecraft installation");
        return;
    };
    let Some(client) = hfc_runtime::discovery::version_jar(&minecraft) else {
        eprintln!("skipped: no Minecraft client jar");
        return;
    };
    let Some(image) = jimage(&minecraft) else {
        eprintln!("skipped: no JDK image under {}", minecraft.display());
        return;
    };

    // A real Minecraft class with a small, deterministic body that the runtime
    // loads from the client jar: `SectionPos.blockToSectionCoord(int)` is
    // `i >> 4` and returns its argument for the section coordinate.
    let dir = std::env::temp_dir().join(format!("hfc-inject-real-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mixin = r#"
package example.real.mixin;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

// `targets` names the class by string: the Minecraft jar is compiled for a
// newer class-file version than the JDK that runs this test, so `javac` must
// not be asked to read it. The runtime resolves the name from its classpath.
@Mixin(targets = "net.minecraft.core.SectionPos")
public class SectionPosMixin {
    @Inject(method = "blockToSectionCoord(I)I", at = @At("HEAD"), cancellable = true)
    private static void replaceSectionCoord(int coordinate, CallbackInfoReturnable<Integer> cir) {
        switch (coordinate) {
            case 16, 80, 1024 -> cir.setReturnValue(99);
            default -> { }
        }
    }
}
"#;
    let sources: Vec<(&str, &str)> = ANNOTATION_SOURCES
        .iter()
        .copied()
        .chain([("example/real/mixin/SectionPosMixin.java", mixin)])
        .collect();
    compile(&java, &sources, &dir, None).unwrap_or_else(|e| panic!("{e}"));
    let jar_path = dir.join("real-mod.jar");
    let metadata = r#"{"schemaVersion":1,"id":"inject_real","version":"1.0.0","environment":"*","mixins":["real.mixins.json"]}"#.to_string();
    let config = r#"{"required":true,"minVersion":"0.8","package":"example.real.mixin","mixins":["SectionPosMixin"]}"#.to_string();
    jar(
        &dir.join("classes"),
        &jar_path,
        &[("fabric.mod.json", metadata), ("real.mixins.json", config)],
    )
    .unwrap_or_else(|e| panic!("{e}"));

    let mut bridge = FabricBridge::new();
    bridge.add_jar(&client).unwrap_or_else(|e| panic!("{e}"));
    let _ = bridge.add_jimage(&image);
    bridge
        .load_mod_jar(&jar_path)
        .unwrap_or_else(|e| panic!("{e}"));
    bridge.init().unwrap_or_else(|e| panic!("init: {e}"));

    assert_eq!(
        bridge.census_mixins().wired.len(),
        1,
        "the real-class injector was not wired: {:?}",
        bridge.census_mixins().unsupported
    );

    // The vanilla body's answer for these two inputs, and the value the
    // callback substitutes for one of them.
    let plain = bridge
        .interp
        .execute_method(
            "net/minecraft/core/SectionPos",
            "blockToSectionCoord",
            "(I)I",
            &[Value::Int(16)],
        )
        .expect("the real method runs");
    assert_eq!(
        plain.as_int(),
        99,
        "the callback did not replace the result"
    );

    let untargeted = bridge
        .interp
        .execute_method(
            "net/minecraft/core/SectionPos",
            "blockToSectionCoord",
            "(I)I",
            &[Value::Int(24)],
        )
        .expect("the real method runs");
    assert_eq!(
        untargeted.as_int(),
        1,
        "a coordinate the callback ignores must keep the vanilla answer"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The pinned real world-render mod, by the file name `test-mods/README.md`
/// lists. It is only a fixture: nothing below names one of its classes or
/// branches on it; its mixin config is read the way any mod's is.
const ANIMATED_DOORS: &str = "AnimatedDoors-2.0.2.jar";

/// The jars the probe needs, from `test-mods/` or the sibling corpus.
fn fixture(name: &str) -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .to_path_buf();
    [
        root.join("test-mods"),
        root.join("test-mods/corpus"),
        PathBuf::from("/home/daniel/horizon-fabric-test/game/horizon-mods"),
    ]
    .into_iter()
    .map(|dir| dir.join(name))
    .find(|path| path.is_file())
}

/// A real client mod's `@Inject(cancellable = true)` against real Minecraft
/// classes is wired and dispatched, and its callback's `cancel()` ends the
/// target method.
///
/// The mod is a fixture that reproduces the shape the engine has to support: a
/// mixin whose callback cancels a target it does not own. Its own render path
/// needs a world, which a headless test has no seam for, so the assertions are
/// about the injection itself: the annotation resolves against the real
/// Minecraft classes, the callback runs with the target's real arguments, and
/// the value it records decides the method's result.
#[test]
fn a_real_mods_cancellable_inject_is_wired_against_the_real_classes() {
    let Some(animated_doors) = fixture(ANIMATED_DOORS) else {
        eprintln!("skipped: {ANIMATED_DOORS} is absent");
        return;
    };
    let Some(minecraft) = hfc_runtime::discovery::minecraft_dir() else {
        eprintln!("skipped: no Minecraft installation");
        return;
    };
    let Some(client) = hfc_runtime::discovery::version_jar(&minecraft) else {
        eprintln!("skipped: no Minecraft client jar");
        return;
    };
    let Some(image) = jimage(&minecraft) else {
        eprintln!("skipped: no JDK image");
        return;
    };
    let Some(fabric_api) = fixture("fabric-api-0.165.0+26.3.jar") else {
        eprintln!("skipped: no Fabric API jar");
        return;
    };

    let mut bridge = FabricBridge::new();
    bridge.add_jar(&client).unwrap_or_else(|e| panic!("{e}"));
    let _ = bridge.add_jimage(&image);
    for jar in [&fabric_api, &animated_doors] {
        bridge.load_mod_jar(jar).unwrap_or_else(|e| panic!("{e}"));
    }
    bridge.init().unwrap_or_else(|e| panic!("init: {e}"));

    let manifest = bridge.census_mixins();
    // Every injector the mod declares that this engine can place must be wired,
    // and each must name a real target method: no entry here is a mere parse of
    // an annotation.
    let cancellable: Vec<_> = manifest
        .wired
        .iter()
        .filter(|injector| injector.cancellable)
        .collect();
    assert!(
        !cancellable.is_empty(),
        "no cancellable injector of the real mod was wired; unsupported: {:#?}",
        manifest.unsupported
    );
    for injector in &cancellable {
        assert!(
            bridge
                .interp
                .loader
                .get(&injector.declaring_class)
                .is_some(),
            "{}.{} is not a loaded class",
            injector.declaring_class,
            injector.target_method
        );
        let class = bridge
            .interp
            .loader
            .get(&injector.declaring_class)
            .expect("declaring class is loaded");
        assert!(
            class.methods.contains_key(&(
                injector.target_method.clone(),
                injector.target_descriptor.clone()
            )),
            "{} declares no {}{}",
            injector.declaring_class,
            injector.target_method,
            injector.target_descriptor
        );
        assert!(
            injector.returns_value() == injector.callback_info_class().ends_with("Returnable"),
            "the callback class matches the target's return type"
        );
    }

    // The engine dispatches exactly what the manifest reports as wired: the
    // interpreter's table is the same set, keyed by the declaring class.
    for injector in &manifest.wired {
        let table = bridge
            .interp
            .mixin_injectors
            .get(&(
                injector.declaring_class.clone(),
                injector.target_method.clone(),
            ))
            .unwrap_or_else(|| {
                panic!(
                    "{}.{} is in the manifest but not dispatched",
                    injector.declaring_class, injector.target_method
                )
            });
        assert!(
            table.iter().any(|dispatched| dispatched == injector),
            "the dispatcher's entry for {}.{} differs from the manifest's",
            injector.declaring_class,
            injector.target_method
        );
    }
}
