//! Safe inspection of an untrusted Fabric jar.
//!
//! The scanner opens the archive with `zip`, validates every path, entry count
//! and size before reading anything, reads only metadata files, and follows
//! declared nested jars to a bounded depth. It never extracts entries, never
//! writes outside a caller-provided folder, never parses a class file as code
//! and never runs Java.

use crate::metadata::{FABRIC_MOD_JSON, Metadata, MixinConfig};
use hfc_core::{Error, Limits, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Cursor, Read, Seek},
    path::{Path, PathBuf},
};

/// The loader whose metadata a jar carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Loader {
    Fabric,
    Quilt,
    NeoForge,
    Forge,
    Unknown,
}

impl Loader {
    pub fn label(self) -> &'static str {
        match self {
            Loader::Fabric => "fabric",
            Loader::Quilt => "quilt",
            Loader::NeoForge => "neoforge",
            Loader::Forge => "forge",
            Loader::Unknown => "unknown",
        }
    }
}

/// The most bytes a nested jar may expand to before it is refused.
const MAX_NESTED_BYTES: u64 = 128 * 1024 * 1024;

/// A census of the files in an archive, used for classification and reporting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Census {
    pub files: usize,
    pub classes: usize,
    pub mixin_configs: usize,
    pub blockstates: usize,
    pub item_models: usize,
    pub recipes: usize,
    pub loot_tables: usize,
    pub tags: usize,
    pub worldgen: usize,
    pub structures: usize,
    pub languages: usize,
    pub textures: usize,
    pub sounds: usize,
    pub nested_jars: usize,
}

impl Census {
    /// Files that carry game content (as opposed to code or metadata).
    pub fn content_files(&self) -> usize {
        self.blockstates
            + self.item_models
            + self.recipes
            + self.loot_tables
            + self.tags
            + self.worldgen
            + self.structures
            + self.languages
            + self.textures
            + self.sounds
    }
}

/// A nested jar Fabric Loader would load with its parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NestedScan {
    /// The path in the parent archive, from `fabric.mod.json` `jars[].file`.
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub metadata: Option<Metadata>,
    pub census: Census,
    /// Non-fatal notes, such as a nested jar of a nested jar not being read.
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

/// Everything HFC learned from one jar without running it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JarScan {
    pub path: PathBuf,
    pub file_name: String,
    pub sha256: String,
    pub size: u64,
    pub loader: Loader,
    pub metadata: Option<Metadata>,
    pub nested: Vec<NestedScan>,
    pub census: Census,
    /// Class names named by the declared mixin configurations (`target`, or
    /// the configuration's package plus the mixin's name).
    pub mixin_targets: Vec<String>,
    /// Non-fatal problems: a missing mixin config, a nested jar that is not
    /// there, a second loader's metadata file.
    pub warnings: Vec<String>,
    /// A fatal problem with this jar. The scanner still reports hashes and a
    /// census; the bootstrap leaves the jar unprocessed.
    pub error: Option<String>,
}

impl JarScan {
    pub fn is_fabric(&self) -> bool {
        self.loader == Loader::Fabric
    }

    /// A stable identity for reports and ordering.
    pub fn display_name(&self) -> &str {
        self.metadata
            .as_ref()
            .map(|m| m.id.as_str())
            .unwrap_or(&self.file_name)
    }
}

/// A discovered jar file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DiscoveredJar {
    pub path: PathBuf,
    pub file_name: String,
}

/// Find ordinary `.jar` files directly in `dir`. Folders, hidden files,
/// `.disabled` jars and HFC's own generated folders are skipped. The original
/// files are only ever read.
pub fn discover(dir: &Path) -> Result<Vec<DiscoveredJar>> {
    let entries = std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))?;
    let mut jars = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(dir, e))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name.ends_with(".jar.disabled") {
            continue;
        }
        let is_jar = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jar"));
        if !is_jar {
            continue;
        }
        let file_type = entry.file_type().map_err(|e| Error::io(&path, e))?;
        if !file_type.is_file() {
            continue;
        }
        jars.push(DiscoveredJar {
            path,
            file_name: name,
        });
    }
    jars.sort();
    Ok(jars)
}

