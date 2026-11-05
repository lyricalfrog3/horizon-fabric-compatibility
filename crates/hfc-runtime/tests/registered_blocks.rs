//! Blocks a mod registers in code reach the host as facts read from the
//! mod's real bytecode (Block constructor, StateDefinition, shape methods).
//!
//! Expected values come from `javap` of the mod's classes (property
//! declarations, constructor arguments, the `makeShapes` lambdas), never from
//! the code under test. Pixel boxes are divided by 16 as `Block.box` does.
//!
//! Needs a Minecraft install and `test-mods/` jars; otherwise it skips.
//! Init costs about 46M interpreter instructions, so run it with `--release`
//! when you can (about 6 s; the debug profile needs about a minute).

mod common;

use hfc_runtime::bridge::{FabricBridge, RegisteredBlock};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf()
}

fn mdm() -> Option<FabricBridge> {
    let jars = ["fabric-api-0.165.0+26.3.jar", "mdm-3.1-fabric-26.3.jar"]
        .map(|name| root().join("test-mods").join(name));
    if jars.iter().any(|jar| !jar.exists()) {
        eprintln!("skipped: test-mods jars are not present");
        return None;
    }
    let mut bridge = common::bridge_with_vanilla()?;
    for jar in &jars {
        bridge.load_mod_jar(jar).unwrap();
    }
    bridge.init().unwrap();
    Some(bridge)
}

fn find<'a>(blocks: &'a [RegisteredBlock], id: &str) -> &'a RegisteredBlock {
    blocks
        .iter()
        .find(|b| b.id == id)
        .unwrap_or_else(|| panic!("{id} not registered"))
}

fn chain(block: &RegisteredBlock) -> Vec<&str> {
    block.class.iter().map(String::as_str).collect()
}

fn values(block: &RegisteredBlock, name: &str) -> Vec<String> {
    block.properties[name].clone()
}

const MOD: &str = "com.cookiecraftmods.mdm.block.";
const BLOCK: &str = "net.minecraft.world.level.block.Block";
const BEHAVIOUR: &str = "net.minecraft.world.level.block.state.BlockBehaviour";

