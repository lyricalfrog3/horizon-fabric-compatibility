//! `@Invoker` / `@Accessor` mixin interfaces: a call naming the accessor
//! interface runs the target class's real member, exactly as Mixin's generated
//! bridge does.
//!
//! Mixin resolves each accessor against its target class when the target is
//! loaded, so the bridge reaches members the interface itself could not: private
//! and protected members, members declared by a superclass, and static members.
//! These tests build a jar Mixin would accept, load it through the ordinary mod
//! path (`load_mod_jar` + `init`), and make the calls a mod makes. The target
//! class carries no annotations; only the accessor interface names the members.
//! Expected values come from the compiled target class, never from the runtime
//! under test. The fixture needs only `javac`, not a Minecraft installation, so
//! this test binary does not use `tests/common`.

use hfc_runtime::bridge::FabricBridge;
use hfc_runtime::mixin::AccessorKind;
use hfc_runtime::value::Value;
use std::io::Write;
use std::path::PathBuf;

/// Stub Mixin annotations matching the real ones' retention and elements, so
/// the fixture compiles without the real Mixin jar. They are compiled to a
/// separate directory and never packed into the mod jar: only the bytes
/// `javac` emits into the accessor interface matter.
const ANNOTATIONS: &[(&str, &str)] = &[
    (
        "org/spongepowered/asm/mixin/Mixin.java",
        r#"
package org.spongepowered.asm.mixin;
import java.lang.annotation.*;
@Retention(RetentionPolicy.CLASS)
@Target(ElementType.TYPE)
public @interface Mixin {
    Class<?>[] value() default {};
    String[] targets() default {};
}
"#,
    ),
    (
        "org/spongepowered/asm/mixin/gen/Accessor.java",
        r#"
package org.spongepowered.asm.mixin.gen;
import java.lang.annotation.*;
@Retention(RetentionPolicy.RUNTIME)
@Target(ElementType.METHOD)
public @interface Accessor {
    String value() default "";
}
"#,
    ),
    (
        "org/spongepowered/asm/mixin/gen/Invoker.java",
        r#"
package org.spongepowered.asm.mixin.gen;
import java.lang.annotation.*;
@Retention(RetentionPolicy.RUNTIME)
@Target(ElementType.METHOD)
public @interface Invoker {
    String value() default "";
}
"#,
    ),
];

/// The target a mod's mixin names. Every member an accessor reaches here is one
/// the accessor interface itself cannot call.
const TARGET: &str = r#"
package hfc.fixture;

public class Target {
    private String secret = "initial";
    private static String staticSecret = "static-initial";
    private static int staticCounter;
    private int counter;
    protected String inherited = "base";

    protected String touch(String value) {
        this.secret = value;
        return "touched:" + value;
    }

    private int bump(int by) {
        this.counter += by;
        return this.counter;
    }

    private static String describe(String prefix) {
        return prefix + ":" + staticSecret;
    }

    private static String describeField() {
        return "field:" + staticSecret;
    }

    private static int scale(int value) {
        return value * 3;
    }
}
"#;

/// A subclass declaring none of the accessed members: they come from `Target`.
/// It does declare its own `bump` with the same signature, though: `Target`'s is
/// private and so not inherited, which is what lets Java accept this.
const SUBTARGET: &str = r#"
package hfc.fixture;

public class SubTarget extends Target {
    public int own;

    public int bump(int by) {
        this.own -= by;
        return this.own;
    }
}
"#;

const ACCESSOR: &str = r#"
package hfc.fixture.accessor;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.gen.Accessor;
import org.spongepowered.asm.mixin.gen.Invoker;

@Mixin(hfc.fixture.Target.class)
public interface TargetAccessor {
    @Accessor("secret")
    String getSecret();

    @Accessor("secret")
    void setSecret(String secret);

    // No `value`: Mixin inflects `inherited` from `getInherited`.
    @Accessor
    String getInherited();

