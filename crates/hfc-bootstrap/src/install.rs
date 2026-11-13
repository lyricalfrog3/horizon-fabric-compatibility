//! Writing and removing generated folder mods without ever touching a user's
//! own folder.
//!
//! Every folder HFC writes carries `.hfc-generated.json`. Removal checks that
//! marker first; a directory without it is treated as the user's and left
//! alone.
//!
//! Installation copies into a hidden staging sibling, writes the marker, then
//! swaps it into place:
//!
//! 1. `.hfc-install-<token>` is built completely (content and marker);
//! 2. an existing generated target is renamed to `.hfc-replace-<token>`;
//! 3. the staging folder is renamed onto the target path.
//!
//! Step 3 is the only moment the target name is free. A crash there is
//! repaired by [`recover`], which runs at the start of every run that is
//! allowed to install: an `.hfc-replace-*` folder whose mod id has no target is
//! renamed back, and one whose target exists and carries HFC's marker (the swap
//! finished) is deleted. Leftover `.hfc-install-*` staging directories with a
//! marker are deleted, because the carry cache still holds their content.
//! Neither prefix is ever treated as a mod folder: [`generated_folders`] skips
//! dot directories. See [`recover`] for the full rule.

use hfc_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const MARKER_FILE: &str = hfc_core::GENERATED_MARKER;
pub const MARKER_FORMAT: u32 = 1;

/// Prefix of a hidden folder being built for an install.
pub const STAGING_PREFIX: &str = ".hfc-install-";
/// Prefix of the hidden backup of the folder being replaced.
pub const BACKUP_PREFIX: &str = ".hfc-replace-";

/// The most files one generated folder may hold (a carried mod is data only).
const MAX_FILES: usize = 100_000;
const MAX_DEPTH: u8 = 24;
const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedMarker {
    pub format: u32,
    pub mod_id: String,
    pub source_file: String,
    pub source_sha256: String,
    pub hfc_version: String,
    pub compat_api: String,
    pub installed_unix: u64,
}

impl GeneratedMarker {
    pub fn new(mod_id: &str, source_file: &str, source_sha256: &str, installed_unix: u64) -> Self {
        Self {
            format: MARKER_FORMAT,
            mod_id: mod_id.to_owned(),
            source_file: source_file.to_owned(),
            source_sha256: source_sha256.to_owned(),
            hfc_version: hfc_core::HFC_VERSION.into(),
            compat_api: hfc_core::COMPAT_API.into(),
            installed_unix,
        }
    }

    pub fn matches(&self, other: &Self) -> bool {
        self.format == other.format
            && self.mod_id == other.mod_id
            && self.source_sha256 == other.source_sha256
            && self.hfc_version == other.hfc_version
            && self.compat_api == other.compat_api
    }
}

