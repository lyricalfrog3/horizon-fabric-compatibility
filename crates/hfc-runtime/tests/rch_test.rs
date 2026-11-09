use hfc_runtime::minecraft::{BlockPos, BlockState, Direction, EventResult, Level, Player};
use hfc_runtime::value::Value;
use std::path::PathBuf;

mod common;

#[test]
fn test_rch_initialize() {
    // The mods' bytecode runs against the real Minecraft and JDK classes;
    // without an installation the test has nothing real to run on.
    let Some(mut bridge) = common::bridge_with_vanilla() else {
        eprintln!("skipping: no Minecraft installation");
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let jamlib_path = root.join("test-mods/jamlib-fabric-2.3.1+26.3.x.jar");
    let rch_path = root.join("test-mods/rightclickharvest-fabric-4.6.2+26.3.x.jar");

    // RightClickHarvest posts Fabric's PlayerBlockBreakEvents.BEFORE, whose
    // empty-listener result comes from Fabric API's own event factory.
    bridge
        .load_mod_jar(&root.join("test-mods/fabric-api-0.165.0+26.3.jar"))
        .expect("load fabric api");
    bridge.load_mod_jar(&jamlib_path).expect("load jamlib");
    bridge.load_mod_jar(&rch_path).expect("load rch");
    // `init` reads the mods' mixin configs (the accessor interfaces) and runs
    // their entrypoints, exactly as the client does.
    bridge.init().expect("mod initialization");

    // The mod's `@Invoker` interface is read from its mixin config, not hardcoded.
    let invoker = bridge
        .interp
        .mixin_accessors
        .values()
        .find(|accessor| accessor.interface.ends_with("/CropBlockAccessor"))
        .expect("the mod's invoker mixin is registered");
    assert_eq!(
        invoker.target_class,
        "net/minecraft/world/level/block/CropBlock"
    );
    assert_eq!(
        invoker.kind,
        hfc_runtime::mixin::AccessorKind::Invoker {
            method: "getAgeProperty".into()
        }
    );

    let interp = &mut bridge.interp;
    let manager = interp.resolve_static_field(
        "io/github/jamalam360/rightclickharvest/RightClickHarvest",
        "CONFIG",
    );
    let config = interp
        .execute_method(
            "io/github/jamalam360/jamlib/api/config/ConfigManager",
            "get",
            "()Ljava/lang/Object;",
            &[manager],
        )
        .expect("ConfigManager.get");
    // The mod's own constructor built it (through `Constructor.newInstance`),
    // with the defaults its field initializers declare.
    let config_id = config.as_obj().expect("a config object, not null");
    match interp.heap.get(config_id) {
        Some(hfc_runtime::value::JavaObject::Instance { class_name, fields }) => {
            assert_eq!(class_name, "io/github/jamalam360/rightclickharvest/Config");
            assert_eq!(fields.get("harvestInRadius"), Some(&Value::Int(1)));
            assert_eq!(fields.get("requireHoe"), Some(&Value::Int(0)));
        }
        other => panic!("unexpected config object: {other:?}"),
    }
}

#[test]
fn test_rch_harvest_crop_dynamically() {
    // The mods' bytecode runs against the real Minecraft and JDK classes;
    // without an installation the test has nothing real to run on.
    let Some(mut bridge) = common::bridge_with_vanilla() else {
        eprintln!("skipping: no Minecraft installation");
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let jamlib_path = root.join("test-mods/jamlib-fabric-2.3.1+26.3.x.jar");
    let rch_path = root.join("test-mods/rightclickharvest-fabric-4.6.2+26.3.x.jar");

    // RightClickHarvest posts Fabric's PlayerBlockBreakEvents.BEFORE, whose
    // empty-listener result comes from Fabric API's own event factory.
    bridge
        .load_mod_jar(&root.join("test-mods/fabric-api-0.165.0+26.3.jar"))
        .expect("load fabric api");
    bridge.load_mod_jar(&jamlib_path).expect("load jamlib");
    bridge.load_mod_jar(&rch_path).expect("load rch");
    // `init` reads the mods' mixin configs (the accessor interfaces) and runs
    // their entrypoints, exactly as the client does.
    bridge.init().expect("mod initialization");
    let interp = &mut bridge.interp;

    // Setup world with a mature wheat crop (age 7) at (10, 64, 10)
    let mut level = Level::new(false);
    let crop_pos = BlockPos {
        x: 10,
        y: 64,
        z: 10,
    };
    let mature_wheat = BlockState::new("minecraft:wheat").with_property("age", "7");
    level.set_block_and_update(&crop_pos, mature_wheat);

    let level_id = interp.heap.alloc_level(level);
    let player = Player::new(level_id);
    let player_id = interp.heap.alloc_player(player);

    let hit_pos = BlockPos {
        x: 10,
        y: 64,
        z: 10,
    };
    let hit_id = interp.heap.alloc_hit_result(hit_pos, Direction::UP);

    let main_hand = interp.resolve_static_field("net/minecraft/world/InteractionHand", "MAIN_HAND");

    let args = vec![
        Value::Object(Some(player_id)),
        Value::Object(Some(level_id)),
        main_hand,
        Value::Object(Some(hit_id)),
        Value::from_bool(true),
    ];

    // Call RightClickHarvest.onBlockUse(...) dynamically
    let res = interp.execute_method(
        "io/github/jamalam360/rightclickharvest/RightClickHarvest",
        "onBlockUse",
        "(Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/world/level/Level;Lnet/minecraft/world/InteractionHand;Lnet/minecraft/world/phys/BlockHitResult;Z)Lio/github/jamalam360/jamlib/api/events/core/EventResult;",
        &args,
    );

    assert!(res.is_ok(), "onBlockUse failed: {:?}", res.err());
    let ret_val = res.unwrap();
    println!("Return value: {:?}", ret_val);
    if let Some(res_id) = ret_val.as_obj() {
        println!("Object 33: {:?}", interp.heap.get(res_id));
        let event_res = interp
            .heap
            .get_native::<EventResult>(res_id)
            .expect("EventResult");
        assert!(
            event_res.cancelled,
            "event should be cancelled (handled/consumed)"
        );
    } else {
        panic!("expected EventResult object");
    }

    // Verify Level state: crop should be replanted to age 0, and items dropped!
    let final_level = interp.heap.get_native::<Level>(level_id).expect("Level");
    let final_crop_state = final_level.get_block_state(&crop_pos);
    assert_eq!(
        final_crop_state.get_age(),
        0,
        "crop should be replanted with age 0"
    );
    assert!(
        !final_level.dropped_stacks.is_empty(),
        "harvest drops should be spawned"
    );
    println!("Dropped stacks: {:?}", final_level.dropped_stacks);
    println!("Replanted state: {:?}", final_crop_state);
}