    // Mixin requires an accessor's static-ness to match its target's. A Java
    // interface cannot declare a static method without a body, so a mod's
    // static accessors throw, exactly as Fabric API's own do; Mixin replaces
    // the body with the bridge.
    @Accessor("staticSecret")
    static String getStaticSecret() {
        throw new UnsupportedOperationException("implemented via mixin");
    }

    @Accessor("staticSecret")
    static void setStaticSecret(String value) {
        throw new UnsupportedOperationException("implemented via mixin");
    }

    @Accessor("staticCounter")
    static int getStaticCounter() {
        throw new UnsupportedOperationException("implemented via mixin");
    }

    @Accessor("staticCounter")
    static void setStaticCounter(int value) {
        throw new UnsupportedOperationException("implemented via mixin");
    }

    @Accessor("counter")
    int getCounter();

    // No `value`: Mixin inflects `touch` from `callTouch`.
    @Invoker
    String callTouch(String value);

    @Invoker("bump")
    int callBump(int by);

    // No `value`: Mixin inflects `describe` from `callDescribe`.
    @Invoker
    static String callDescribe(String prefix) {
        throw new UnsupportedOperationException("implemented via mixin");
    }

    // A static invoker with no parameters has no receiver at all.
    @Invoker("describeField")
    static String callDescribeField() {
        throw new UnsupportedOperationException("implemented via mixin");
    }

    // A static invoker whose first parameter is a primitive has no receiver
    // either, but does have arguments.
    @Invoker("scale")
    static int callScale(int value) {
        throw new UnsupportedOperationException("implemented via mixin");
    }

    // A target that does not exist. Mixin rejects this at load time with
    // `InvalidAccessorException`; a runtime that applied it must not answer a
    // default instead.
    @Accessor("noSuchField")
    String getNoSuchField();

    @Invoker("noSuchMethod")
    String callNoSuchMethod();
}
"#;

/// A jar holding the target classes and the accessor interface, with the mod
/// manifest and mixin config that make `load_mod_jar` read it.
struct Fixture {
    dir: PathBuf,
    jar: PathBuf,
}

impl Fixture {
    fn build() -> Option<Self> {
        // Every test builds its own fixture, in parallel: one directory each.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hfc-mixin-accessors-{}-{sequence}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let source = dir.join("src");
        let classes = dir.join("classes");
        let annotations = dir.join("annotations");
        for directory in [&source, &classes, &annotations] {
            std::fs::create_dir_all(directory).ok()?;
        }
        let mut sources = vec![
            ("hfc/fixture/Target.java", TARGET),
            ("hfc/fixture/SubTarget.java", SUBTARGET),
            ("hfc/fixture/accessor/TargetAccessor.java", ACCESSOR),
        ];
        sources.extend_from_slice(ANNOTATIONS);
        let mut files = Vec::new();
        for (path, text) in &sources {
            let file = source.join(path);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).ok()?;
            }
            std::fs::write(&file, text).ok()?;
            files.push(file);
        }
        let output = std::process::Command::new(java_tool())
            .arg("-m")
            .arg("jdk.compiler/com.sun.tools.javac.Main")
            .arg("-d")
            .arg(&classes)
            .args(&files)
            .output()
            .ok()?;
        if !output.status.success() {
            eprintln!(
                "skipping: javac failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return None;
        }
        // The stubs stay out of the jar: they are javac's input only, and a
        // real Minecraft classpath already has Mixin's own.
        std::fs::rename(classes.join("org"), annotations.join("org"))
            .ok()
            .or_else(|| {
                // `rename` of a directory into a missing parent fails; fall back to
                // copying the tree.
                copy_tree(&classes.join("org"), &annotations.join("org")).ok()
            })
            .or_else(|| {
                // Already moved by a previous attempt in the same process.
                annotations.join("org").is_dir().then_some(())
            })?;

        let jar = dir.join("fixture.jar");
        let file = std::fs::File::create(&jar).ok()?;
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        let mut add = |path: &str, bytes: &[u8]| -> Option<()> {
            zip.start_file(path, options).ok()?;
            zip.write_all(bytes).ok()?;
            Some(())
        };
        add(
            "fabric.mod.json",
            br#"{"schemaVersion":1,"id":"hfc_mixin_fixture","version":"1.0.0","name":"hfc mixin fixture","mixins":["hfc_fixture.mixins.json"]}"#,
        )?;
        add(
            "hfc_fixture.mixins.json",
            br#"{"required":true,"minVersion":"0.8","package":"hfc.fixture","mixins":["accessor.TargetAccessor"]}"#,
        )?;
        let mut stack = vec![classes.clone()];
        let mut packaged = Vec::new();
        while let Some(current) = stack.pop() {
            for entry in std::fs::read_dir(&current).ok()? {
                let path = entry.ok()?.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    packaged.push(path);
                }
            }
        }
        packaged.sort();
        for path in packaged {
            let name = path
                .strip_prefix(&classes)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            add(&name, &std::fs::read(&path).ok()?)?;
        }
        zip.finish().ok()?;
        Some(Self { dir, jar })
    }
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The `java` this machine has: `JAVA_HOME` first, as the JVM-differential
/// tests do, else `PATH`.
fn java_tool() -> String {
    std::env::var("JAVA_HOME")
        .map(|home| format!("{home}/bin/java"))
        .ok()
        .filter(|path| PathBuf::from(path).is_file())
        .unwrap_or_else(|| "java".into())
}

