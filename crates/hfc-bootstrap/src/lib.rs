//! The pre-launch bootstrap: find jars, analyze them, convert content, install
//! generated folder mods and write a report.
//!
//! This is the only crate that writes into the player's mods folder. It never
//! writes to an original `.jar`, and it removes only folders carrying its own
//! `.hfc-generated.json` marker.
//!
//! The launcher's job is deliberately small: if
//! `<mods>/HorizonFabricCompat/hfc.toml` exists, run `hfc bootstrap --mods
//! <mods>` before starting the game. All policy lives here.

pub mod config;
pub mod install;
pub mod report;
pub mod state;

pub use config::{CONFIG_FILE, Config};
pub use report::{CacheState, ModReport, ModStatus, Report, Summary};
pub use state::{InstalledMod, State};

use hfc_cache::{Cache, CachedEntry, KeyContext};
use hfc_core::{Error, Limits, Result};
use hfc_fabric::{EnvironmentInfo, Issue, JarScan, ModNode, SkippedJar, resolve, scan_path};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// Options for one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunOptions {
    /// Analyze and report without converting, installing or cleaning up.
    pub dry_run: bool,
    /// Ignore cached conversions.
    pub force: bool,
    /// The timestamp recorded in the state, marker and report.
    pub now: u64,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            force: false,
            now: now_unix(),
        }
    }
}

pub struct Bootstrap {
    mods_dir: PathBuf,
    compat_dir: PathBuf,
    list_path: PathBuf,
    config: Config,
    limits: Limits,
}

impl Bootstrap {
    /// Open an installation. `Ok(None)` means the folder is not an HFC
    /// installation (no `hfc.toml`), which is not an error: the launcher
    /// simply skips HFC.
    pub fn open(mods_dir: impl Into<PathBuf>) -> Result<Option<Self>> {
        let mods_dir = mods_dir.into();
        if !mods_dir.is_dir() {
            return Err(Error::new(
                mods_dir.display().to_string(),
                "the mods folder does not exist",
            ));
        }
        let compat_dir = config::compat_dir(&mods_dir);
        if !config::marker(&mods_dir).is_file() {
            return Ok(None);
        }
        let config = Config::load(&compat_dir)?;
        let list_path = mods_dir
            .parent()
            .map(|parent| parent.join("horizon-mods.txt"))
            .unwrap_or_else(|| mods_dir.join("horizon-mods.txt"));
        Ok(Some(Self {
            mods_dir,
            compat_dir,
            list_path,
            config,
            limits: Limits::default(),
        }))
    }

