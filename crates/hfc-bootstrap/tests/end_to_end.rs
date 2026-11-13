//! End-to-end runs against synthetic jars: discovery, carry, install, cache
//! reuse, invalidation and cleanup. No real mod jar is redistributed.

use hfc_bootstrap::{Bootstrap, CacheState, ModStatus, RunOptions};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "hfc-e2e-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("horizon-mods")).unwrap();
    fs::create_dir_all(dir.join("horizon-mods/HorizonFabricCompat")).unwrap();
    dir
}

fn jar(path: &Path, entries: &[(&str, &[u8])]) {
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, bytes) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
}

fn content_jar(path: &Path, extra_lang: bool) {
    named_content_jar(path, "ruby", extra_lang);
}

fn named_content_jar_with_marker(path: &Path, id: &str, marker: &str) {
    named_content_jar_inner(path, id, false, Some(marker));
}

fn named_content_jar(path: &Path, id: &str, extra_lang: bool) {
    named_content_jar_inner(path, id, extra_lang, None);
}

fn named_content_jar_inner(path: &Path, id: &str, extra_lang: bool, marker: Option<&str>) {
    let metadata = format!(
        r#"{{
  "schemaVersion": 1,
  "id": "{id}",
  "version": "1.0.0",
  "name": "{id}",
  "license": "MIT",
  "environment": "*",
  "depends": {{"minecraft": ">=1.21", "fabricloader": ">=0.15"}},
  "entrypoints": {{"main": ["{id}.Mod"]}}
}}"#
    );
    let blockstate = format!(r#"{{"variants":{{"":{{"model":"{id}:block/{id}_block"}}}}}}"#);
    let model = format!(
        r#"{{"parent":"minecraft:block/cube_all","textures":{{"all":"{id}:block/{id}_block"}}}}"#
    );
    let item_model = format!(r#"{{"parent":"{id}:block/{id}_block"}}"#);
    let tag = format!(r#"{{"values":["{id}:{id}_block"]}}"#);
    let mut entries: Vec<(String, Vec<u8>)> = vec![
        ("fabric.mod.json".into(), metadata.into_bytes()),
        (
            format!("assets/{id}/blockstates/{id}_block.json"),
            blockstate.into_bytes(),
        ),
        (
            format!("assets/{id}/models/block/{id}_block.json"),
            model.into_bytes(),
        ),
        (
            format!("assets/{id}/models/item/{id}_block.json"),
            item_model.into_bytes(),
        ),
        (
            format!("assets/{id}/textures/block/{id}_block.png"),
            b"\x89PNG\r\n\x1a\nnot-really-a-png".to_vec(),
        ),
        (
            format!("data/{id}/tags/block/{id}_blocks.json"),
            tag.into_bytes(),
        ),
    ];
    if extra_lang {
        entries.push((
            format!("assets/{id}/lang/en_us.json"),
            format!(r#"{{"block.{id}.{id}_block":"{id} Block"}}"#).into_bytes(),
        ));
    }
    if let Some(marker) = marker {
        // A per-jar asset: its presence in the installed folder proves which
        // of two same-id jars was converted.
        entries.push((
            format!("assets/{id}/textures/block/from_{marker}.png"),
            format!("from {marker}").into_bytes(),
        ));
    }
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    jar(path, &refs);
}

fn open(mods: &Path) -> Bootstrap {
    Bootstrap::open(mods).unwrap().unwrap()
}

/// Read an installed folder with Horizon's own loader: the manifest and every
/// content declaration the game will read. This is the integration check that
/// HFC's output is really a content mod, not just a directory.
fn assert_horizon_loads(mods: &Path, id: &str) {
    let folder = mods.join(id);
    let package = horizon_mods::Package::read(&folder)
        .unwrap_or_else(|e| panic!("Horizon rejected {id}: {e}"));
    assert_eq!(package.manifest.id, id);
    assert!(
        package
            .manifest
            .capabilities
            .contains(&horizon_mods::Capability::Registries),
        "{id} is not a registries content mod"
    );
    assert!(package.wasm.is_none(), "{id} unexpectedly ships code");
    let content = horizon_mods::Content::read(&folder, &package.manifest)
        .unwrap_or_else(|e| panic!("Horizon rejected {id} content: {e}"));
    assert!(
        content
            .blocks
            .iter()
            .any(|b| b.id == format!("{id}:{id}_block")),
        "{id}: Horizon did not read its block: {:#?}",
        content.blocks
    );
    assert!(
        content.files.iter().any(|f| f.starts_with("assets/")),
        "{id}: no assets were loaded"
    );
}

#[test]
fn installs_content_reuses_cache_and_leaves_originals_alone() {
    let root = temp("happy");
    let mods = root.join("horizon-mods");
    fs::write(
        mods.join("HorizonFabricCompat/hfc.toml"),
        hfc_bootstrap::Config::template(),
    )
    .unwrap();
    let source = mods.join("ruby.jar");
    content_jar(&source, false);
    let original = fs::read(&source).unwrap();

    let report = open(&mods).run(RunOptions::default()).unwrap();
    let ruby = report
        .mods
        .iter()
        .find(|m| m.id.as_deref() == Some("ruby"))
        .unwrap();
    assert_eq!(ruby.status, ModStatus::Installed, "{:#?}", report);
    assert!(ruby.verdict.is_some());
    assert!(mods.join("ruby/horizon-mod.toml").is_file());
    assert!(mods.join("ruby/.hfc-generated.json").is_file());
    for path in [
        "ruby/assets/ruby/blockstates/ruby_block.json",
        "ruby/assets/ruby/models/block/ruby_block.json",
        "ruby/assets/ruby/models/item/ruby_block.json",
        "ruby/assets/ruby/textures/block/ruby_block.png",
        "ruby/data/ruby/tags/block/ruby_blocks.json",
    ] {
        assert!(mods.join(path).is_file(), "missing installed {path}");
    }
    assert_horizon_loads(&mods, "ruby");
    assert!(
        fs::read(&source).unwrap() == original,
        "the original jar changed"
    );
    let list = fs::read_to_string(root.join("horizon-mods.txt")).unwrap();
    assert!(list.contains("mod.ruby:true"), "{list}");
    assert!(
        root.join("horizon-mods/HorizonFabricCompat/report.json")
            .is_file()
    );
    assert!(
        root.join("horizon-mods/HorizonFabricCompat/report.md")
            .is_file()
    );

    let report = open(&mods).run(RunOptions::default()).unwrap();
    let ruby = report
        .mods
        .iter()
        .find(|m| m.id.as_deref() == Some("ruby"))
        .unwrap();
    assert_eq!(ruby.status, ModStatus::Reused, "{:#?}", report);
    assert_eq!(ruby.cache, CacheState::Hit);
    assert!(fs::read(&source).unwrap() == original);

    // A changed jar is a different cache entry and replaces the generated
    // folder, with no stale sibling.
    content_jar(&source, true);
    let report = open(&mods).run(RunOptions::default()).unwrap();
    let ruby = report
        .mods
        .iter()
        .find(|m| m.id.as_deref() == Some("ruby"))
        .unwrap();
    assert_eq!(ruby.status, ModStatus::Installed);
    assert_eq!(ruby.cache, CacheState::Miss);
    let entries = fs::read_dir(mods.join("ruby")).unwrap().count();
    assert!(entries > 1);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn an_installed_mod_with_entrypoints_is_content_only_not_runtime_ready() {
    let root = temp("content-only");
    let mods = root.join("horizon-mods");
    fs::write(
        mods.join("HorizonFabricCompat/hfc.toml"),
        hfc_bootstrap::Config::template(),
    )
    .unwrap();
    // `content_jar` declares a `main` entrypoint, so its content installs but
    // its Java code never runs.
    content_jar(&mods.join("ruby.jar"), false);

    let report = open(&mods).run(RunOptions::default()).unwrap();
    let ruby = report
        .mods
        .iter()
        .find(|m| m.id.as_deref() == Some("ruby"))
        .unwrap();
    assert_eq!(ruby.status, ModStatus::Installed, "data still installs");
    assert!(
        !ruby.runtime_ready,
        "a content-only install must not be marked runtime-ready: {ruby:#?}"
    );
    assert_eq!(ruby.runtime_label(), "content-only");
    // The JSON report carries the same honest flag.
    let json: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("horizon-mods/HorizonFabricCompat/report.json")).unwrap(),
    )
    .unwrap();
    let entry = json["mods"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "ruby")
        .unwrap();
    assert_eq!(entry["runtime_ready"], false, "{entry}");
    let markdown =
        fs::read_to_string(root.join("horizon-mods/HorizonFabricCompat/report.md")).unwrap();
    assert!(
        markdown.contains("| ruby | 1.0.0 | installed | content-only |"),
        "{markdown}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn reports_libraries_and_broken_jars_without_stopping() {
    let root = temp("mixed");
    let mods = root.join("horizon-mods");
    fs::write(
        mods.join("HorizonFabricCompat/hfc.toml"),
        hfc_bootstrap::Config::template(),
    )
    .unwrap();
    content_jar(&mods.join("ruby.jar"), false);
    jar(
        &mods.join("library.jar"),
        &[(
            "fabric.mod.json",
            br#"{"schemaVersion":1,"id":"library","version":"1","entrypoints":{"main":["x.Y"]}}"#,
        )],
    );
    fs::write(mods.join("broken.jar"), b"not a zip").unwrap();

    let report = open(&mods).run(RunOptions::default()).unwrap();
    let library = report
        .mods
        .iter()
        .find(|m| m.id.as_deref() == Some("library"))
        .unwrap();
    assert_eq!(library.status, ModStatus::NotPortable);
    assert!(report.skipped.iter().any(|j| j.file_name == "broken.jar"));
    assert!(report.mods.iter().any(|m| m.status == ModStatus::Installed));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn installs_multiple_mods_and_reports_duplicate_ids() {
    let root = temp("multi");
    let mods = root.join("horizon-mods");
    fs::write(
        mods.join("HorizonFabricCompat/hfc.toml"),
        hfc_bootstrap::Config::template(),
    )
    .unwrap();
    named_content_jar(&mods.join("alpha.jar"), "alpha", false);
    named_content_jar(&mods.join("zeta.jar"), "zeta", false);
    named_content_jar(&mods.join("copy.jar"), "alpha", false);

    let report = open(&mods).run(RunOptions::default()).unwrap();
    assert_eq!(report.summary.installed, 2, "{:#?}", report);
    assert!(mods.join("alpha/horizon-mod.toml").is_file());
    assert!(mods.join("zeta/horizon-mod.toml").is_file());
    assert_horizon_loads(&mods, "alpha");
    assert_horizon_loads(&mods, "zeta");
    assert!(
        report
            .skipped
            .iter()
            .any(|j| j.file_name == "copy.jar" && j.reason.contains("duplicate mod id")),
        "{:#?}",
        report.skipped
    );
    let list = fs::read_to_string(root.join("horizon-mods.txt")).unwrap();
    assert!(list.contains("mod.alpha:true"), "{list}");
    assert!(list.contains("mod.zeta:true"), "{list}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn dry_run_writes_reports_but_no_mods() {
    let root = temp("dry");
    let mods = root.join("horizon-mods");
    fs::write(
        mods.join("HorizonFabricCompat/hfc.toml"),
        hfc_bootstrap::Config::template(),
    )
    .unwrap();
    content_jar(&mods.join("ruby.jar"), false);
    named_content_jar(&mods.join("bad-id.jar"), "bad-id", false);
    let report = open(&mods)
        .run(RunOptions {
            dry_run: true,
            ..RunOptions::default()
        })
        .unwrap();
    assert!(report.dry_run);
    assert!(
        report
            .mods
            .iter()
            .any(|m| m.status == ModStatus::WouldInstall)
    );
    assert!(
        report
            .mods
            .iter()
            .any(|m| m.id.as_deref() == Some("bad-id") && m.status == ModStatus::Blocked),
        "a dry run must still block ids Horizon cannot load"
    );
    assert!(!mods.join("ruby").exists());
    assert!(!root.join("horizon-mods.txt").exists());
    assert!(
        root.join("horizon-mods/HorizonFabricCompat/report.json")
            .is_file()
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn cache_prune_reclaims_unreferenced_entries() {
    let root = temp("prune_e2e");
    let mods = root.join("horizon-mods");
    fs::write(
        mods.join("HorizonFabricCompat/hfc.toml"),
        hfc_bootstrap::Config::template(),
    )
    .unwrap();
    named_content_jar(&mods.join("ruby.jar"), "ruby", false);
    named_content_jar(&mods.join("alpha.jar"), "alpha", false);

    // Bootstrap installs both and commits cache entries
    let report = open(&mods).run(RunOptions::default()).unwrap();
    assert_eq!(report.summary.installed, 2);

    let cache_dir = mods.join("HorizonFabricCompat/cache");
    let cache = hfc_cache::Cache::new(&cache_dir);
    assert_eq!(cache.stats().unwrap().entries, 2);

    // Remove alpha.jar and prune keeping only ruby.jar's hash
    let ruby_sha = hfc_core::sha256_file(&mods.join("ruby.jar")).unwrap();
    fs::remove_file(mods.join("alpha.jar")).unwrap();

    let mut keep = std::collections::BTreeSet::new();
    keep.insert(ruby_sha.clone());
    let prune_report = cache.prune(&keep).unwrap();

    assert_eq!(prune_report.pruned_entries, 1);
    assert!(prune_report.reclaimed_bytes > 0);
    assert_eq!(cache.stats().unwrap().entries, 1);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn duplicate_id_converts_the_same_jar_the_report_names() {
    let root = temp("duplicate-winner");
    let mods = root.join("horizon-mods");
    fs::write(
        mods.join("HorizonFabricCompat/hfc.toml"),
        hfc_bootstrap::Config::template(),
    )
    .unwrap();
    // Two jars, one id. The resolver keeps the lexically first file name
    // (`a-ruby.jar`); the report and the converted content must both describe
    // that same jar, not `zzz-ruby.jar`.
    named_content_jar_with_marker(&mods.join("zzz-ruby.jar"), "ruby", "zzz");
    named_content_jar_with_marker(&mods.join("a-ruby.jar"), "ruby", "a");

    let report = open(&mods).run(RunOptions::default()).unwrap();

    let ruby = report
        .mods
        .iter()
        .find(|m| m.id.as_deref() == Some("ruby"))
        .expect("one report row for the shared id");
    assert_eq!(
        ruby.file_name, "a-ruby.jar",
        "the report must name the resolver's winner: {:#?}",
        report.mods
    );
    assert!(
        report
            .skipped
            .iter()
            .any(|j| j.file_name == "zzz-ruby.jar" && j.reason.contains("duplicate mod id")),
        "{:#?}",
        report.skipped
    );
    assert_eq!(report.summary.installed, 1, "{:#?}", report);

    // The carried data must come from a-ruby.jar: its unique asset is present
    // and the loser's is not.
    assert!(
        mods.join("ruby/assets/ruby/textures/block/from_a.png")
            .is_file(),
        "the winner's content was not converted"
    );
    assert!(
        !mods
            .join("ruby/assets/ruby/textures/block/from_zzz.png")
            .exists(),
        "the duplicate jar's content was converted instead of the winner's"
    );
    let marker: serde_json::Value =
        serde_json::from_slice(&fs::read(mods.join("ruby/.hfc-generated.json")).unwrap()).unwrap();
    assert_eq!(marker["source_file"], "a-ruby.jar");
    assert_horizon_loads(&mods, "ruby");

    // The report must not claim content-only Java behaviour is satisfied, and
    // the markdown must not print the old blanket phrase.
    assert!(!ruby.runtime_ready, "{:#?}", ruby);
    let markdown =
        fs::read_to_string(root.join("horizon-mods/HorizonFabricCompat/report.md")).unwrap();
    assert!(
        !markdown.contains("satisfied as far as HFC can tell"),
        "{markdown}"
    );
    assert!(markdown.contains("never executed"), "{markdown}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn an_interrupted_install_is_repaired_at_the_start_of_the_next_run() {
    let root = temp("recover-run");
    let mods = root.join("horizon-mods");
    fs::write(
        mods.join("HorizonFabricCompat/hfc.toml"),
        hfc_bootstrap::Config::template(),
    )
    .unwrap();
    named_content_jar(&mods.join("ruby.jar"), "ruby", false);

    let first = open(&mods).run(RunOptions::default()).unwrap();
    assert_eq!(first.summary.installed, 1);
    let installed_marker: serde_json::Value =
        serde_json::from_slice(&fs::read(mods.join("ruby/.hfc-generated.json")).unwrap()).unwrap();

    // Simulate a crash inside the swap: the generated folder moved aside to the
    // hidden backup, and a half-written staging folder left behind.
    fs::rename(mods.join("ruby"), mods.join(".hfc-replace-4242-1")).unwrap();
    fs::create_dir_all(mods.join(".hfc-install-4242-1/assets")).unwrap();
    fs::write(
        mods.join(".hfc-install-4242-1/.hfc-generated.json"),
        serde_json::to_vec(
            &serde_json::json!({"format": 1, "mod_id": "ruby", "source_file": "ruby.jar",
                                 "source_sha256": "stale", "hfc_version": "0",
                                 "compat_api": "0", "installed_unix": 0}),
        )
        .unwrap(),
    )
    .unwrap();

    // A dry run must not repair anything, only report the pending work.
    let dry = open(&mods)
        .run(RunOptions {
            dry_run: true,
            ..RunOptions::default()
        })
        .unwrap();
    assert!(
        dry.notes.iter().any(|n| n.contains("interrupted install")),
        "{:#?}",
        dry.notes
    );
    assert!(
        !mods.join("ruby").exists(),
        "a dry run must not touch the mods folder"
    );
    assert!(mods.join(".hfc-replace-4242-1").exists());

    // The real run restores the previous folder, sweeps the stale staging, and
    // then reuses the restored install (the jar did not change).
    let report = open(&mods).run(RunOptions::default()).unwrap();
    assert!(
        report.notes.iter().any(|n| n.contains("rolled back")),
        "{:#?}",
        report.notes
    );
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("stale install staging")),
        "{:#?}",
        report.notes
    );
    assert!(mods.join("ruby/horizon-mod.toml").is_file());
    assert!(!mods.join(".hfc-replace-4242-1").exists());
    assert!(!mods.join(".hfc-install-4242-1").exists());
    let restored_marker: serde_json::Value =
        serde_json::from_slice(&fs::read(mods.join("ruby/.hfc-generated.json")).unwrap()).unwrap();
    assert_eq!(restored_marker, installed_marker);
    // Only the real folder is a mod, never the hidden swap folders.
    assert_eq!(
        report.summary.installed + report.summary.reused,
        1,
        "{:#?}",
        report
    );
    let list = fs::read_to_string(root.join("horizon-mods.txt")).unwrap();
    assert_eq!(list.matches("mod.ruby:").count(), 1, "{list}");
    let _ = fs::remove_dir_all(&root);
}