fn fixture_bridge() -> Option<(Fixture, FabricBridge)> {
    let fixture = Fixture::build()?;
    let mut bridge = FabricBridge::new();
    bridge.load_mod_jar(&fixture.jar).expect("load the fixture");
    bridge.init().expect("initialize the fixture");
    Some((fixture, bridge))
}

fn find(bridge: &FabricBridge, method: &str) -> hfc_runtime::mixin::MixinAccessor {
    bridge
        .interp
        .mixin_accessors
        .values()
        .find(|accessor| accessor.method == method)
        .unwrap_or_else(|| panic!("{method} is registered from the mod's mixin config"))
        .clone()
}

fn alloc_target(bridge: &mut FabricBridge, class: &str) -> Value {
    let id = bridge.interp.alloc_uninitialized(class);
    let this = Value::Object(Some(id));
    bridge
        .interp
        .execute_method(class, "<init>", "()V", std::slice::from_ref(&this))
        .expect("the target's constructor");
    this
}

fn string(bridge: &mut FabricBridge, text: &str) -> Value {
    Value::Object(Some(bridge.interp.heap.alloc_string(text)))
}

fn text(bridge: &FabricBridge, value: &Value) -> Option<String> {
    value
        .as_obj()
        .and_then(|id| bridge.interp.heap.get_string(id))
        .map(str::to_string)
}

/// Call an accessor method the way a mod's own bytecode does: an
/// `invokeinterface` on the accessor interface.
fn call(bridge: &mut FabricBridge, method: &str, args: &[Value]) -> Value {
    let accessor = find(bridge, method);
    let result = bridge
        .interp
        .execute_method(
            &accessor.interface,
            &accessor.method,
            &accessor.descriptor,
            args,
        )
        .unwrap_or_else(|error| panic!("{method}: {error}"));
    assert_eq!(
        bridge.interp.take_escaped_exception(),
        None,
        "{method} raised"
    );
    result
}

#[test]
fn invoker_reaches_a_private_target_and_its_effect_is_visible() {
    let Some((_fixture, mut bridge)) = fixture_bridge() else {
        return;
    };
    let target = alloc_target(&mut bridge, "hfc/fixture/Target");

    let accessor = find(&bridge, "callBump");
    assert_eq!(
        accessor.kind,
        AccessorKind::Invoker {
            method: "bump".into()
        }
    );
    // `bump` is private: dispatch through the receiver's class would not find
    // it, and Mixin's bridge uses `invokespecial` for exactly that reason.
    let result = call(&mut bridge, "callBump", &[target.clone(), Value::Int(4)]);
    assert_eq!(result.as_int(), 4, "the private target ran and returned");
    // Its effect is in the target's own field.
    let counter = call(&mut bridge, "getCounter", std::slice::from_ref(&target));
    assert_eq!(counter.as_int(), 4, "the private target's write is visible");
    let again = call(&mut bridge, "callBump", &[target.clone(), Value::Int(3)]);
    assert_eq!(again.as_int(), 7, "the same instance accumulated");
}

