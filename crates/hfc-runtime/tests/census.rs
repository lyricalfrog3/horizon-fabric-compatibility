//! A real-classpath smoke test of the compatibility census. It runs the
//! census's own library functions over `test-mods/`, so it needs the pinned
//! jars and a Minecraft installation; without either it returns early, like
//! the other real-classpath tests.

mod common;

use hfc_runtime::census::{self, Options, Report};
use std::path::PathBuf;

#[test]
fn the_census_records_entrypoints_events_and_json() {
    if common::minecraft_dir().is_none() {
        eprintln!("skipped: no Minecraft installation");
        return;
    }
    let mods = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("test-mods");
    if !mods.join("modmenu-21.0.0.jar").is_file() {
        eprintln!("skipped: test-mods/modmenu-21.0.0.jar is absent");
        return;
    }
    let out = std::env::temp_dir().join(format!("hfc-census-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let written = census::run(&Options {
        mods,
        out: Some(out.clone()),
        alone: true,
        ..Options::default()
    })
    .unwrap_or_else(|error| panic!("census failed: {error}"));

    assert!(written.json.is_file());
    assert!(written.markdown.is_file());
    let bytes = std::fs::read(&written.json).unwrap();
    let report: Report = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(report.version, census::SCHEMA_VERSION);

    let modmenu = report
        .mods
        .iter()
        .find(|m| m.id == "modmenu")
        .expect("modmenu is in the corpus");
    assert!(
        modmenu
            .entrypoints
            .iter()
            .any(|entry| entry.class.contains("modmenu")),
        "Mod Menu's entrypoints are recorded: {:?}",
        modmenu.entrypoints
    );

    // ItemTooltipCallback is dispatched; if a mod listens, the census must not
    // list it as undispatched.
    let events: Vec<_> = report
        .mods
        .iter()
        .flat_map(|m| m.events.iter())
        .chain(report.shared.events.iter())
        .collect();
    if let Some(event) = events
        .iter()
        .find(|event| event.event.ends_with("ItemTooltipCallback.EVENT"))
    {
        assert!(event.dispatched, "{event:?} is dispatched");
    }
    let _ = std::fs::remove_dir_all(&out);
}