/// Scan one jar. A file that cannot be opened or hashed is a fatal error; a
/// malformed archive or metadata is recorded in [`JarScan::error`] instead, so
/// one bad mod never aborts a folder scan.
pub fn scan_path(path: &Path, limits: &Limits) -> Result<JarScan> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let size = std::fs::metadata(path)
        .map_err(|e| Error::io(path, e))?
        .len();
    if size > limits.max_jar_bytes {
        return Ok(empty_scan(
            path,
            file_name,
            size,
            format!(
                "archive is {size} bytes, over HFC's {} byte limit",
                limits.max_jar_bytes
            ),
        ));
    }
    let sha256 = hfc_core::sha256_file(path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) => {
            return Ok(empty_scan(
                path,
                file_name,
                size,
                format!("cannot open: {e}"),
            ));
        }
    };
    let mut zip = match zip::ZipArchive::new(file) {
        Ok(zip) => zip,
        Err(e) => {
            return Ok(empty_scan(
                path,
                file_name,
                size,
                format!("not a readable zip: {e}"),
            ));
        }
    };
    let mut scan = scan_archive(&mut zip, path, file_name, size, limits);
    scan.sha256 = sha256;
    Ok(scan)
}

/// Read the central directory and return the first name that occurs twice.
///
/// The scan is bounded: a zip64 directory, a directory larger than 64 MiB, or
/// an unparseable one returns `None` and leaves the decision to the zip
/// reader. It never allocates from an untrusted length beyond that cap.
fn duplicate_entry_name(path: &Path) -> Option<String> {
    const EOCD: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    const CENTRAL: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let tail_len = size.min(66_000) as usize;
    file.seek(std::io::SeekFrom::End(-(tail_len as i64))).ok()?;
    let mut tail = vec![0u8; tail_len];
    file.read_exact(&mut tail).ok()?;
    let end = tail.windows(4).rposition(|w| w == EOCD)?;
    let eocd = tail.get(end..)?;
    if eocd.len() < 22 {
        return None;
    }
    let count = u16::from_le_bytes([eocd[10], eocd[11]]) as usize;
    let cd_size = u32::from_le_bytes([eocd[12], eocd[13], eocd[14], eocd[15]]) as usize;
    let cd_offset = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]) as u64;
    if count == 0xffff || cd_size == 0xffff_ffff || cd_offset == 0xffff_ffff {
        return None;
    }
    if cd_size > 64 * 1024 * 1024 {
        return None;
    }
    file.seek(std::io::SeekFrom::Start(cd_offset)).ok()?;
    let mut directory = vec![0u8; cd_size];
    file.read_exact(&mut directory).ok()?;
    let mut names = BTreeSet::new();
    let mut at = 0usize;
    for _ in 0..count {
        let header = directory.get(at..at + 46)?;
        if header[..4] != CENTRAL {
            return None;
        }
        let name_len = u16::from_le_bytes([header[28], header[29]]) as usize;
        let extra_len = u16::from_le_bytes([header[30], header[31]]) as usize;
        let comment_len = u16::from_le_bytes([header[32], header[33]]) as usize;
        let start = at + 46;
        let name = directory.get(start..start + name_len)?;
        let name = String::from_utf8_lossy(name).into_owned();
        if !names.insert(name.clone()) {
            return Some(name);
        }
        at = start + name_len + extra_len + comment_len;
    }
    None
}

fn empty_scan(path: &Path, file_name: String, size: u64, error: String) -> JarScan {
    JarScan {
        path: path.to_path_buf(),
        file_name,
        sha256: String::new(),
        size,
        loader: Loader::Unknown,
        metadata: None,
        nested: Vec::new(),
        census: Census::default(),
        mixin_targets: Vec::new(),
        warnings: Vec::new(),
        error: Some(error),
    }
}

