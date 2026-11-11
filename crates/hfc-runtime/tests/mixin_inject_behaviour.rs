//! The behavioural half of the `@Inject` proof, written against API that
//! existed before the injection engine did: a bridge, a mod jar, and
//! `Interpreter::execute_method`.
//!
//! It asserts only what a player would notice: a `@Inject(cancellable = true)`
//! callback that sets a return value makes the target method answer with it,
//! and the target's own body does not run. On the commit this capability was
//! added for, these assertions fail: the annotation parses, the callback is
//! recorded, and the method still returns its own value. That is the bug this
//! file pins.
//!
//! The jar is synthetic and compiled here, so the target's body and its witness
//! are known exactly. Nothing names a mod id or a mod class.

use hfc_runtime::bridge::FabricBridge;
use hfc_runtime::value::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET_CLASS: &str = "example/synth/InjectTarget";
const WITNESS: &str = "ran";

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

fn compile(java: &Path, sources: &[(&str, &str)], out: &Path) -> Result<(), String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let source_dir = out.join("src");
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
    let output = Command::new(java)
        .arg("-m")
        .arg("jdk.compiler/com.sun.tools.javac.Main")
        .arg("-d")
        .arg(&classes)
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

/// The witness the target writes when its body runs.
fn witness(bridge: &FabricBridge) -> i32 {
    bridge
        .interp
        .loader
        .get(TARGET_CLASS)
        .and_then(|class| class.static_fields.get(WITNESS).cloned())
        .map(|value| value.as_int())
        .unwrap_or(-1)
}

const ANNOTATIONS: &[(&str, &str)] = &[
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

#[test]
fn a_cancelling_callback_changes_what_the_target_method_returns() {
    let Some(java) = java() else {
        eprintln!("skipped: no java on PATH or in JAVA_HOME");
        return;
    };
    let dir = std::env::temp_dir().join(format!("hfc-inject-behaviour-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let sources: Vec<(&str, &str)> = ANNOTATIONS
        .iter()
        .copied()
        .chain([
            (
                "example/synth/InjectTarget.java",
                r#"
package example.synth;

public class InjectTarget {
    public static int ran = 0;

    public static int value() {
        ran = ran + 1;
        return 7;
    }

    public int scaled(int factor) {
        ran = ran + 1;
        return 7 * factor;
    }
}
"#,
            ),
            (
                "example/synth/mixin/InjectTargetMixin.java",
                r#"
package example.synth.mixin;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;

import example.synth.InjectTarget;

@Mixin(InjectTarget.class)
public class InjectTargetMixin {
    @Inject(method = "value", at = @At("HEAD"), cancellable = true)
    private static void replaceValue(CallbackInfoReturnable<Integer> cir) {
        cir.setReturnValue(9);
    }

    @Inject(method = "scaled", at = @At("HEAD"), cancellable = true)
    private void replaceScaled(int factor, CallbackInfoReturnable<Integer> cir) {
        cir.setReturnValue(40 + factor);
    }
}
"#,
            ),
        ])
        .collect();
    compile(&java, &sources, &dir).unwrap_or_else(|e| panic!("{e}"));
    let jar_path = dir.join("inject-mod.jar");
    jar(
        &dir.join("classes"),
        &jar_path,
        &[
            (
                "fabric.mod.json",
                r#"{"schemaVersion":1,"id":"inject_behaviour","version":"1.0.0","environment":"*","mixins":["inject.mixins.json"]}"#.to_string(),
            ),
            (
                "inject.mixins.json",
                r#"{"required":true,"minVersion":"0.8","package":"example.synth.mixin","mixins":["InjectTargetMixin"]}"#.to_string(),
            ),
        ],
    )
    .unwrap_or_else(|e| panic!("{e}"));

    let mut bridge = FabricBridge::new();
    bridge
        .load_mod_jar(&jar_path)
        .unwrap_or_else(|e| panic!("{e}"));
    bridge.init().unwrap_or_else(|e| panic!("init: {e}"));

    // The callback's 9, not the body's 7.
    let result = bridge
        .interp
        .execute_method(TARGET_CLASS, "value", "()I", &[])
        .expect("the injected static method runs");
    assert_eq!(
        result.as_int(),
        9,
        "the callback cancelled the method but its return value was not used"
    );
    assert_eq!(
        witness(&bridge),
        0,
        "the target's body ran even though the callback cancelled it"
    );

    // The same through an instance method with a parameter: it arrives in
    // order, and the body is skipped.
    let instance = bridge.interp.alloc_uninitialized(TARGET_CLASS);
    let result = bridge
        .interp
        .execute_method(
            TARGET_CLASS,
            "scaled",
            "(I)I",
            &[Value::Object(Some(instance)), Value::Int(3)],
        )
        .expect("the injected instance method runs");
    assert_eq!(
        result.as_int(),
        43,
        "the callback did not receive the target's real parameter"
    );
    assert_eq!(witness(&bridge), 0);

    let _ = std::fs::remove_dir_all(&dir);
}
