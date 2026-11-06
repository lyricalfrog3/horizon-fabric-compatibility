//! Shapes Minecraft keeps in maps keyed by a `Direction` or an enum must be
//! found again under the same key.
//!
//! Minecraft caches rotated shapes (`Shapes.rotateHorizontal`, and classes such
//! as `WallSignBlock` that build their own tables) in `java.util.EnumMap`s.
//! `EnumMap` reads an enum's constants through
//! `SharedSecrets.getJavaLangAccess()`, which on a JVM is filled in by the
//! bootstrap (`System.initPhase1`) and not by any class initializer. Without
//! it the map was built over no constants: `Map.get(Direction.NORTH)` answered
//! null for every facing, a face-keyed `getShape` returned null, and every
//! block whose shape comes from vanilla that way (signs, hanging signs, and
//! anything a mod builds on them) lost its shape.
//!
//! These run against the machine's real Minecraft classes; the expected boxes
//! come from the vanilla code (`javap`), not from the runtime.

use hfc_runtime::interp::Interpreter;
use hfc_runtime::value::{JavaObject, Value};

/// A `VoxelShape`'s boxes, through `toAabbs()`, as `[min_x, min_y, min_z,
/// max_x, max_y, max_z]`.
fn boxes(interp: &mut Interpreter, shape: &Value) -> Vec<[f64; 6]> {
    assert!(!shape.is_null(), "a shape the map answered is null");
    let list = interp
        .execute_method(
            "net/minecraft/world/phys/shapes/VoxelShape",
            "toAabbs",
            "()Ljava/util/List;",
            std::slice::from_ref(shape),
        )
        .expect("toAabbs");
    let size = interp
        .execute_method("java/util/List", "size", "()I", std::slice::from_ref(&list))
        .expect("size")
        .as_int();
    let mut out = Vec::new();
    for index in 0..size {
        let aabb = interp
            .execute_method(
                "java/util/List",
                "get",
                "(I)Ljava/lang/Object;",
                &[list.clone(), Value::Int(index)],
            )
            .expect("get");
        let fields = match aabb.as_obj().and_then(|id| interp.heap.get(id)) {
            Some(JavaObject::Instance { fields, .. }) => fields.clone(),
            other => panic!("an AABB is not an instance: {other:?}"),
        };
        let mut b = [0.0; 6];
        for (slot, name) in b
            .iter_mut()
            .zip(["minX", "minY", "minZ", "maxX", "maxY", "maxZ"])
        {
            *slot = fields
                .get(name)
                .unwrap_or_else(|| panic!("an AABB has no {name}"))
                .as_double();
        }
        out.push(b);
    }
    out
}

/// `net.minecraft.core.Direction.<name>`.
fn direction(interp: &mut Interpreter, name: &str) -> Value {
    interp
        .ensure_initialized("net/minecraft/core/Direction")
        .expect("Direction initializes");
    let value = interp.resolve_static_field("net/minecraft/core/Direction", name);
    assert!(!value.is_null(), "Direction.{name}");
    value
}

fn map_get(interp: &mut Interpreter, map: &Value, key: Value) -> Value {
    interp
        .execute_method(
            "java/util/Map",
            "get",
            "(Ljava/lang/Object;)Ljava/lang/Object;",
            &[map.clone(), key],
        )
        .expect("Map.get")
}

/// `WallSignBlock.SHAPES` is an `EnumMap<Direction, VoxelShape>` built by
/// `Shapes.rotateHorizontal` in the class's own static initializer.
///
/// javap: `Block.boxZ(16.0, 4.5, 12.5, 14.0, 16.0)` is the plate against the
/// north wall (x 0..1 in sixteenths halved around 8, y 4.5..12.5, z 14..16),
/// and `Shapes.rotateHorizontal` turns it a quarter at a time. So north has the
/// plate at z 0.875..1, east at x 0..0.125, south at z 0..0.125 and west at
/// x 0.875..1, all of them 0.125 thick and 4.5/16..12.5/16 tall.
#[test]
fn a_vanilla_direction_keyed_shape_map_answers_every_facing() {
    let Some(mut bridge) = hfc_runtime::discovery::bridge_with_vanilla() else {
        eprintln!("skipped: no Minecraft installation");
        return;
    };
    let interp = &mut bridge.interp;
    interp.step_budget = Some(100_000_000);
    interp
        .ensure_initialized("net/minecraft/world/level/block/WallSignBlock")
        .expect("WallSignBlock initializes");
    let shapes =
        interp.resolve_static_field("net/minecraft/world/level/block/WallSignBlock", "SHAPES");
    assert!(!shapes.is_null(), "WallSignBlock.SHAPES");

    for (facing, expect) in [
        ("NORTH", [0.0, 0.28125, 0.875, 1.0, 0.78125, 1.0]),
        ("EAST", [0.0, 0.28125, 0.0, 0.125, 0.78125, 1.0]),
        ("SOUTH", [0.0, 0.28125, 0.0, 1.0, 0.78125, 0.125]),
        ("WEST", [0.875, 0.28125, 0.0, 1.0, 0.78125, 1.0]),
    ] {
        let key = direction(interp, facing);
        let shape = map_get(interp, &shapes, key);
        assert!(
            !shape.is_null(),
            "{facing}: the map has no shape for the facing it was built with"
        );
        assert_eq!(boxes(interp, &shape), vec![expect], "{facing}");
    }
}