#[test]
fn invoker_reaches_a_protected_target_with_its_argument() {
    let Some((_fixture, mut bridge)) = fixture_bridge() else {
        return;
    };
    let target = alloc_target(&mut bridge, "hfc/fixture/Target");
    let value = string(&mut bridge, "changed");

    let result = call(&mut bridge, "callTouch", &[target.clone(), value]);
    assert_eq!(
        text(&bridge, &result).as_deref(),
        Some("touched:changed"),
        "the protected target received the argument"
    );
    let secret = call(&mut bridge, "getSecret", std::slice::from_ref(&target));
    assert_eq!(
        text(&bridge, &secret).as_deref(),
        Some("changed"),
        "the target's own field shows the write"
    );
}

#[test]
fn accessor_getter_and_setter_read_and_write_the_real_field() {
    let Some((_fixture, mut bridge)) = fixture_bridge() else {
        return;
    };
    let target = alloc_target(&mut bridge, "hfc/fixture/Target");

    let initial = call(&mut bridge, "getSecret", std::slice::from_ref(&target));
    assert_eq!(
        text(&bridge, &initial).as_deref(),
        Some("initial"),
        "the getter read the field's initial value"
    );
    let written = string(&mut bridge, "written");
    call(&mut bridge, "setSecret", &[target.clone(), written]);
    let read_back = call(&mut bridge, "getSecret", std::slice::from_ref(&target));
    assert_eq!(
        text(&bridge, &read_back).as_deref(),
        Some("written"),
        "the setter's write reads back"
    );
    // A second instance keeps its own value: the field is per-instance.
    let other = alloc_target(&mut bridge, "hfc/fixture/Target");
    let other_value = call(&mut bridge, "getSecret", std::slice::from_ref(&other));
    assert_eq!(text(&bridge, &other_value).as_deref(), Some("initial"));
}

#[test]
fn accessor_reaches_a_field_declared_by_a_superclass() {
    let Some((_fixture, mut bridge)) = fixture_bridge() else {
        return;
    };
    // The receiver's class declares none of the accessed members.
    let target = alloc_target(&mut bridge, "hfc/fixture/SubTarget");

    let accessor = find(&bridge, "getInherited");
    assert_eq!(
        accessor.kind,
        AccessorKind::Getter {
            field: "inherited".into()
        },
        "an unnamed @Accessor inflects its target from its own name"
    );
    let value = call(&mut bridge, "getInherited", std::slice::from_ref(&target));
    assert_eq!(
        text(&bridge, &value).as_deref(),
        Some("base"),
        "the getter found the superclass's field"
    );
}

#[test]
fn invoker_reaches_a_private_superclass_target_not_the_override() {
    let Some((_fixture, mut bridge)) = fixture_bridge() else {
        return;
    };
    // A virtual call from the receiver would run `SubTarget.bump`, which has
    // the same signature. `Target.bump` is private and not inherited, so
    // Mixin's bridge (`invokespecial` on the declaring class) runs `Target`'s:
    // the counter goes up and the return is the accumulated value.
    let target = alloc_target(&mut bridge, "hfc/fixture/SubTarget");
    let result = call(&mut bridge, "callBump", &[target.clone(), Value::Int(4)]);
    assert_eq!(
        result.as_int(),
        4,
        "the private superclass target ran, not the subclass's method"
    );
    let counter = call(&mut bridge, "getCounter", std::slice::from_ref(&target));
    assert_eq!(counter.as_int(), 4, "the private target's write is visible");
}