/// Read a folder's generation marker, if it has a valid one.
pub fn read_marker(folder: &Path) -> Option<GeneratedMarker> {
    let bytes = fs::read(folder.join(MARKER_FILE)).ok()?;
    let marker: GeneratedMarker = serde_json::from_slice(&bytes).ok()?;
    (marker.format == MARKER_FORMAT && hfc_core::safe_file_name(&marker.mod_id)).then_some(marker)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallResult {
    Installed,
    Reused,
}

/// Copy `source` into `target` as a generated folder, replacing an earlier
/// generated folder only. Returns [`InstallResult::Reused`] when the target
/// already holds exactly this source.
pub fn ensure_installed(
    source: &Path,
    target: &Path,
    wanted: &GeneratedMarker,
) -> Result<InstallResult> {
    if !hfc_core::safe_file_name(&wanted.mod_id) {
        return Err(Error::new(
            target.display().to_string(),
            format!("unsafe generated mod id {:?}", wanted.mod_id),
        ));
    }
    if target.exists() {
        match read_marker(target) {
            Some(existing) if existing.matches(wanted) => return Ok(InstallResult::Reused),
            Some(_) => {}
            None => {
                return Err(Error::new(
                    target.display().to_string(),
                    "the folder exists and was not created by HFC; leaving it untouched",
                ));
            }
        }
    }
    let parent = target.parent().ok_or_else(|| {
        Error::new(
            target.display().to_string(),
            "target has no parent directory",
        )
    })?;
    fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    let token = format!("{}-{}-{}", std::process::id(), wanted.installed_unix, {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    });
    let staging = parent.join(format!("{STAGING_PREFIX}{token}"));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|e| Error::io(&staging, e))?;

    let result = (|| {
        // The marker goes in first: a folder with it is one HFC created, so a
        // crash halfway through the copy leaves something [`recover`] may
        // delete rather than mistaking for a player's folder.
        hfc_core::atomic_write_json(&staging.join(MARKER_FILE), wanted)?;
        let mut budget = CopyBudget { files: 0, bytes: 0 };
        copy_dir(source, &staging, 0, &mut budget)?;
        Ok::<(), Error>(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    let backup = parent.join(format!("{BACKUP_PREFIX}{token}"));
    let had_old = target.exists();
    if had_old {
        // A directory rename cannot atomically replace a non-empty directory
        // on any supported platform, so this window is unavoidable. It is
        // bounded to a single rename and repaired by [`recover`] if the
        // process dies inside it.
        fs::rename(target, &backup).map_err(|e| Error::io(target, e))?;
    }
    if let Err(e) = fs::rename(&staging, target) {
        if had_old {
            let _ = fs::rename(&backup, target);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(Error::io(target, e));
    }
    if had_old {
        let _ = fs::remove_dir_all(&backup);
    }
    Ok(InstallResult::Installed)
}

/// Finish or roll back an install that a crash interrupted, and sweep stale
/// staging folders. Safe to call on every run; a quiet folder produces no
/// notes.
///
/// The recovery rule for the hidden swap folders, in order:
///
/// - `.hfc-replace-<token>` with a readable marker whose mod id has **no**
///   target folder: the new folder never arrived, so the backup is renamed
///   back to the target path (roll back), leaving the last good install.
/// - `.hfc-replace-<token>` whose target folder exists **and carries HFC's
///   marker**: the swap completed and only the deletion of the backup is
///   missing, so the backup is deleted.
/// - `.hfc-replace-<token>` whose target exists without HFC's marker: the
///   folder at the target is the player's, so the backup is left alone with a
///   note rather than deleting the last generated folder HFC had.
/// - `.hfc-replace-<token>` without a marker: not one of HFC's, left alone.
/// - `.hfc-install-<token>` with a marker: an interrupted copy, deleted (the
///   carry cache still has its content and the next run rebuilds it).
/// - `.hfc-install-<token>` without a marker but empty: deleted.
/// - `.hfc-install-<token>` without a marker and not empty: left alone with a
///   note, because HFC cannot prove it created it.
pub fn recover(mods_dir: &Path) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    let entries = match fs::read_dir(mods_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(notes),
        Err(e) => return Err(Error::io(mods_dir, e)),
    };
    let mut staging = Vec::new();
    let mut backups = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(mods_dir, e))?;
        let path = entry.path();
        if !entry.file_type().map_err(|e| Error::io(&path, e))?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(BACKUP_PREFIX) {
            backups.push(path);
        } else if name.starts_with(STAGING_PREFIX) {
            staging.push(path);
        }
    }
    // Backups first: restoring a target must happen before a stale staging
    // folder is swept, so a note below names the folder that survived.
    for path in backups {
        let Some(marker) = read_marker(&path) else {
            notes.push(format!(
                "left {} in place: it is hidden and has no HFC marker",
                path.display()
            ));
            continue;
        };
        let target = mods_dir.join(&marker.mod_id);
        if !target.exists() {
            fs::rename(&path, &target).map_err(|e| Error::io(&path, e))?;
            notes.push(format!(
                "rolled back an interrupted install of {}; its previous folder was restored",
                marker.mod_id
            ));
        } else if read_marker(&target).is_some() {
            fs::remove_dir_all(&path).map_err(|e| Error::io(&path, e))?;
            notes.push(format!(
                "removed an interrupted install's backup of {}; {} is in place",
                marker.mod_id, marker.mod_id
            ));
        } else {
            notes.push(format!(
                "left {} in place: {} exists but was not created by HFC, so the backup may \
                 still be the only generated copy",
                path.display(),
                target.display()
            ));
        }
    }
    for path in staging {
        match read_marker(&path) {
            Some(marker) => {
                fs::remove_dir_all(&path).map_err(|e| Error::io(&path, e))?;
                notes.push(format!(
                    "removed stale install staging for {} left by an interrupted run",
                    marker.mod_id
                ));
            }
            None if is_empty_dir(&path) => {
                fs::remove_dir_all(&path).map_err(|e| Error::io(&path, e))?;
                notes.push(format!(
                    "removed an empty install staging folder {}",
                    path.display()
                ));
            }
            None => notes.push(format!(
                "left {} in place: it is hidden and has no HFC marker",
                path.display()
            )),
        }
    }
    Ok(notes)
}

fn is_empty_dir(path: &Path) -> bool {
    fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
}

/// Whether [`recover`] would touch anything in `mods_dir`. Used by a dry run to
/// say that a real run has repair work, without doing it.
pub fn pending_recovery(mods_dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(mods_dir) else {
        return false;
    };
    entries.filter_map(|entry| entry.ok()).any(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        name.starts_with(BACKUP_PREFIX) || name.starts_with(STAGING_PREFIX)
    })
}