fn scan_archive(
    zip: &mut zip::ZipArchive<File>,
    path: &Path,
    file_name: String,
    size: u64,
    limits: &Limits,
) -> JarScan {
    let mut scan = JarScan {
        path: path.to_path_buf(),
        file_name,
        sha256: String::new(),
        size,
        loader: Loader::Unknown,
        metadata: None,
        nested: Vec::new(),
        census: Census::default(),
        mixin_targets: Vec::new(),
        warnings: Vec::new(),
        error: None,
    };
    // 0. The zip crate indexes entries by name, so duplicate central-directory
    // names collapse before `by_index_raw` can see them. Check the raw
    // directory first; a duplicate is a malformed archive, not a mod.
    if let Some(name) = duplicate_entry_name(path) {
        scan.error = Some(format!("duplicate archive entry {name:?}"));
        return scan;
    }
    // 1. Validate every entry before reading any of them.
    if zip.len() > limits.max_entries {
        scan.error = Some(format!(
            "archive holds {} entries, over HFC's {} limit",
            zip.len(),
            limits.max_entries
        ));
        return scan;
    }
    let mut names = BTreeSet::new();
    let mut total: u64 = 0;
    let mut all_names = Vec::new();
    for index in 0..zip.len() {
        let entry = match zip.by_index_raw(index) {
            Ok(entry) => entry,
            Err(e) => {
                scan.error = Some(format!("entry {index} cannot be read: {e}"));
                return scan;
            }
        };
        let name = match entry.name() {
            Ok(name) => name.into_owned(),
            Err(e) => {
                scan.error = Some(format!("entry {index} has no readable name: {e}"));
                return scan;
            }
        };
        let is_dir = name.ends_with('/') || entry.is_dir();
        let entry_size = entry.size();
        total = total.saturating_add(entry_size);
        if total > limits.max_uncompressed_bytes {
            scan.error = Some(format!(
                "archive expands to more than HFC's {} byte limit",
                limits.max_uncompressed_bytes
            ));
            return scan;
        }
        if is_dir {
            continue;
        }
        if !hfc_core::safe_archive_path(&name, limits.max_path_bytes) {
            scan.error = Some(format!("unsafe archive path {name:?}"));
            return scan;
        }
        if !names.insert(name.clone()) {
            scan.error = Some(format!("duplicate archive entry {name:?}"));
            return scan;
        }
        all_names.push(name);
    }
    scan.census = census(&all_names);
    scan.loader = loader(&names);
    if scan.loader != Loader::Fabric {
        if scan.loader == Loader::Unknown {
            scan.error = Some("no fabric.mod.json (not a Fabric mod)".into());
        } else {
            scan.error = Some(format!(
                "{} metadata, not Fabric: HFC only reads fabric.mod.json",
                scan.loader.label()
            ));
        }
        return scan;
    }
    for (name, label) in [
        ("META-INF/neoforge.mods.toml", "NeoForge"),
        ("META-INF/mods.toml", "Forge"),
        ("quilt.mod.json", "Quilt"),
    ] {
        if names.contains(name) {
            scan.warnings
                .push(format!("also carries {label} metadata ({name}); ignored"));
        }
    }
    // 2. Read fabric.mod.json.
    let bytes = match read_entry(zip, FABRIC_MOD_JSON, limits.max_metadata_bytes) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            scan.error = Some(format!("{FABRIC_MOD_JSON}: missing"));
            return scan;
        }
        Err(e) => {
            scan.error = Some(e);
            return scan;
        }
    };
    let metadata = match Metadata::parse(&bytes) {
        Ok(metadata) => metadata,
        Err(e) => {
            scan.error = Some(e);
            return scan;
        }
    };
    // 3. Declared mixin configurations, read as JSON only.
    for config in &metadata.mixins {
        match read_mixin_targets(zip, config, limits) {
            Ok(targets) => merge_targets(&mut scan.mixin_targets, targets),
            Err(e) => scan.warnings.push(e),
        }
    }
    scan.mixin_targets.sort();
    scan.mixin_targets.dedup();
    // 4. Read-only checks of declared files, and nested jars.
    if let Some(path) = &metadata.access_widener
        && !names.contains(path)
        && !names.contains(&format!("META-INF/{path}"))
    {
        scan.warnings.push(format!(
            "declared access widener {path:?} is not in the jar"
        ));
    }
    if metadata.jars.len() > limits.max_nested_jars {
        scan.error = Some(format!(
            "declares {} nested jars, over HFC's {} limit",
            metadata.jars.len(),
            limits.max_nested_jars
        ));
        scan.metadata = Some(metadata);
        return scan;
    }
    if limits.max_nested_depth > 0 {
        for nested in &metadata.jars {
            let path = nested.file.as_str();
            if !hfc_core::safe_archive_path(path, limits.max_path_bytes) {
                scan.warnings.push(format!(
                    "nested jar path {path:?} is not a safe archive path"
                ));
                continue;
            }
            let bytes = match read_entry(zip, path, MAX_NESTED_BYTES.min(limits.max_jar_bytes)) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    scan.warnings
                        .push(format!("declared nested jar {path:?} is not in the jar"));
                    continue;
                }
                Err(e) => {
                    scan.warnings.push(e);
                    continue;
                }
            };
            scan.nested.push(scan_nested(
                path,
                &bytes,
                limits.max_nested_depth.saturating_sub(1),
                limits,
            ));
        }
    }
    scan.census.nested_jars = scan.nested.len();
    scan.metadata = Some(metadata);
    scan
}

