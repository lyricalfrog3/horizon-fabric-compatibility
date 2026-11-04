use hfc_runtime::bridge::FabricBridge;
use hfc_runtime::value::{JavaObject, Value};

fn root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Mod Menu hides a jar-in-jar mod under the mod that ships it by asking Fabric
/// Loader for its containing mod; that answer has to come from the jars.
#[test]
fn a_jar_in_jar_mod_reports_the_mod_that_ships_it() {
    let mut bridge = FabricBridge::new();
    bridge
        .load_mod_jar(root().join("test-mods/fabric-api-0.165.0+26.3.jar"))
        .expect("load fabric api");
    let (nested, outer): (Vec<_>, Vec<_>) = bridge
        .loaded_mods()
        .iter()
        .cloned()
        .partition(|meta| meta.is_nested);
    let child = nested.first().expect("fabric-api ships nested mods");
    let parent = outer.first().expect("fabric-api itself is a top-level mod");

    let heap_str = |bridge: &mut FabricBridge, s: &str| {
        Value::Object(Some(bridge.interp.heap.alloc_string(s)))
    };
    let containing = |bridge: &mut FabricBridge, id: &str| -> Option<String> {
        let id_value = heap_str(bridge, id);
        let container = bridge
            .interp
            .execute_method(
                "net/fabricmc/loader/api/FabricLoader",
                "getModContainer",
                "(Ljava/lang/String;)Ljava/util/Optional;",
                &[Value::NULL, id_value],
            )
            .expect("getModContainer");
        let container = match bridge.interp.heap.get(container.as_obj()?) {
            Some(JavaObject::Instance { fields, .. }) => fields.get("value")?.clone(),
            _ => return None,
        };
        let optional = bridge
            .interp
            .execute_method(
                "net/fabricmc/loader/api/ModContainer",
                "getContainingMod",
                "()Ljava/util/Optional;",
                &[container],
            )
            .expect("getContainingMod");
        let parent_container = match bridge.interp.heap.get(optional.as_obj()?) {
            Some(JavaObject::Instance { fields, .. }) => fields.get("value")?.clone(),
            _ => return None,
        };
        let metadata = match bridge.interp.heap.get(parent_container.as_obj()?) {
            Some(JavaObject::Instance { fields, .. }) => fields.get("metadata")?.clone(),
            _ => return None,
        };
        let id = match bridge.interp.heap.get(metadata.as_obj()?) {
            Some(JavaObject::Instance { fields, .. }) => fields.get("id")?.clone(),
            _ => return None,
        };
        bridge
            .interp
            .heap
            .get_string(id.as_obj()?)
            .map(str::to_string)
    };

    assert_eq!(
        containing(&mut bridge, &child.id).as_deref(),
        Some(parent.id.as_str()),
        "{} is nested in {}",
        child.id,
        parent.id
    );
    assert_eq!(containing(&mut bridge, &parent.id), None);
}