/// Remove a generated folder. Returns false when the folder is missing or was
/// not generated by HFC (in which case it is left untouched).
pub fn remove_generated(target: &Path, id: &str) -> Result<bool> {
    if !target.exists() {
        return Ok(false);
    }
    match read_marker(target) {
        Some(marker) if marker.mod_id == id => {
            fs::remove_dir_all(target).map_err(|e| Error::io(target, e))?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Every generated folder directly in `mods_dir`.
///
/// Dot-prefixed directories are skipped: they are HFC's own staging and swap
/// folders ([`STAGING_PREFIX`], [`BACKUP_PREFIX`]), and a leftover backup
/// carries a marker, so enumerating it would turn a stale copy into a second
/// "generated mod" that cleanup could keep beside the real one.
pub fn generated_folders(mods_dir: &Path) -> Result<Vec<(PathBuf, GeneratedMarker)>> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(mods_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(Error::io(mods_dir, e)),
    };
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(mods_dir, e))?;
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let kind = entry.file_type().map_err(|e| Error::io(&path, e))?;
        if !kind.is_dir() {
            continue;
        }
        if let Some(marker) = read_marker(&path) {
            out.push((path, marker));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

struct CopyBudget {
    files: usize,
    bytes: u64,
}

fn copy_dir(from: &Path, to: &Path, depth: u8, budget: &mut CopyBudget) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::new(
            from.display().to_string(),
            "generated folder nests too deeply",
        ));
    }
    for entry in fs::read_dir(from).map_err(|e| Error::io(from, e))? {
        let entry = entry.map_err(|e| Error::io(from, e))?;
        let kind = entry.file_type().map_err(|e| Error::io(&entry.path(), e))?;
        if kind.is_symlink() {
            return Err(Error::new(
                entry.path().display().to_string(),
                "generated folder holds a symlink; refusing to copy it",
            ));
        }
        let name = entry.file_name();
        let destination = to.join(&name);
        if kind.is_dir() {
            fs::create_dir(&destination).map_err(|e| Error::io(&destination, e))?;
            copy_dir(&entry.path(), &destination, depth + 1, budget)?;
        } else if kind.is_file() {
            budget.files += 1;
            if budget.files > MAX_FILES {
                return Err(Error::new(
                    from.display().to_string(),
                    "generated folder holds too many files",
                ));
            }
            let size = entry
                .metadata()
                .map_err(|e| Error::io(&entry.path(), e))?
                .len();
            budget.bytes = budget.bytes.saturating_add(size);
            if budget.bytes > MAX_BYTES {
                return Err(Error::new(
                    from.display().to_string(),
                    "generated folder is too large",
                ));
            }
            fs::copy(entry.path(), &destination).map_err(|e| Error::io(&destination, e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hfc-install-{name}-{}-{}",
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

    fn source(dir: &Path) -> PathBuf {
        let source = dir.join("carry");
        fs::create_dir_all(source.join("assets/x")).unwrap();
        fs::write(source.join("horizon-mod.toml"), "[mod]\nid='x'\n").unwrap();
        fs::write(source.join("assets/x/a.txt"), b"a").unwrap();
        source
    }

    #[test]
    fn installs_reuses_and_replaces_only_generated_folders() {
        let dir = temp("generated");
        let source = source(&dir);
        let target = dir.join("mods/x");
        let marker = GeneratedMarker::new("x", "x.jar", "sha", 1);
        assert_eq!(
            ensure_installed(&source, &target, &marker).unwrap(),
            InstallResult::Installed
        );
        assert!(target.join("assets/x/a.txt").is_file());
        assert_eq!(read_marker(&target).unwrap().mod_id, "x");
        assert_eq!(
            ensure_installed(&source, &target, &marker).unwrap(),
            InstallResult::Reused
        );
        let changed = GeneratedMarker::new("x", "x.jar", "sha2", 2);
        assert_eq!(
            ensure_installed(&source, &target, &changed).unwrap(),
            InstallResult::Installed
        );
        assert_eq!(read_marker(&target).unwrap().source_sha256, "sha2");
        assert!(remove_generated(&target, "x").unwrap());
        assert!(!target.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_to_overwrite_a_user_folder() {
        let dir = temp("user");
        let source = source(&dir);
        let target = dir.join("x");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("mine.txt"), b"mine").unwrap();
        let error = ensure_installed(
            &source,
            &target,
            &GeneratedMarker::new("x", "x.jar", "s", 1),
        )
        .unwrap_err();
        assert!(error.to_string().contains("not created by HFC"));
        assert!(target.join("mine.txt").is_file());
        assert!(!remove_generated(&target, "x").unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn generated_folders_only_lists_marked_directories() {
        let dir = temp("list");
        let target = dir.join("x");
        fs::create_dir_all(&target).unwrap();
        hfc_core::atomic_write_json(
            &target.join(MARKER_FILE),
            &GeneratedMarker::new("x", "x.jar", "s", 1),
        )
        .unwrap();
        fs::create_dir_all(dir.join("user")).unwrap();
        let found = generated_folders(&dir).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1.mod_id, "x");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_leftover_backup_is_not_enumerated_as_a_mod() {
        let dir = temp("backup-not-mod");
        // The real install, and the backup a crash left behind. Both carry a
        // valid marker with the same mod id.
        let target = dir.join("x");
        fs::create_dir_all(&target).unwrap();
        hfc_core::atomic_write_json(
            &target.join(MARKER_FILE),
            &GeneratedMarker::new("x", "x.jar", "s", 1),
        )
        .unwrap();
        let backup = dir.join(format!("{BACKUP_PREFIX}1234-1"));
        fs::create_dir_all(&backup).unwrap();
        hfc_core::atomic_write_json(
            &backup.join(MARKER_FILE),
            &GeneratedMarker::new("x", "x.jar", "old", 0),
        )
        .unwrap();

        let found = generated_folders(&dir).unwrap();
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].0, target);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_staging_folders_are_swept_and_unknown_hidden_folders_are_kept() {
        let dir = temp("sweep");
        // A staging folder from an interrupted copy of a marked mod.
        let staging = dir.join(format!("{STAGING_PREFIX}1234-1"));
        fs::create_dir_all(staging.join("assets")).unwrap();
        hfc_core::atomic_write_json(
            &staging.join(MARKER_FILE),
            &GeneratedMarker::new("x", "x.jar", "s", 1),
        )
        .unwrap();
        // An empty staging folder with no marker yet.
        let empty = dir.join(format!("{STAGING_PREFIX}1234-2"));
        fs::create_dir_all(&empty).unwrap();
        // A hidden folder HFC cannot prove it made: left alone, with a note.
        let unknown = dir.join(format!("{STAGING_PREFIX}1234-3"));
        fs::create_dir_all(&unknown).unwrap();
        fs::write(unknown.join("user.txt"), b"mine").unwrap();

        let notes = recover(&dir).unwrap();
        assert!(!staging.exists(), "marked staging was not swept");
        assert!(!empty.exists(), "empty staging was not swept");
        assert!(unknown.join("user.txt").is_file(), "user data was deleted");
        assert!(
            notes
                .iter()
                .any(|n| n.contains(STAGING_PREFIX) || n.contains("staging")),
            "{notes:#?}"
        );
        assert!(
            notes.iter().any(|n| n.contains("left")),
            "the unmarked folder must be reported, not silently kept: {notes:#?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crash_between_the_two_renames_is_recovered_on_the_next_run() {
        let dir = temp("recover-rollback");
        let source = source(&dir);
        let target = dir.join("x");
        assert_eq!(
            ensure_installed(
                &source,
                &target,
                &GeneratedMarker::new("x", "x.jar", "sha1", 1)
            )
            .unwrap(),
            InstallResult::Installed
        );
        let installed_sha = read_marker(&target).unwrap().source_sha256;

        // Simulate the crash window: the old folder was renamed to the backup,
        // and the process died before the staging rename.
        let backup = dir.join(format!("{BACKUP_PREFIX}999-1"));
        fs::rename(&target, &backup).unwrap();
        assert!(!target.exists());

        let notes = recover(&dir).unwrap();
        assert!(target.exists(), "the target path was left absent");
        assert_eq!(read_marker(&target).unwrap().source_sha256, installed_sha);
        assert!(!backup.exists());
        assert!(
            notes.iter().any(|n| n.contains("rolled back")),
            "{notes:#?}"
        );

        // The next install then replaces the recovered folder normally.
        assert_eq!(
            ensure_installed(
                &source,
                &target,
                &GeneratedMarker::new("x", "x.jar", "sha2", 2)
            )
            .unwrap(),
            InstallResult::Installed
        );
        assert_eq!(read_marker(&target).unwrap().source_sha256, "sha2");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(BACKUP_PREFIX) || name.starts_with(STAGING_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "swap folders left: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crash_after_the_swap_deletes_the_backup_not_the_install() {
        let dir = temp("recover-forward");
        let source = source(&dir);
        let target = dir.join("x");
        assert_eq!(
            ensure_installed(
                &source,
                &target,
                &GeneratedMarker::new("x", "x.jar", "new", 2)
            )
            .unwrap(),
            InstallResult::Installed
        );
        // The swap finished; only the backup deletion was lost.
        let backup = dir.join(format!("{BACKUP_PREFIX}999-1"));
        fs::create_dir_all(backup.join("assets")).unwrap();
        hfc_core::atomic_write_json(
            &backup.join(MARKER_FILE),
            &GeneratedMarker::new("x", "x.jar", "old", 1),
        )
        .unwrap();

        let notes = recover(&dir).unwrap();
        assert_eq!(read_marker(&target).unwrap().source_sha256, "new");
        assert!(!backup.exists());
        assert!(notes.iter().any(|n| n.contains("backup")), "{notes:#?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_never_restores_a_backup_over_someone_elses_folder() {
        let dir = temp("recover-user");
        // A player created `x` after the interrupted install. The backup is
        // the only generated copy left, so it must survive.
        let target = dir.join("x");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("mine.txt"), b"mine").unwrap();
        let backup = dir.join(format!("{BACKUP_PREFIX}999-1"));
        fs::create_dir_all(&backup).unwrap();
        hfc_core::atomic_write_json(
            &backup.join(MARKER_FILE),
            &GeneratedMarker::new("x", "x.jar", "old", 1),
        )
        .unwrap();

        let notes = recover(&dir).unwrap();
        assert!(target.join("mine.txt").is_file());
        assert!(backup.exists(), "the only generated copy was deleted");
        assert!(
            notes.iter().any(|n| n.contains("only generated copy")),
            "{notes:#?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
