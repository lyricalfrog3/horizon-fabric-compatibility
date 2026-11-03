//! What a mod actually needs, discovered from its bytecode instead of a list.
//!
//! These tests use the machine's real Minecraft installation when it is
//! present and skip otherwise, the same way the parity harnesses do.

use hfc_runtime::bridge::FabricBridge;
use std::path::{Path, PathBuf};

fn minecraft_dir() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("HORIZON_MINECRAFT_DIR").map(PathBuf::from),
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".local/share/horizon-launcher/minecraft")),
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".minecraft")),
    ];
    candidates.into_iter().flatten().find(|d| d.is_dir())
}

fn jars(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let libraries = root.join("libraries");
    let mut stack = vec![libraries];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|s| s.to_str()) == Some("jar") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn version_jar(root: &Path) -> Option<PathBuf> {
    let versions = root.join("versions");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&versions)
        .ok()?
        .flatten()
        .map(|entry| {
            entry
                .path()
                .join(format!("{}.jar", entry.file_name().to_string_lossy()))
        })
        .filter(|jar| jar.is_file())
        .collect();
    found.sort();
    found.pop()
}

fn image(root: &Path) -> Option<PathBuf> {
    let runtime = root.join("runtime");
    let mut stack = vec![runtime];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
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

#[test]
fn a_mods_needs_come_from_the_jars_and_the_jdk() {
    let Some(root) = minecraft_dir() else {
        eprintln!("no Minecraft installation: skipping");
        return;
    };
    let Some(client) = version_jar(&root) else {
        eprintln!("no client jar: skipping");
        return;
    };

    let mut bridge = FabricBridge::new();
    let started = std::time::Instant::now();
    bridge.add_jar(&client).unwrap();
    eprintln!("client jar indexed in {:?}", started.elapsed());
    let started = std::time::Instant::now();
    for jar in jars(&root) {
        bridge.add_jar(jar).unwrap();
    }
    eprintln!("libraries indexed in {:?}", started.elapsed());
    if let Some(image) = image(&root) {
        let started = std::time::Instant::now();
        bridge.add_jimage(image).unwrap();
        eprintln!("jimage indexed in {:?}", started.elapsed());
    }

    // The repository's own copy of the target mods, when present.
    let mods = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("test-mods");
    let mut loaded = 0;
    if let Ok(entries) = std::fs::read_dir(&mods) {
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|s| s.to_str()) == Some("jar")
                && bridge.load_mod_jar(entry.path()).is_ok()
            {
                loaded += 1;
            }
        }
    }
    if loaded == 0 {
        eprintln!("no mod jars: skipping");
        return;
    }

    let report = {
        let started = std::time::Instant::now();
        let report = bridge.class_closure();
        eprintln!("closure walked in {:?}", started.elapsed());
        report
    };
    eprintln!(
        "closure: {} resolved, {} missing{}",
        report.resolved.len(),
        report.missing.len(),
        if report.truncated { ", truncated" } else { "" }
    );
    for name in &report.missing {
        eprintln!("  missing {name}");
    }

    assert!(
        report.resolved.len() > 1000,
        "the closure should reach the client library, got {}",
        report.resolved.len()
    );
    assert!(
        report
            .resolved
            .iter()
            .any(|name| name == "net/minecraft/client/Minecraft"),
        "the client class every mod touches must be resolvable"
    );
    assert!(
        report
            .resolved
            .iter()
            .any(|name| name == "java/lang/Object"),
        "the JDK image must supply java.base"
    );

    // What happens when a mod's client initializer now runs against the real
    // client classes instead of getting `null` for every one of them?
    let started = std::time::Instant::now();
    bridge.interp.trace_missing = true;
    eprintln!(
        "Calling bridge.init() on {} entrypoints...",
        bridge.entrypoints.len()
    );
    let init_res = bridge.init();
    eprintln!("bridge.init() took {:?}: {init_res:?}", started.elapsed());
    assert!(init_res.is_ok(), "bridge.init() should succeed");
}