#[test]
fn static_invoker_runs_the_target_without_a_receiver() {
    let Some((_fixture, mut bridge)) = fixture_bridge() else {
        return;
    };
    // Mixin's bridge for a static target is static too: no receiver is passed.
    let prefix = string(&mut bridge, "prefix");
    let result = call(&mut bridge, "callDescribe", &[prefix]);
    assert_eq!(
        text(&bridge, &result).as_deref(),
        Some("prefix:static-initial"),
        "the static target ran and read the static field"
    );
}

#[test]
fn static_invoker_with_no_parameters_runs_without_a_receiver() {
    let Some((_fixture, mut bridge)) = fixture_bridge() else {
        return;
    };
    // No receiver and no arguments: the old redirect required a non-null first
    // argument, so this call fell through to a missing-method null.
    let result = call(&mut bridge, "callDescribeField", &[]);
    assert_eq!(
        text(&bridge, &result).as_deref(),
        Some("field:static-initial"),
        "a no-argument static invoker ran the real target"
    );
}

#[test]
fn static_invoker_whose_first_parameter_is_a_primitive_runs() {
    let Some((_fixture, mut bridge)) = fixture_bridge() else {
        return;
    };
    // The first argument is an `int`, not a receiver object: a redirect that
    // reads `args[0]` as the receiver takes the wrong path.
    let result = call(&mut bridge, "callScale", &[Value::Int(7)]);
    assert_eq!(result.as_int(), 21, "the static target ran: 7 * 3");
}

#[test]
fn static_accessor_getter_and_setter_use_the_class_field() {
    let Some((_fixture, mut bridge)) = fixture_bridge() else {
        return;
    };

    let initial = call(&mut bridge, "getStaticSecret", &[]);
    assert_eq!(
        text(&bridge, &initial).as_deref(),
        Some("static-initial"),
        "the static getter read the class field"
    );
    let written = string(&mut bridge, "static-written");
    call(&mut bridge, "setStaticSecret", &[written]);
    let read_back = call(&mut bridge, "getStaticSecret", &[]);
    assert_eq!(
        text(&bridge, &read_back).as_deref(),
        Some("static-written"),
        "the static setter's write reads back"
    );

    // The same storage `getstatic`/`putstatic` use: the static invoker, whose
    // target reads the field, sees the write.
    let prefix = string(&mut bridge, "p");
    let described = call(&mut bridge, "callDescribe", &[prefix]);
    assert_eq!(
        text(&bridge, &described).as_deref(),
        Some("p:static-written")
    );
}

#[test]
fn static_accessor_reads_and_writes_a_primitive() {
    let Some((_fixture, mut bridge)) = fixture_bridge() else {
        return;
    };

    let initial = call(&mut bridge, "getStaticCounter", &[]);
    assert_eq!(initial.as_int(), 0);
    call(&mut bridge, "setStaticCounter", &[Value::Int(41)]);
    let read_back = call(&mut bridge, "getStaticCounter", &[]);
    assert_eq!(read_back.as_int(), 41, "the static write reads back");
}

#[test]
fn an_accessor_whose_target_is_missing_fails_visibly() {
    let Some((_fixture, mut bridge)) = fixture_bridge() else {
        return;
    };
    let target = alloc_target(&mut bridge, "hfc/fixture/Target");

    // Mixin rejects these at load time with `InvalidAccessorException`. The
    // runtime applied them, so it must report the missing member rather than
    // answer a default. The call is an error the mod sees, not a null it can
    // keep using.
    let error = bridge
        .interp
        .execute_method(
            "hfc/fixture/accessor/TargetAccessor",
            "callNoSuchMethod",
            "()Ljava/lang/String;",
            std::slice::from_ref(&target),
        )
        .expect_err("a missing invoker target is an error");
    assert!(
        error.to_string().contains("noSuchMethod"),
        "the error names the member: {error}"
    );

    let error = bridge
        .interp
        .execute_method(
            "hfc/fixture/accessor/TargetAccessor",
            "getNoSuchField",
            "()Ljava/lang/String;",
            std::slice::from_ref(&target),
        )
        .expect_err("a missing accessor target is an error");
    assert!(
        error.to_string().contains("noSuchField"),
        "the error names the member: {error}"
    );
}