fn scan_nested(path: &str, bytes: &[u8], depth: u8, limits: &Limits) -> NestedScan {
    let sha256 = hfc_core::sha256_bytes(bytes);
    let mut result = NestedScan {
        path: path.to_owned(),
        sha256,
        size: bytes.len() as u64,
        metadata: None,
        census: Census::default(),
        warnings: Vec::new(),
        error: None,
    };
    let mut zip = match zip::ZipArchive::new(Cursor::new(bytes.to_vec())) {
        Ok(zip) => zip,
        Err(e) => {
            result.error = Some(format!("nested jar {path:?} is not a readable zip: {e}"));
            return result;
        }
    };
    if zip.len() > limits.max_entries {
        result.error = Some(format!(
            "nested jar {path:?} holds {} entries, over HFC's {} limit",
            zip.len(),
            limits.max_entries
        ));
        return result;
    }
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    let mut all_names = Vec::new();
    for index in 0..zip.len() {
        let Ok(entry) = zip.by_index_raw(index) else {
            result.error = Some(format!("nested jar {path:?}: entry {index} unreadable"));
            return result;
        };
        let Ok(name) = entry.name().map(|name| name.into_owned()) else {
            result.error = Some(format!("nested jar {path:?}: entry {index} has no name"));
            return result;
        };
        if name.ends_with('/') || entry.is_dir() {
            continue;
        }
        total = total.saturating_add(entry.size());
        if total > limits.max_uncompressed_bytes {
            result.error = Some(format!("nested jar {path:?} expands past HFC's limit"));
            return result;
        }
        if !hfc_core::safe_archive_path(&name, limits.max_path_bytes) {
            result.error = Some(format!("nested jar {path:?}: unsafe path {name:?}"));
            return result;
        }
        if !names.insert(name.clone()) {
            result.error = Some(format!("nested jar {path:?}: duplicate entry {name:?}"));
            return result;
        }
        all_names.push(name);
    }
    result.census = census(&all_names);
    if !names.contains(FABRIC_MOD_JSON) {
        result.error = Some(format!("nested jar {path:?} has no {FABRIC_MOD_JSON}"));
        return result;
    }
    match read_entry(&mut zip, FABRIC_MOD_JSON, limits.max_metadata_bytes) {
        Ok(Some(bytes)) => match Metadata::parse(&bytes) {
            Ok(metadata) => result.metadata = Some(metadata),
            Err(e) => result.error = Some(e),
        },
        Ok(None) => result.error = Some(format!("nested jar {path:?} has no {FABRIC_MOD_JSON}")),
        Err(e) => result.error = Some(e),
    }
    // Nested jars of nested jars are reported, but their content is not read:
    // depth is bounded and the report says so.
    if depth == 0 && result.metadata.as_ref().is_some_and(|m| !m.jars.is_empty()) {
        result.warnings.push(
            "this nested jar declares further nested jars; HFC's depth limit stops here".into(),
        );
    }
    result
}