    /// Override where `horizon-mods.txt` is written (tests, or an instance
    /// whose mods folder is not a direct child of the game folder).
    pub fn with_mod_list(mut self, path: impl Into<PathBuf>) -> Self {
        self.list_path = path.into();
        self
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn mods_dir(&self) -> &Path {
        &self.mods_dir
    }

    pub fn compat_dir(&self) -> &Path {
        &self.compat_dir
    }

    pub fn run(&self, options: RunOptions) -> Result<Report> {
        let mut report = Report::new(&self.mods_dir, options.dry_run, options.now);
        let (state, state_note) = State::load(&self.compat_dir.join(hfc_core::STATE));
        if let Some(note) = state_note {
            report.notes.push(note);
        }

        let mut discovered = hfc_fabric::discover(&self.mods_dir)?;
        if discovered.len() > self.config.compat.max_mods {
            report.notes.push(format!(
                "found {} jars; only the first {} are scanned (compat.max_mods)",
                discovered.len(),
                self.config.compat.max_mods
            ));
            discovered.truncate(self.config.compat.max_mods);
        }

        let mut scans: Vec<JarScan> = Vec::new();
        for jar in discovered {
            match scan_path(&jar.path, &self.limits) {
                Ok(scan) => scans.push(scan),
                Err(error) => report.skipped.push(SkippedJar {
                    file_name: jar.file_name,
                    sha256: String::new(),
                    reason: error.to_string(),
                }),
            }
        }

        let environment = EnvironmentInfo {
            minecraft: self.config.compat.minecraft.clone(),
            fabric_loader: self.config.compat.fabric_loader.clone(),
            java: self.config.compat.java.clone(),
        };
        let resolution = resolve(&scans, &environment).map_err(|e| Error::new("resolve", e))?;
        report.issues = resolution.issues.clone();
        report.skipped.extend(resolution.skipped.clone());

        // One report row per top-level mod, in the resolver's deterministic
        // order. Nested providers are noted, not installed as siblings.
        let scan_by_id: BTreeMap<String, &JarScan> = scans
            .iter()
            .filter_map(|scan| {
                scan.metadata
                    .as_ref()
                    .map(|metadata| (metadata.id.clone(), scan))
            })
            .collect();
        for node in resolution
            .mods
            .iter()
            .filter(|node| !node.origin.is_nested())
        {
            let issues = resolution
                .issues_for(&node.id)
                .cloned()
                .collect::<Vec<Issue>>();
            report.mods.push(mod_report(node, issues));
        }
        let nested: Vec<&ModNode> = resolution
            .mods
            .iter()
            .filter(|node| node.origin.is_nested())
            .collect();
        for node in nested {
            report.notes.push(format!(
                "{} {} is a nested provider from {}; it satisfies dependencies but its \
                 content is only carried as part of the parent jar",
                node.id,
                node.version,
                node.origin.file_name()
            ));
        }

        let enabled_by_config = self.config.compat.enabled && self.config.carry.enabled;
        if !self.config.compat.enabled {
            report
                .notes
                .push("compat.enabled = false: nothing was installed".into());
        } else if !self.config.carry.enabled {
            report
                .notes
                .push("carry.enabled = false: analyzed only, nothing was installed".into());
        }

        let cache = Cache::new(self.compat_dir.join("cache"));
        let key = KeyContext::new(
            horizon_core::version::VERSION_ID,
            format!("mc={}", self.config.compat.minecraft),
        );
        let mut desired: BTreeMap<String, (String, String, String)> = BTreeMap::new();
        let mut namespaces: BTreeMap<String, String> = BTreeMap::new();

        for id in &resolution.order {
            let Some(node) = resolution.node(id) else {
                continue;
            };
            let Some(scan) = scan_by_id.get(id) else {
                continue;
            };
            let Some(entry) = report.mods.iter_mut().find(|m| m.id.as_deref() == Some(id)) else {
                continue;
            };
            if !enabled_by_config {
                entry.status = ModStatus::Scanned;
                continue;
            }
            if !is_carry_id(id) {
                entry.status = ModStatus::Blocked;
                entry.issues.push(Issue {
                    severity: hfc_fabric::Severity::Error,
                    code: hfc_fabric::IssueCode::InvalidMetadata,
                    mod_id: Some(id.clone()),
                    related: Vec::new(),
                    message: format!(
                        "mod id {id:?} is not [a-z0-9_]{{1,64}}, the only form Horizon can \
                         install as a folder mod; rename the mod or ask its author"
                    ),
                });
                continue;
            }
            if options.dry_run {
                entry.status = ModStatus::WouldInstall;
                entry.cache = CacheState::NotReached;
                continue;
            }
            let outcome = self.convert_and_install(
                scan,
                node,
                entry,
                &cache,
                &key,
                &options,
                &mut namespaces,
            );
            match outcome {
                Ok(Some(installed)) => {
                    desired.insert(
                        installed.clone(),
                        (
                            scan.file_name.clone(),
                            scan.sha256.clone(),
                            entry.verdict.clone().unwrap_or_default(),
                        ),
                    );
                    entry.installed_as = Some(installed);
                }
                Ok(None) => {}
                Err(error) => {
                    entry.status = ModStatus::Failed;
                    report.notes.push(format!("{id}: {error}"));
                }
            }
        }

        if !options.dry_run {
            let removed = self.cleanup(&desired, &state, &mut report)?;
            let state = State {
                format: state::STATE_FORMAT,
                installed: report
                    .mods
                    .iter()
                    .filter_map(|m| {
                        let id = m.installed_as.as_ref()?;
                        let (file, sha, verdict) = desired.get(id)?;
                        Some(InstalledMod {
                            id: id.clone(),
                            source_file: file.clone(),
                            source_sha256: sha.clone(),
                            verdict: verdict.clone(),
                            installed_unix: options.now,
                        })
                    })
                    .collect(),
            };
            state.save(&self.compat_dir.join(hfc_core::STATE))?;
            self.sync_mod_list(&desired, &removed)?;
            if !removed.is_empty() {
                report.notes.push(format!(
                    "turned off generated mods that are no longer installed: {}",
                    removed.iter().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
        } else {
            report
                .notes
                .push("dry run: no files were installed, replaced or removed".into());
        }

        report.recompute();
        report.write(&self.compat_dir)?;
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    fn convert_and_install(
        &self,
        scan: &JarScan,
        node: &ModNode,
        entry: &mut ModReport,
        cache: &Cache,
        key: &KeyContext,
        options: &RunOptions,
        namespaces: &mut BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        let (cached, cache_state) = self.convert(scan, cache, key, options.force)?;
        entry.cache = cache_state;
        let check = read_check(&cached)?;
        entry.verdict = Some(verdict_label(&check));
        entry.carry_summary = Some(check.summary.clone());
        entry.ports = check.ports.clone();
        entry.skipped_features = check.skipped.clone();

        if check.mod_id != node.id {
            entry.issues.push(Issue {
                severity: hfc_fabric::Severity::Warning,
                code: hfc_fabric::IssueCode::InvalidMetadata,
                mod_id: Some(node.id.clone()),
                related: Vec::new(),
                message: format!(
                    "fabric.mod.json id {:?} does not match the port's id {:?}; using {:?}",
                    node.id, check.mod_id, check.mod_id
                ),
            });
        }
        if !is_carry_id(&check.mod_id) {
            entry.status = ModStatus::Blocked;
            entry.issues.push(Issue {
                severity: hfc_fabric::Severity::Error,
                code: hfc_fabric::IssueCode::InvalidMetadata,
                mod_id: Some(node.id.clone()),
                related: Vec::new(),
                message: format!(
                    "the carried id {:?} is not [a-z0-9_]{{1,64}}; Horizon cannot load it",
                    check.mod_id
                ),
            });
            return Ok(None);
        }

        // Two mods may not own the same namespace in Horizon.
        for namespace in &check.namespaces {
            if let Some(owner) = namespaces.get(namespace)
                && owner != &check.mod_id
            {
                entry.status = ModStatus::Blocked;
                entry.issues.push(Issue {
                    severity: hfc_fabric::Severity::Error,
                    code: hfc_fabric::IssueCode::Conflict,
                    mod_id: Some(node.id.clone()),
                    related: vec![owner.clone()],
                    message: format!(
                        "namespace {namespace:?} is already provided by {owner}; Horizon \
                         content mods may not share a namespace"
                    ),
                });
                return Ok(None);
            }
        }

        if check.verdict == horizon_port_pack::check::Verdict::No {
            let is_runnable_code_mod = scan.census.classes > 0
                && cached
                    .carry_dir()
                    .join(horizon_mods::manifest::MANIFEST)
                    .is_file();
            if !is_runnable_code_mod {
                entry.status = ModStatus::NotPortable;
                entry.runtime_ready = false;
                return Ok(None);
            }
        }

        // Horizon's own loader is the authority on whether the carry is a
        // content mod it will accept. Validate before anything reaches the
        // mods folder, so a pipeline bug cannot install a broken folder.
        if let Err(error) = verify_carry(&cached.carry_dir(), &check.mod_id) {
            entry.status = ModStatus::Failed;
            entry.runtime_ready = false;
            entry.issues.push(Issue {
                severity: hfc_fabric::Severity::Error,
                code: hfc_fabric::IssueCode::InvalidMetadata,
                mod_id: Some(node.id.clone()),
                related: Vec::new(),
                message: format!("Horizon rejected the generated mod: {error}"),
            });
            return Ok(None);
        }

        let target = self.mods_dir.join(&check.mod_id);
        let marker = install::GeneratedMarker::new(
            &check.mod_id,
            &scan.file_name,
            &scan.sha256,
            options.now,
        );
        match install::ensure_installed(&cached.carry_dir(), &target, &marker) {
            Ok(install::InstallResult::Installed) => {
                entry.status = ModStatus::Installed;
                entry.runtime_ready = true;
            }
            Ok(install::InstallResult::Reused) => {
                entry.status = ModStatus::Reused;
                entry.runtime_ready = true;
            }
            Err(error) => {
                entry.status = ModStatus::Failed;
                entry.issues.push(Issue {
                    severity: hfc_fabric::Severity::Error,
                    code: hfc_fabric::IssueCode::InvalidMetadata,
                    mod_id: Some(node.id.clone()),
                    related: Vec::new(),
                    message: error.to_string(),
                });
                return Ok(None);
            }
        }
        for namespace in &check.namespaces {
            namespaces.insert(namespace.clone(), check.mod_id.clone());
        }
        Ok(Some(check.mod_id))
    }

    fn convert(
        &self,
        scan: &JarScan,
        cache: &Cache,
        key: &KeyContext,
        force: bool,
    ) -> Result<(CachedEntry, CacheState)> {
        if !self.config.compat.cache {
            let staging = cache.staging()?;
            horizon_port_pack::check::check(&scan.path, &staging)
                .map_err(|e| Error::new(scan.file_name.clone(), e))?;
            let cached = cache.commit(&staging, &scan.sha256, key)?;
            return Ok((cached, CacheState::Disabled));
        }
        if !force
            && let Some(cached) = cache.lookup(&scan.sha256, key)?
            && read_check(&cached).is_ok()
        {
            return Ok((cached, CacheState::Hit));
        }
        let staging = cache.staging()?;
        horizon_port_pack::check::check(&scan.path, &staging)
            .map_err(|e| Error::new(scan.file_name.clone(), e))?;
        let cached = cache.commit(&staging, &scan.sha256, key)?;
        Ok((cached, CacheState::Miss))
    }

    /// Remove generated folders whose source jar disappeared or changed, and
    /// disable them in `horizon-mods.txt`. Only marked folders are touched.
    fn cleanup(
        &self,
        desired: &BTreeMap<String, (String, String, String)>,
        state: &State,
        report: &mut Report,
    ) -> Result<BTreeSet<String>> {
        let mut removed = BTreeSet::new();
        for previous in &state.installed {
            if !desired.contains_key(&previous.id) {
                removed.insert(previous.id.clone());
            }
        }
        for (path, marker) in install::generated_folders(&self.mods_dir)? {
            let keep = desired
                .get(&marker.mod_id)
                .is_some_and(|(_, sha, _)| sha == &marker.source_sha256);
            if keep {
                continue;
            }
            match install::remove_generated(&path, &marker.mod_id) {
                Ok(true) => {
                    report.notes.push(format!(
                        "removed generated mod {:?} (its source jar is gone or changed)",
                        marker.mod_id
                    ));
                    removed.insert(marker.mod_id.clone());
                }
                Ok(false) => {}
                Err(error) => report
                    .notes
                    .push(format!("could not remove {}: {error}", path.display())),
            }
        }
        Ok(removed)
    }

    /// Write `mod.<id>:true` for installed mods and `mod.<id>:false` for
    /// generated mods that are no longer present. Other lines are preserved.
    fn sync_mod_list(
        &self,
        desired: &BTreeMap<String, (String, String, String)>,
        removed: &BTreeSet<String>,
    ) -> Result<()> {
        let path = &self.list_path;
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(Error::io(path, e)),
        };
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let managed: BTreeSet<&str> = desired
            .keys()
            .map(String::as_str)
            .chain(removed.iter().map(String::as_str))
            .collect();
        lines.retain(|line| {
            let Some((key, _)) = line.split_once(':') else {
                return true;
            };
            let Some(id) = key.trim().strip_prefix("mod.") else {
                return true;
            };
            !managed.contains(id)
        });
        for id in desired.keys() {
            lines.push(format!("mod.{id}:true"));
        }
        for id in removed {
            lines.push(format!("mod.{id}:false"));
        }
        let mut out = lines.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        hfc_core::atomic_write(path, out.as_bytes())?;
        Ok(())
    }
}

fn mod_report(node: &ModNode, issues: Vec<Issue>) -> ModReport {
    ModReport {
        id: Some(node.id.clone()),
        name: node.name.clone(),
        version: node.version.clone(),
        file_name: node.origin.file_name().to_owned(),
        sha256: node.origin.sha256().to_owned(),
        status: if node.installable {
            ModStatus::Scanned
        } else {
            ModStatus::Blocked
        },
        runtime_ready: node.runtime_ready,
        verdict: None,
        carry_summary: None,
        installed_as: None,
        cache: CacheState::NotReached,
        ports: Vec::new(),
        skipped_features: Vec::new(),
        issues,
    }
}

/// Read one carried folder with Horizon's own loader. This is the same
/// `Package::read` / `Content::read` the game uses, so HFC installs nothing
/// the engine would refuse.
fn verify_carry(dir: &Path, expected_id: &str) -> std::result::Result<(), String> {
    let package = horizon_mods::Package::read(dir).map_err(|error| format!("manifest: {error}"))?;
    if package.manifest.id != expected_id {
        return Err(format!(
            "manifest id {:?} does not match the carried id {expected_id:?}",
            package.manifest.id
        ));
    }
    if package.wasm.is_some() {
        return Err("a carried content mod unexpectedly ships code".into());
    }
    if package
        .manifest
        .capabilities
        .contains(&horizon_mods::Capability::Registries)
    {
        horizon_mods::Content::read(dir, &package.manifest)
            .map_err(|error| format!("content: {error}"))?;
    }
    Ok(())
}

fn read_check(entry: &CachedEntry) -> Result<horizon_port_pack::check::Check> {
    let value = entry.check_json()?;
    serde_json::from_value(value).map_err(|e| {
        Error::new(
            entry.check_path().display().to_string(),
            format!("cached check.json is not readable: {e}"),
        )
    })
}

fn verdict_label(check: &horizon_port_pack::check::Check) -> String {
    check.verdict.label().to_owned()
}

/// Horizon folder mod ids are `[a-z0-9_]{1,64}`; the carry pipeline refuses
/// anything else, so HFC reports it instead of installing a broken folder.
pub fn is_carry_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carry_ids_match_the_pipeline() {
        assert!(is_carry_id("ruby"));
        assert!(is_carry_id("ruby_2"));
        assert!(!is_carry_id("fabric-api"));
        assert!(!is_carry_id("Ruby"));
        assert!(!is_carry_id(""));
    }

    #[test]
    fn verify_carry_catches_invalid_packages() {
        let dir = std::env::temp_dir().join(format!("hfc-verify-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Missing manifest
        assert!(verify_carry(&dir, "ruby").is_err());

        // ID mismatch
        fs::write(
            dir.join("horizon-mod.toml"),
            "id = \"other\"\nname = \"Other\"\nversion = \"1.0.0\"\napi = \"0.2\"\ncapabilities = [\"registries\"]\n",
        )
        .unwrap();
        let err = verify_carry(&dir, "ruby").unwrap_err();
        assert!(err.contains("does not match"), "{err}");

        // Has unexpected wasm code
        fs::write(
            dir.join("horizon-mod.toml"),
            "id = \"ruby\"\nname = \"Ruby\"\nversion = \"1.0.0\"\napi = \"0.2\"\ncapabilities = [\"registries\"]\n",
        )
        .unwrap();
        fs::write(dir.join("mod.wasm"), b"fake wasm").unwrap();
        let err = verify_carry(&dir, "ruby").unwrap_err();
        assert!(err.contains("ships code"), "{err}");

        let _ = fs::remove_dir_all(&dir);
    }
}