#[test]
fn mdm_registers_344_blocks_with_real_facts() {
    let Some(mut bridge) = mdm() else { return };
    let blocks = bridge.registered_blocks();

    assert_eq!(blocks.len(), 344);
    // Every fact was readable.
    for b in &blocks {
        assert!(b.unreadable.is_empty(), "{}: {:?}", b.id, b.unreadable);
        assert!(b.states.contains_key(&b.default), "{} default", b.id);
    }
    // Only the multipart helper has no block entity and no item.
    let plain: Vec<&str> = blocks
        .iter()
        .filter(|b| !b.has_block_entity)
        .map(|b| b.id.as_str())
        .collect();
    assert_eq!(plain, ["mdm:furniture_part"]);
    assert_eq!(find(&blocks, "mdm:furniture_part").item, None);
    assert_eq!(blocks.iter().filter(|b| b.item.is_some()).count(), 343);

    // Storage block: extends Block directly, one `facing` property.
    let cabinet = find(&blocks, "mdm:set_1_cabinet");
    assert_eq!(
        chain(cabinet),
        [&*format!("{MOD}Set1CabinetBlock"), BLOCK, BEHAVIOUR]
    );
    assert_eq!(
        values(cabinet, "facing"),
        ["north", "south", "west", "east"]
    );
    assert_eq!(cabinet.properties.len(), 1);
    assert_eq!(cabinet.default, "facing=north");
    assert_eq!(cabinet.states.len(), 4);
    assert_eq!(cabinet.states[&cabinet.default].hardness, 1.0);
    assert_eq!(cabinet.explosion_resistance, 10.0);
    assert_eq!(cabinet.item.as_deref(), Some("mdm:set_1_cabinet"));
    assert!(cabinet.has_block_entity);
    // javap of Set1CabinetBlock: it declares getShape (BlockBehaviour's),
    // getStateForPlacement and createBlockStateDefinition (Block's) and
    // useWithoutItem (BlockBehaviour's); makeShapes is private and its own,
    // and the constructor is no override.
    let has = |b: &RegisteredBlock, m: &str| b.overrides.iter().any(|o| o == m);
    for m in [
        format!("{BEHAVIOUR}.getShape"),
        format!("{BEHAVIOUR}.useWithoutItem"),
        format!("{BLOCK}.getStateForPlacement"),
        format!("{BLOCK}.createBlockStateDefinition"),
    ] {
        assert!(has(cabinet, &m), "{m} in {:?}", cabinet.overrides);
    }
    assert!(
        cabinet
            .overrides
            .iter()
            .all(|o| o.starts_with("net.minecraft.")
                && !o.contains("makeShapes")
                && !o.contains('<')),
        "{:?}",
        cabinet.overrides
    );

    // Bed: three properties; PART defaults to head (the mod re-sets it).
    let bed = find(&blocks, "mdm:bedroom_set_1grey_single_bed");
    let chain_bed = chain(bed);
    assert!(chain_bed.contains(&"net.minecraft.world.level.block.BedBlock"));
    assert!(chain_bed.contains(&"net.minecraft.world.level.block.HorizontalDirectionalBlock"));
    assert_eq!(chain_bed.last(), Some(&BEHAVIOUR));
    assert_eq!(bed.properties.len(), 3);
    assert_eq!(values(bed, "occupied"), ["true", "false"]);
    assert_eq!(values(bed, "part"), ["head", "foot"]);
    assert_eq!(bed.default, "facing=north,occupied=false,part=head");
    assert_eq!(bed.states.len(), 16);
    assert_eq!(bed.states[&bed.default].hardness, 1.0);
    // javap of SingleBedBlock: it replaces the bed's placement (setPlacedBy
    // places the mod's own parts, not vanilla's head half), which 26.3's
    // AbstractBedBlock declares, and the render shape BlockBehaviour declares.
    for m in [
        "net.minecraft.world.level.block.AbstractBedBlock.setPlacedBy",
        "net.minecraft.world.level.block.AbstractBedBlock.getStateForPlacement",
        "net.minecraft.world.level.block.AbstractBedBlock.updateShape",
        "net.minecraft.world.level.block.state.BlockBehaviour.getRenderShape",
    ] {
        assert!(has(bed, m), "{m} in {:?}", bed.overrides);
    }
    assert_eq!(
        blocks
            .iter()
            .filter(|b| b
                .class
                .iter()
                .any(|c| c == "net.minecraft.world.level.block.BedBlock"))
            .count(),
        48
    );

    // Facing furniture through two mod base classes.
    let fridge = find(&blocks, "mdm:black_modern_fridge_1");
    assert_eq!(fridge.class.len(), 5);
    assert_eq!(fridge.class[0], format!("{MOD}BlackModernFridge1Block"));
    assert_eq!(fridge.default, "facing=north");
    assert_eq!(fridge.states.len(), 4);
    assert_eq!(fridge.states[&fridge.default].hardness, 1.0);

    // Integer and boolean properties: 9x9x9x2x2 states.
    let part = find(&blocks, "mdm:furniture_part");
    assert_eq!(part.states.len(), 2916);
    assert_eq!(
        part.default,
        "extended=false,offset_x=1,offset_y=1,offset_z=1,waterlogged=false"
    );
    let nine: Vec<String> = (0..9).map(|n| n.to_string()).collect();
    assert_eq!(values(part, "offset_x"), nine);
    assert_eq!(2916, 9 * 9 * 9 * 2 * 2);

    // Shapes are not read by init.
    assert!(cabinet.states[&cabinet.default].outline.is_none());
    bridge.read_block_shapes(&["mdm:set_1_cabinet"]);
    let blocks = bridge.registered_blocks();
    let cabinet = find(&blocks, "mdm:set_1_cabinet");
    assert!(cabinet.unreadable.is_empty(), "{:?}", cabinet.unreadable);
    // javap of Set1CabinetBlock.lambda$makeShapes$0: box(0,0,5,16,16,16) for
    // north, (0,0,0,11,16,16) east, (5,0,0,16,16,16) west, (0,0,0,16,16,11)
    // south.
    let expect = [
        ("facing=north", [0.0, 0.0, 0.3125, 1.0, 1.0, 1.0]),
        ("facing=east", [0.0, 0.0, 0.0, 0.6875, 1.0, 1.0]),
        ("facing=west", [0.3125, 0.0, 0.0, 1.0, 1.0, 1.0]),
        ("facing=south", [0.0, 0.0, 0.0, 1.0, 1.0, 0.6875]),
    ];
    for (key, boxes) in expect {
        let state = &cabinet.states[key];
        assert_eq!(state.outline.as_deref(), Some(&[boxes][..]), "{key}");
        assert_eq!(state.collision.as_deref(), Some(&[boxes][..]), "{key}");
    }
}