fn read_entry<R: Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    max: u64,
) -> std::result::Result<Option<Vec<u8>>, String> {
    let entry = match zip.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(format!("{name}: {e}")),
    };
    if entry.size() > max {
        return Err(format!(
            "{name}: {} bytes, over HFC's {max} byte metadata limit",
            entry.size()
        ));
    }
    let mut bytes = Vec::with_capacity(entry.size().min(max) as usize);
    entry
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{name}: {e}"))?;
    if bytes.len() as u64 > max {
        return Err(format!("{name}: expands past HFC's {max} byte limit"));
    }
    Ok(Some(bytes))
}

fn read_mixin_targets(
    zip: &mut zip::ZipArchive<File>,
    config: &MixinConfig,
    limits: &Limits,
) -> std::result::Result<Vec<String>, String> {
    let path = config.path();
    if !hfc_core::safe_archive_path(path, limits.max_path_bytes) {
        return Err(format!("mixin config {path:?} is not a safe archive path"));
    }
    let Some(bytes) = read_entry(zip, path, limits.max_metadata_bytes)? else {
        return Err(format!("declared mixin config {path:?} is not in the jar"));
    };
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("{path}: malformed mixin JSON: {e}"))?;
    let package = value
        .get("package")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let mut targets = Vec::new();
    for key in ["mixins", "client", "server"] {
        let Some(items) = value.get(key).and_then(serde_json::Value::as_array) else {
            continue;
        };
        for item in items {
            if let Some(target) = item.get("target").and_then(serde_json::Value::as_str) {
                targets.push(target.to_owned());
            } else if let Some(name) = item.get("name").and_then(serde_json::Value::as_str) {
                if package.is_empty() {
                    targets.push(name.to_owned());
                } else {
                    targets.push(format!("{package}.{name}"));
                }
            }
        }
    }
    targets.sort();
    targets.dedup();
    Ok(targets)
}

fn merge_targets(into: &mut Vec<String>, targets: Vec<String>) {
    into.extend(targets);
}

fn loader(names: &BTreeSet<String>) -> Loader {
    if names.contains(FABRIC_MOD_JSON) {
        Loader::Fabric
    } else if names.contains("quilt.mod.json") {
        Loader::Quilt
    } else if names.contains("META-INF/neoforge.mods.toml") {
        Loader::NeoForge
    } else if names.contains("META-INF/mods.toml") {
        Loader::Forge
    } else {
        Loader::Unknown
    }
}

/// Count the files that matter to classification. This is a name census: no
/// entry is decompressed.
pub fn census(names: &[String]) -> Census {
    let mut c = Census {
        files: names.len(),
        ..Census::default()
    };
    for name in names {
        let parts: Vec<&str> = name.split('/').collect();
        if name.ends_with(".class") {
            c.classes += 1;
            continue;
        }
        if name.ends_with(".mixins.json") || (parts.len() == 1 && name.contains("mixins")) {
            c.mixin_configs += 1;
        }
        if name.starts_with("META-INF/jars/") && name.ends_with(".jar") {
            c.nested_jars += 1;
            continue;
        }
        let (Some(root), Some(kind)) = (parts.first(), parts.get(2)) else {
            continue;
        };
        let json = name.ends_with(".json");
        match (*root, *kind) {
            ("assets", "blockstates") if json => c.blockstates += 1,
            ("assets", "models") if json && parts.get(3) == Some(&"item") => c.item_models += 1,
            ("assets", "lang") => c.languages += 1,
            ("assets", "textures") if name.ends_with(".png") => c.textures += 1,
            ("assets", _) if name.ends_with("sounds.json") => c.sounds += 1,
            ("data", "recipe" | "recipes") if json => c.recipes += 1,
            ("data", "loot_table" | "loot_tables") if json => c.loot_tables += 1,
            ("data", "tags") if json => c.tags += 1,
            ("data", "worldgen") if json => c.worldgen += 1,
            ("data", "structure" | "structures") if name.ends_with(".nbt") => c.structures += 1,
            _ => {}
        }
    }
    c
}

