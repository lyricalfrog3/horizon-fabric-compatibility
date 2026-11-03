//! Probe: does real library / Minecraft bytecode run on the interpreter?

use hfc_runtime::bridge::FabricBridge;
use hfc_runtime::value::{JavaObject, Value};
use std::collections::HashMap;
use std::path::PathBuf;

fn libs() -> PathBuf {
    PathBuf::from("/home/daniel/.local/share/horizon-launcher/minecraft")
}

#[test]
fn probe_real_bytecode() {
    let mut b = FabricBridge::new();
    b.interp.trace_missing = true;
    for rel in [
        "libraries/com/google/guava/guava/33.6.0-jre/guava-33.6.0-jre.jar",
        "libraries/com/google/code/gson/gson/2.14.0/gson-2.14.0.jar",
        "libraries/it/unimi/dsi/fastutil/8.5.18/fastutil-8.5.18.jar",
        "versions/26.3/26.3.jar",
    ] {
        b.add_jar(libs().join(rel)).unwrap();
    }
    b.interp
        .loader
        .add_jimage(
            "/home/daniel/.local/share/horizon-launcher/minecraft/runtime/java-runtime-epsilon/linux/java-runtime-epsilon/lib/modules",
        )
        .unwrap();

    // 1. Guava: Ordering.natural().compare(a, b)
    b.interp.missing.clear();
    let natural = b.interp.execute_method(
        "com/google/common/collect/Ordering",
        "natural",
        "()Lcom/google/common/collect/Ordering;",
        &[],
    );
    println!("Ordering.natural() = {natural:?}");
    println!("  missing: {:?}", b.interp.missing);

    if let Ok(Value::Object(Some(ord))) = natural {
        b.interp.missing.clear();
        let r = b.interp.execute_method(
            "com/google/common/collect/Ordering",
            "compare",
            "(Ljava/lang/Object;Ljava/lang/Object;)I",
            &[Value::Object(Some(ord)), Value::Int(1), Value::Int(2)],
        );
        println!("compare(1,2) = {r:?}  missing {:?}", b.interp.missing);
    }

    // 2. Minecraft TextColor.fromRgb
    b.interp.missing.clear();
    let tc = b.interp.execute_method(
        "net/minecraft/network/chat/TextColor",
        "fromRgb",
        "(I)Lnet/minecraft/network/chat/TextColor;",
        &[Value::Int(0x12_34_56)],
    );
    println!("TextColor.fromRgb = {tc:?}");
    println!("  missing: {:?}", b.interp.missing);

    if let Ok(Value::Object(Some(id))) = tc {
        println!("  heap: {:?}", b.interp.heap.get(id));
        let v = b.interp.execute_method(
            "net/minecraft/network/chat/TextColor",
            "getValue",
            "()I",
            &[Value::Object(Some(id))],
        );
        println!("  getValue = {v:?}");
        b.interp.missing.clear();
        b.interp.step_budget = Some(100_000);
        let ser = b.interp.execute_method(
            "net/minecraft/network/chat/TextColor",
            "serialize",
            "()Ljava/lang/String;",
            &[Value::Object(Some(id))],
        );
        b.interp.step_budget = None;
        println!("  serialize = {ser:?}");
        println!("    missing {:?}", b.interp.missing);
    }

    // 3. Gson JsonObject
    b.interp.missing.clear();
    let jo = b.interp.heap.alloc(JavaObject::Instance {
        class_name: "com/google/gson/JsonObject".into(),
        fields: HashMap::new(),
    });
    let init = b.interp.execute_method(
        "com/google/gson/JsonObject",
        "<init>",
        "()V",
        &[Value::Object(Some(jo))],
    );
    println!(
        "JsonObject.<init> = {init:?} missing {:?}",
        b.interp.missing
    );
    b.interp.missing.clear();
    let key_a = b.interp.heap.alloc_string("a");
    let key_b = b.interp.heap.alloc_string("b");
    let add = b.interp.execute_method(
        "com/google/gson/JsonObject",
        "addProperty",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        &[
            Value::Object(Some(jo)),
            Value::Object(Some(key_a)),
            Value::Object(Some(key_b)),
        ],
    );
    println!("addProperty = {add:?} missing {:?}", b.interp.missing);
    b.interp.missing.clear();
    let ts = b.interp.execute_method(
        "com/google/gson/JsonObject",
        "toString",
        "()Ljava/lang/String;",
        &[Value::Object(Some(jo))],
    );
    println!("toString = {ts:?} missing {:?}", b.interp.missing);
}
