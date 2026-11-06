//! Locating the machine's Minecraft installation for the census and the
//! real-classpath tests. Shared so both use the same discovery.

use std::path::{Path, PathBuf};

pub fn minecraft_dir() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("HORIZON_MINECRAFT_DIR").map(PathBuf::from),
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".local/share/horizon-launcher/minecraft")),
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".minecraft")),
    ];
    candidates.into_iter().flatten().find(|d| d.is_dir())
}

pub fn jars(root: &Path) -> Vec<PathBuf> {
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

pub fn version_jar(root: &Path) -> Option<PathBuf> {
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

pub fn image(root: &Path) -> Option<PathBuf> {
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

/// A bridge whose classpath is the installed client, its libraries and the
/// JDK image, or `None` when there is no installation.
pub fn bridge_with_vanilla() -> Option<crate::bridge::FabricBridge> {
    let root = minecraft_dir()?;
    let client = version_jar(&root)?;
    let mut bridge = crate::bridge::FabricBridge::new();
    bridge.add_jar(&client).ok()?;
    for jar in jars(&root) {
        let _ = bridge.add_jar(jar);
    }
    if let Some(image) = image(&root) {
        let _ = bridge.add_jimage(&image);
    }
    Some(bridge)
}
