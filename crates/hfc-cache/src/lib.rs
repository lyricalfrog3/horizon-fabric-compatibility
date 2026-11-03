//! A content-addressed cache for converted jars.
//!
//! One entry is one jar, converted once. Its directory name is derived from
//! every input that can change the result:
//!
//! - the jar's SHA-256;
//! - the Horizon version;
//! - HFC's compatibility API and pipeline version;
//! - the platform;
//! - a free-form carry profile (the target Minecraft version and any carry
//!   options that affect output).
//!
//! Changing any input produces a different key, so a stale entry is simply
//! never found instead of being silently reused. Entries are committed with a
//! directory rename, so an interrupted conversion leaves staging garbage that
//! [`Cache::sweep_staging`] can remove and never a half-valid entry.

use hfc_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// Bump when the layout of an entry itself changes.
pub const ENTRY_FORMAT: u32 = 1;

const ENTRY: &str = "entry.json";
const CHECK: &str = "check.json";
const STAGING: &str = ".hfc-staging";
/// A staging directory younger than this may belong to another live run.
/// Only abandoned ones are removed.
const STAGING_MAX_AGE_SECS: u64 = 24 * 60 * 60;

/// Everything outside the jar that decides what a conversion produces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyContext {
    pub horizon_version: String,
    pub compat_api: String,
    pub pipeline_version: u32,
    pub platform: String,
    /// Target Minecraft version / carry options, joined by the caller.
    pub carry_profile: String,
}

impl KeyContext {
    pub fn new(horizon_version: impl Into<String>, carry_profile: impl Into<String>) -> Self {
        Self {
            horizon_version: horizon_version.into(),
            compat_api: hfc_core::COMPAT_API.into(),
            pipeline_version: hfc_core::PIPELINE_VERSION,
            platform: hfc_core::platform(),
            carry_profile: carry_profile.into(),
        }
    }
}

/// The metadata written into every committed entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryMeta {
    pub format: u32,
    pub jar_sha256: String,
    pub horizon_version: String,
    pub compat_api: String,
    pub pipeline_version: u32,
    pub platform: String,
    pub carry_profile: String,
    pub created_unix: u64,
}

/// A cache entry that has been verified to match the wanted key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedEntry {
    pub dir: PathBuf,
    pub meta: EntryMeta,
}

impl CachedEntry {
    /// The `horizon-port-pack` carry output.
    pub fn carry_dir(&self) -> PathBuf {
        self.dir.join("carry")
    }

    pub fn check_path(&self) -> PathBuf {
        self.dir.join(CHECK)
    }

    /// Read the check JSON as a value: the cache crate deliberately does not
    /// depend on the port pipeline's types.
    pub fn check_json(&self) -> Result<serde_json::Value> {
        read_json(&self.check_path())
    }
}