/// A world of grass at y=0 and air above; records nothing.
struct Meadow;

impl hfc_runtime::bridge::HostBridge for Meadow {
    fn block(&self, pos: [i32; 3]) -> String {
        if pos[1] <= 0 {
            "minecraft:grass_block".into()
        } else {
            "minecraft:air".into()
        }
    }
    fn block_property(&self, _: [i32; 3], _: &str) -> Option<String> {
        None
    }
    fn set_block_property(&mut self, _: [i32; 3], _: &str, _: &str) -> bool {
        false
    }
    fn give(&mut self, _: &str, _: u32) -> Result<(), String> {
        Ok(())
    }
}

/// A click on the top of the grass at (4, 0, 4), the block going to (4, 1, 4).
fn click(block: &str, yaw: f32, pitch: f32) -> hfc_runtime::bridge::PlacementRequest {
    hfc_runtime::bridge::PlacementRequest {
        block: block.into(),
        pos: [4, 1, 4],
        hit_pos: [4, 0, 4],
        face: [0, 1, 0],
        hit: [4.5, 1.0, 4.5],
        inside: false,
        yaw,
        pitch,
        player_pos: [4.5, 1.0, 7.5],
        sneaking: false,
        off_hand: false,
        item: block.into(),
        replace_clicked: false,
    }
}

fn facing(placement: hfc_runtime::bridge::Placement) -> String {
    match placement {
        hfc_runtime::bridge::Placement::State(state) => state["facing"].clone(),
        other => panic!("not a state: {other:?}"),
    }
}

#[test]
fn a_cabinet_faces_the_player_it_is_placed_by() {
    let Some(mut bridge) = mdm() else { return };
    // javap of Set1CabinetBlock.getStateForPlacement: Block's own state with
    // FACING set to context.getHorizontalDirection().getOpposite(). The
    // horizontal direction is Direction.fromYRot(yaw): 0 looks south, 90
    // west, 180 north and 270 east.
    for (yaw, looking, placed) in [
        (0.0, "south", "north"),
        (90.0, "west", "east"),
        (180.0, "north", "south"),
        (270.0, "east", "west"),
        (-90.0, "east", "west"),
        (44.0, "south", "north"),
        (46.0, "west", "east"),
    ] {
        let got =
            facing(bridge.state_for_placement(&Meadow, &click("mdm:set_1_cabinet", yaw, 10.0)));
        assert_eq!(got, placed, "looking {looking} (yaw {yaw})");
    }
    // The pitch does not matter to a horizontal-facing block.
    assert_eq!(
        facing(bridge.state_for_placement(&Meadow, &click("mdm:set_1_cabinet", 90.0, -80.0))),
        "east"
    );
}

#[test]
fn a_block_that_does_not_override_placement_is_left_to_the_host() {
    use hfc_runtime::bridge::Placement;
    let Some(mut bridge) = mdm() else { return };
    let blocks = bridge.registered_blocks();
    let plain = blocks
        .iter()
        .find(|b| {
            !b.overrides
                .iter()
                .any(|o| o.ends_with(".getStateForPlacement"))
        })
        .expect("some mdm block keeps vanilla placement");
    assert_eq!(
        bridge.state_for_placement(&Meadow, &click(&plain.id.clone(), 0.0, 0.0)),
        Placement::NotOverridden
    );
    assert_eq!(
        bridge.state_for_placement(&Meadow, &click("minecraft:stone", 0.0, 0.0)),
        Placement::NotOverridden
    );
}

/// The meadow with one stone block standing at `stone`.
struct Meadow1 {
    stone: [i32; 3],
}

impl hfc_runtime::bridge::HostBridge for Meadow1 {
    fn block(&self, pos: [i32; 3]) -> String {
        if pos == self.stone {
            "minecraft:stone".into()
        } else {
            Meadow.block(pos)
        }
    }
    fn block_property(&self, _: [i32; 3], _: &str) -> Option<String> {
        None
    }
    fn set_block_property(&mut self, _: [i32; 3], _: &str, _: &str) -> bool {
        false
    }
    fn give(&mut self, _: &str, _: u32) -> Result<(), String> {
        Ok(())
    }
}

const BED: &str = "mdm:bedroom_set_1grey_single_bed";