/// A compact map for reports: file kind → count.
pub fn census_map(census: &Census) -> BTreeMap<&'static str, usize> {
    let mut out = BTreeMap::new();
    for (name, value) in [
        ("classes", census.classes),
        ("blockstates", census.blockstates),
        ("item models", census.item_models),
        ("recipes", census.recipes),
        ("loot tables", census.loot_tables),
        ("tags", census.tags),
        ("worldgen", census.worldgen),
        ("structures", census.structures),
        ("textures", census.textures),
        ("languages", census.languages),
        ("mixin configs", census.mixin_configs),
        ("nested jars", census.nested_jars),
    ] {
        if value > 0 {
            out.insert(name, value);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hfc-scan-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A minimal stored zip, written by hand so that deliberately malformed
    /// archives (duplicate names) can be produced; `ZipWriter` refuses them.
    fn raw_zip(dir: &Path, name: &str, entries: &[&str]) -> PathBuf {
        let path = dir.join(name);
        let mut bytes = Vec::new();
        let mut central = Vec::new();
        for entry in entries {
            let offset = bytes.len() as u32;
            let data = b"x";
            let put_local = |out: &mut Vec<u8>| {
                out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
                out.extend_from_slice(&20u16.to_le_bytes());
                out.extend_from_slice(&0u16.to_le_bytes());
                out.extend_from_slice(&0u16.to_le_bytes());
                out.extend_from_slice(&0u16.to_le_bytes());
                out.extend_from_slice(&0u16.to_le_bytes());
                out.extend_from_slice(&0u32.to_le_bytes());
                out.extend_from_slice(&(data.len() as u32).to_le_bytes());
                out.extend_from_slice(&(data.len() as u32).to_le_bytes());
                out.extend_from_slice(&(entry.len() as u16).to_le_bytes());
                out.extend_from_slice(&0u16.to_le_bytes());
                out.extend_from_slice(entry.as_bytes());
                out.extend_from_slice(data);
            };
            put_local(&mut bytes);
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(entry.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(entry.as_bytes());
        }
        let cd_offset = bytes.len() as u32;
        let cd_size = central.len() as u32;
        bytes.extend_from_slice(&central);
        bytes.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&cd_size.to_le_bytes());
        bytes.extend_from_slice(&cd_offset.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn jar(dir: &Path, name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join(name);
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        for (entry, bytes) in entries {
            zip.start_file(*entry, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    const MOD: &[u8] = br#"{"schemaVersion":1,"id":"ruby","version":"1.0.0",
        "name":"Ruby","depends":{"minecraft":">=1.20","cloth-config":"*"},
        "entrypoints":{"main":["ruby.Ruby"]},"mixins":["ruby.mixins.json"]}"#;

    #[test]
    fn reads_metadata_census_and_mixin_targets() {
        let dir = temp("full");
        let path = jar(
            &dir,
            "ruby.jar",
            &[
                ("fabric.mod.json", MOD),
                (
                    "ruby.mixins.json",
                    br#"{"package":"ruby.mixin","mixins":[{"name":"Chest"},{"target":"net.minecraft.Foo"}]}"#,
                ),
                ("assets/ruby/blockstates/ruby_block.json", b"{}"),
                ("assets/ruby/textures/block/ruby.png", b"png"),
                ("data/ruby/recipe/ruby.json", b"{}"),
                ("ruby/Ruby.class", b"\xca\xfe\xba\xbe"),
            ],
        );
        let scan = scan_path(&path, &Limits::default()).unwrap();
        assert!(scan.error.is_none(), "{:?}", scan.error);
        assert_eq!(scan.loader, Loader::Fabric);
        let m = scan.metadata.as_ref().unwrap();
        assert_eq!(m.id, "ruby");
        assert_eq!(scan.census.blockstates, 1);
        assert_eq!(scan.census.textures, 1);
        assert_eq!(scan.census.recipes, 1);
        assert_eq!(scan.census.classes, 1);
        assert_eq!(
            scan.mixin_targets,
            ["net.minecraft.Foo", "ruby.mixin.Chest"]
        );
        assert_eq!(&scan.sha256[..8], &scan.sha256[..8]);
        assert_eq!(scan.sha256.len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_traversal_duplicates_and_oversized_metadata() {
        let dir = temp("bad");
        for (name, entries) in [
            ("traversal.jar", vec![("../evil", b"x".as_slice())]),
            ("absolute.jar", vec![("/etc/passwd", b"x".as_slice())]),
        ] {
            let path = jar(&dir, name, &entries);
            let scan = scan_path(&path, &Limits::default()).unwrap();
            assert!(scan.error.is_some(), "{name}: {scan:?}");
        }
        let path = raw_zip(&dir, "duplicate.jar", &["a", "a"]);
        let scan = scan_path(&path, &Limits::default()).unwrap();
        assert!(
            scan.error
                .as_deref()
                .unwrap()
                .to_lowercase()
                .contains("duplicate"),
            "{:?}",
            scan.error
        );
        let path = jar(&dir, "big.jar", &[("fabric.mod.json", MOD)]);
        let limits = Limits {
            max_metadata_bytes: 8,
            ..Limits::default()
        };
        let scan = scan_path(&path, &limits).unwrap();
        assert!(scan.error.as_deref().unwrap().contains("limit"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_and_missing_metadata_are_reported() {
        let dir = temp("malformed");
        let path = jar(&dir, "bad.jar", &[("fabric.mod.json", b"{not json")]);
        let scan = scan_path(&path, &Limits::default()).unwrap();
        assert!(scan.error.as_deref().unwrap().contains("JSON"));
        let path = jar(&dir, "none.jar", &[("a.txt", b"x")]);
        let scan = scan_path(&path, &Limits::default()).unwrap();
        assert!(scan.error.as_deref().unwrap().contains("not a Fabric mod"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_declared_nested_jars_with_a_depth_limit() {
        let dir = temp("nested");
        let inner = {
            let path = dir.join("inner.jar");
            let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
            zip.start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(br#"{"id":"inner","version":"1"}"#).unwrap();
            zip.finish().unwrap();
            std::fs::read(path).unwrap()
        };
        let outer = br#"{"id":"outer","version":"1","jars":[{"file":"META-INF/jars/inner.jar"}]}"#;
        let path = jar(
            &dir,
            "outer.jar",
            &[
                ("fabric.mod.json", outer),
                ("META-INF/jars/inner.jar", inner.as_slice()),
            ],
        );
        let scan = scan_path(&path, &Limits::default()).unwrap();
        assert_eq!(scan.nested.len(), 1);
        assert_eq!(scan.nested[0].metadata.as_ref().unwrap().id, "inner");
        assert!(scan.nested[0].error.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovery_is_flat_and_skips_disabled_and_hidden() {
        let dir = temp("discover");
        for name in ["a.jar", "b.JAR", "c.jar.disabled", ".hidden.jar"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        std::fs::create_dir_all(dir.join("folder.jar")).unwrap();
        let found = discover(&dir).unwrap();
        let names: Vec<&str> = found.iter().map(|j| j.file_name.as_str()).collect();
        assert_eq!(names, ["a.jar", "b.JAR"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