#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory a jar's entries live under. The hash is validated so a
    /// caller cannot smuggle a path component in through it.
    fn jar_dir(&self, jar_sha256: &str) -> Result<PathBuf> {
        if !is_sha256(jar_sha256) {
            return Err(Error::new(
                "cache",
                format!("invalid jar hash {jar_sha256:?}"),
            ));
        }
        Ok(self.root.join(&jar_sha256[..2]).join(jar_sha256))
    }

    /// The final directory for one key.
    pub fn entry_dir(&self, jar_sha256: &str, context: &KeyContext) -> Result<PathBuf> {
        Ok(self.jar_dir(jar_sha256)?.join(key_id(context)))
    }

    /// Look up a complete, matching entry. A directory without valid
    /// metadata, with a mismatched key, or without `check.json` is a miss.
    pub fn lookup(&self, jar_sha256: &str, context: &KeyContext) -> Result<Option<CachedEntry>> {
        let dir = self.entry_dir(jar_sha256, context)?;
        let meta = match fs::read(dir.join(ENTRY)) {
            Ok(bytes) => serde_json::from_slice::<EntryMeta>(&bytes).ok(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(Error::io(&dir.join(ENTRY), e)),
        };
        let Some(meta) = meta else {
            return Ok(None);
        };
        if !meta_matches(&meta, jar_sha256, context) || !dir.join(CHECK).is_file() {
            return Ok(None);
        }
        Ok(Some(CachedEntry { dir, meta }))
    }

    /// A private staging directory for a conversion. The caller writes the
    /// conversion output there and then calls [`Cache::commit`].
    pub fn staging(&self) -> Result<PathBuf> {
        fs::create_dir_all(&self.root).map_err(|e| Error::io(&self.root, e))?;
        sweep_staging(&self.root);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = self
            .root
            .join(format!("{STAGING}-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        Ok(dir)
    }

    /// Commit a staging directory as the entry for `jar_sha256`/`context`.
    ///
    /// The staging directory must contain `check.json`; its other contents
    /// (usually `carry/`) are kept. If another process committed the same key
    /// first, this one is discarded and the existing entry is returned.
    pub fn commit(
        &self,
        staging: &Path,
        jar_sha256: &str,
        context: &KeyContext,
    ) -> Result<CachedEntry> {
        if !staging.join(CHECK).is_file() {
            return Err(Error::new(
                staging.display().to_string(),
                "conversion produced no check.json",
            ));
        }
        let target = self.entry_dir(jar_sha256, context)?;
        let created_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let meta = EntryMeta {
            format: ENTRY_FORMAT,
            jar_sha256: jar_sha256.to_owned(),
            horizon_version: context.horizon_version.clone(),
            compat_api: context.compat_api.clone(),
            pipeline_version: context.pipeline_version,
            platform: context.platform.clone(),
            carry_profile: context.carry_profile.clone(),
            created_unix,
        };
        hfc_core::atomic_write_json(&staging.join(ENTRY), &meta)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        if target.exists() {
            if let Some(existing) = self.lookup(jar_sha256, context)? {
                // A concurrent run won the race. Keep its entry.
                let _ = fs::remove_dir_all(staging);
                return Ok(existing);
            }
            // An incomplete or tampered entry from an earlier interrupted
            // run: replace it rather than fail forever.
            fs::remove_dir_all(&target).map_err(|e| Error::io(&target, e))?;
        }
        match fs::rename(staging, &target) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_dir_all(staging);
            }
            Err(e) => return Err(Error::io(&target, e)),
        }
        self.lookup(jar_sha256, context)?.ok_or_else(|| {
            Error::new(
                target.display().to_string(),
                "entry did not verify after commit",
            )
        })
    }

    /// Remove abandoned staging directories. Safe to call at any time: only
    /// names carrying the staging prefix are touched.
    pub fn sweep_staging(&self) {
        sweep_staging(&self.root);
    }

    /// Remove cache entries for jars whose SHA-256 is not in `keep_hashes`.
    /// Also sweeps abandoned staging and cleans up empty directories.
    pub fn prune(&self, keep_hashes: &std::collections::BTreeSet<String>) -> Result<PruneReport> {
        self.sweep_staging();
        let mut report = PruneReport::default();
        if !self.root.is_dir() {
            return Ok(report);
        }
        for shard in read_dirs(&self.root)? {
            for jar in read_dirs(&shard)? {
                let jar_hash = jar
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !keep_hashes.contains(&jar_hash) {
                    let bytes = dir_size(&jar)?;
                    let mut count = 0u64;
                    for entry in read_dirs(&jar)? {
                        if entry.join(ENTRY).is_file() {
                            count += 1;
                        }
                    }
                    if fs::remove_dir_all(&jar).is_ok() {
                        report.pruned_entries += count;
                        report.reclaimed_bytes += bytes;
                    }
                }
            }
            if fs::read_dir(&shard).is_ok_and(|mut it| it.next().is_none()) {
                let _ = fs::remove_dir(&shard);
            }
        }
        Ok(report)
    }

    /// Number and total size of committed entries, for `hfc status`.
    pub fn stats(&self) -> Result<CacheStats> {
        let mut stats = CacheStats::default();
        if !self.root.is_dir() {
            return Ok(stats);
        }
        for shard in read_dirs(&self.root)? {
            for jar in read_dirs(&shard)? {
                for entry in read_dirs(&jar)? {
                    if entry.join(ENTRY).is_file() {
                        stats.entries += 1;
                        stats.bytes = stats.bytes.saturating_add(dir_size(&entry)?);
                    }
                }
            }
        }
        Ok(stats)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheStats {
    pub entries: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneReport {
    pub pruned_entries: u64,
    pub reclaimed_bytes: u64,
}

fn read_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
        let entry = entry.map_err(|e| Error::io(dir, e))?;
        if entry
            .file_type()
            .map_err(|e| Error::io(&entry.path(), e))?
            .is_dir()
        {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

fn dir_size(dir: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
        let entry = entry.map_err(|e| Error::io(dir, e))?;
        let kind = entry.file_type().map_err(|e| Error::io(&entry.path(), e))?;
        if kind.is_dir() {
            total = total.saturating_add(dir_size(&entry.path())?);
        } else if kind.is_file() {
            total = total.saturating_add(
                entry
                    .metadata()
                    .map_err(|e| Error::io(&entry.path(), e))?
                    .len(),
            );
        }
    }
    Ok(total)
}

fn sweep_staging(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(STAGING) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age.as_secs() > STAGING_MAX_AGE_SECS);
        if old {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn read_json(path: &Path) -> Result<serde_json::Value> {
    let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::new(path.display().to_string(), e.to_string()))
}

fn meta_matches(meta: &EntryMeta, jar_sha256: &str, context: &KeyContext) -> bool {
    meta.format == ENTRY_FORMAT
        && meta.jar_sha256 == jar_sha256
        && meta.horizon_version == context.horizon_version
        && meta.compat_api == context.compat_api
        && meta.pipeline_version == context.pipeline_version
        && meta.platform == context.platform
        && meta.carry_profile == context.carry_profile
}

/// A short, collision-resistant id for one key. The full key is checked
/// again from `entry.json`, so even a truncated hash cannot cause a wrong hit.
fn key_id(context: &KeyContext) -> String {
    let joined = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        context.horizon_version,
        context.compat_api,
        context.pipeline_version,
        context.platform,
        context.carry_profile
    );
    hfc_core::sha256_bytes(joined.as_bytes())[..32].to_owned()
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hfc-cache-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn stage(cache: &Cache, marker: &str) -> PathBuf {
        let staging = cache.staging().unwrap();
        fs::create_dir_all(staging.join("carry")).unwrap();
        fs::write(staging.join(CHECK), format!("{{\"mod_id\":\"{marker}\"}}")).unwrap();
        fs::write(staging.join("carry/horizon-mod.toml"), b"[mod]\nid='x'\n").unwrap();
        staging
    }

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn stores_looks_up_and_reuses() {
        let root = temp("store");
        let cache = Cache::new(&root);
        let context = KeyContext::new("26.3", "mc=1.21.1");
        assert!(cache.lookup(SHA, &context).unwrap().is_none());
        let staging = stage(&cache, "ruby");
        let entry = cache.commit(&staging, SHA, &context).unwrap();
        assert!(entry.carry_dir().join("horizon-mod.toml").is_file());
        let found = cache.lookup(SHA, &context).unwrap().unwrap();
        assert_eq!(found.dir, entry.dir);
        assert_eq!(found.check_json().unwrap()["mod_id"], "ruby");
        assert_eq!(cache.stats().unwrap().entries, 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn any_key_change_misses() {
        let root = temp("invalidate");
        let cache = Cache::new(&root);
        let context = KeyContext::new("26.3", "mc=1.21.1");
        cache.commit(&stage(&cache, "x"), SHA, &context).unwrap();
        for changed in [
            KeyContext::new("26.4", "mc=1.21.1"),
            KeyContext::new("26.3", "mc=1.21.4"),
            KeyContext {
                compat_api: "0.2".into(),
                ..context.clone()
            },
            KeyContext {
                pipeline_version: context.pipeline_version + 1,
                ..context.clone()
            },
            KeyContext {
                platform: "other-arch".into(),
                ..context.clone()
            },
        ] {
            assert!(cache.lookup(SHA, &changed).unwrap().is_none());
        }
        assert!(cache.lookup(SHA, &context).unwrap().is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn incomplete_or_tampered_entries_are_misses() {
        let root = temp("tamper");
        let cache = Cache::new(&root);
        let context = KeyContext::new("26.3", "mc=1.21.1");
        let entry = cache.commit(&stage(&cache, "x"), SHA, &context).unwrap();
        fs::remove_file(entry.check_path()).unwrap();
        assert!(cache.lookup(SHA, &context).unwrap().is_none());

        let entry = cache.commit(&stage(&cache, "x"), SHA, &context).unwrap();
        let mut meta: EntryMeta =
            serde_json::from_slice(&fs::read(entry.dir.join(ENTRY)).unwrap()).unwrap();
        meta.pipeline_version += 1;
        fs::write(entry.dir.join(ENTRY), serde_json::to_vec(&meta).unwrap()).unwrap();
        assert!(cache.lookup(SHA, &context).unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn refuses_unsafe_hashes_and_sweeps_staging() {
        let root = temp("staging");
        let cache = Cache::new(&root);
        let context = KeyContext::new("26.3", "x");
        assert!(cache.lookup("../escape", &context).is_err());
        let stale = root.join(".hfc-staging-999");
        fs::create_dir_all(&stale).unwrap();
        let old = SystemTime::now() - std::time::Duration::from_secs(2 * STAGING_MAX_AGE_SECS);
        fs::File::open(&stale).unwrap().set_modified(old).unwrap();
        let live = root.join(".hfc-staging-live");
        fs::create_dir_all(&live).unwrap();
        let fresh = cache.staging().unwrap();
        assert!(!stale.exists());
        assert!(live.exists(), "a concurrent run's staging was removed");
        assert!(fresh.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_removes_unreferenced_jars_and_cleans_empty_dirs() {
        let root = temp("prune");
        let cache = Cache::new(&root);
        let context = KeyContext::new("26.3", "mc=1.21.1");
        let sha_keep = "1111111111111111111111111111111111111111111111111111111111111111";
        let sha_drop = "2222222222222222222222222222222222222222222222222222222222222222";
        cache
            .commit(&stage(&cache, "keep"), sha_keep, &context)
            .unwrap();
        cache
            .commit(&stage(&cache, "drop"), sha_drop, &context)
            .unwrap();
        assert_eq!(cache.stats().unwrap().entries, 2);

        let mut keep_set = std::collections::BTreeSet::new();
        keep_set.insert(sha_keep.to_string());
        let report = cache.prune(&keep_set).unwrap();
        assert_eq!(report.pruned_entries, 1);
        assert!(report.reclaimed_bytes > 0);
        assert_eq!(cache.stats().unwrap().entries, 1);
        assert!(cache.lookup(sha_keep, &context).unwrap().is_some());
        assert!(cache.lookup(sha_drop, &context).unwrap().is_none());

        // The "22" shard folder should be cleaned up
        assert!(!root.join("22").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn commit_without_check_fails() {
        let root = temp("nocheck");
        let cache = Cache::new(&root);
        let staging = cache.staging().unwrap();
        let mut file = fs::File::create(staging.join("other.txt")).unwrap();
        file.write_all(b"x").unwrap();
        assert!(
            cache
                .commit(&staging, SHA, &KeyContext::new("26.3", "x"))
                .is_err()
        );
        let _ = fs::remove_dir_all(&root);
    }
}