#[test]
fn a_bed_takes_its_place_and_sets_its_other_cells() {
    use hfc_runtime::bridge::{PlacedBy, Placement};
    let Some(mut bridge) = mdm() else { return };
    // javap: BedroomSet1greySingleBedBlock.getStateForPlacement is
    // SingleBedBlock's (the default state), FACING = horizontal direction
    // opposite, then FurnitureFootprint.placement, which refuses (null) when a
    // cell of the footprint is not replaceable.
    let request = click(BED, 0.0, 0.0);
    let Placement::State(state) = bridge.state_for_placement(&Meadow, &request) else {
        panic!("the bed was not placed");
    };
    assert_eq!(state["facing"], "north");
    assert_eq!(state["part"], "head");

    // SingleBedBlock.setPlacedBy is FurnitureFootprint.placeParts: one
    // `furniture_part` per other cell of the shape, from
    // FurniturePartBlock.forOffset(offset, state): EXTENDED true, X/Y/Z =
    // offset + 4. A north-facing bed at (4,1,4) has its other cell at
    // (4,1,3), offset (0,0,-1).
    let PlacedBy::Changes(changes) = bridge.block_placed(&Meadow, &request, &state) else {
        panic!("the bed set nothing");
    };
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].pos, [4, 1, 3]);
    assert_eq!(changes[0].block, "mdm:furniture_part");
    let expect = [
        ("extended", "true"),
        ("offset_x", "4"),
        ("offset_y", "4"),
        ("offset_z", "3"),
        ("waterlogged", "false"),
    ];
    assert_eq!(
        changes[0]
            .properties
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>(),
        expect
    );

    // Turned the other way, the other cell is on the other side.
    let request = click(BED, 180.0, 0.0);
    let Placement::State(state) = bridge.state_for_placement(&Meadow, &request) else {
        panic!("the bed was not placed");
    };
    assert_eq!(state["facing"], "south");
    let PlacedBy::Changes(changes) = bridge.block_placed(&Meadow, &request, &state) else {
        panic!("the bed set nothing");
    };
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].pos, [4, 1, 5]);
    assert_eq!(changes[0].properties["offset_z"], "5");
}

#[test]
fn a_bed_is_refused_where_the_host_has_a_block_in_its_way() {
    use hfc_runtime::bridge::Placement;
    let Some(mut bridge) = mdm() else { return };
    // The other cell of a north-facing bed is (4,1,3): stone there is read from
    // the host's world, and the mod's own code says no.
    let blocked = Meadow1 { stone: [4, 1, 3] };
    assert_eq!(
        bridge.state_for_placement(&blocked, &click(BED, 0.0, 0.0)),
        Placement::Refused
    );
    // Facing south it does not reach the stone.
    assert!(matches!(
        bridge.state_for_placement(&blocked, &click(BED, 180.0, 0.0)),
        Placement::State(_)
    ));
}

#[test]
fn mdm_storage_blocks_report_the_container_their_block_entity_builds() {
    use hfc_runtime::bridge::{BlockContainer, ContainerRead};
    let Some(bridge) = mdm() else { return };
    let blocks = bridge.registered_blocks();

    // javap of Set1CabinetBlockEntity: it extends RandomizableContainerBlockEntity,
    // its constructor makes `NonNullList.withSize(27, EMPTY)`, `getContainerSize()`
    // is that list's size, `createMenu(int, Inventory)` is `ChestMenu.threeRows`
    // (MenuType.GENERIC_9x3) and `getDisplayName()` is `Component.literal("Storage")`.
    assert_eq!(
        find(&blocks, "mdm:set_1_cabinet").container,
        ContainerRead::Container(BlockContainer {
            slots: 27,
            menu_type: "minecraft:generic_9x3".into(),
            title: Some("Storage".into()),
        })
    );

    // The multipart helper has no block entity at all.
    assert_eq!(
        find(&blocks, "mdm:furniture_part").container,
        ContainerRead::NoBlockEntity
    );

    let mut by_kind: std::collections::BTreeMap<String, usize> = Default::default();
    for b in &blocks {
        let key = match &b.container {
            ContainerRead::Container(c) => format!("{} x{}", c.menu_type, c.slots),
            ContainerRead::NoBlockEntity => "no block entity".into(),
            ContainerRead::NotVanillaContainer(class) => format!("not a container: {class}"),
            ContainerRead::Unreadable(why) => format!("unreadable: {why}"),
        };
        *by_kind.entry(key).or_default() += 1;
    }
    eprintln!("container counts: {by_kind:#?}");
}
