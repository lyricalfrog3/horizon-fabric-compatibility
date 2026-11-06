//! The file system boundary mods see through `java.nio.file`.
//!
//! A host sets a root directory (the game directory); paths under it read and
//! write the real disk, so a mod's config survives restarts. Without a root
//! everything stays in memory, so tests and tools never touch the disk. A
//! path outside the root is never opened: the mod sees it as missing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Vfs {
    root: Option<PathBuf>,
    files: BTreeMap<String, Vec<u8>>,
    dirs: BTreeSet<String>,
}

/// Join and normalize `.`/`..` segments; the result uses `/` and no trailing slash.
pub fn join(base: &str, child: &str) -> String {
    let joined = if child.starts_with('/') || base.is_empty() {
        child.to_string()
    } else {
        format!("{base}/{child}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let absolute = joined.starts_with('/');
    format!("{}{}", if absolute { "/" } else { "" }, parts.join("/"))
}

pub fn parent(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    let at = trimmed.rfind('/')?;
    Some(if at == 0 {
        "/".into()
    } else {
        trimmed[..at].to_string()
    })
}

pub fn file_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

impl Vfs {
    pub fn set_root(&mut self, root: Option<PathBuf>) {
        self.root = root;
    }

    /// Every in-memory file, for tests and diagnostics.
    pub fn memory_files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// The real location of `path`, when the disk is in play and `path` is inside
    /// the root. A relative path is the mod's working directory's, and that is
    /// the game folder (the root), as it is for a JVM process started there.
    fn real(&self, path: &str) -> Option<PathBuf> {
        let root = self.root.as_ref()?;
        let normalized = join("", path);
        let candidate = if normalized.starts_with('/') {
            PathBuf::from(&normalized)
        } else {
            root.join(&normalized)
        };
        candidate.starts_with(root).then_some(candidate)
    }

    fn disk(&self) -> bool {
        self.root.is_some()
    }

    pub fn is_file(&self, path: &str) -> bool {
        if self.disk() {
            return self.real(path).is_some_and(|p| p.is_file());
        }
        self.files.contains_key(&join("", path))
    }

    /// Whether `path` is the root itself or one of its ancestors. The file
    /// system above the sandbox exists (a JVM reports `/` and the folders
    /// leading to a game directory), but nothing outside the root is listed or
    /// read, so a mod walking up from its own config folder stops there
    /// instead of recursing over a path that "does not exist".
    pub fn is_root_ancestor(&self, path: &str) -> bool {
        let normalized = join("", path);
        if normalized == "/" {
            return true;
        }
        let Some(root) = self.root.as_ref() else {
            return false;
        };
        let root_text = join("", &root.to_string_lossy());
        root_text == normalized || root_text.starts_with(&format!("{normalized}/"))
    }

    pub fn is_dir(&self, path: &str) -> bool {
        let key = join("", path);
        if self.disk() {
            return self.real(&key).is_some_and(|p| p.is_dir()) || self.is_root_ancestor(&key);
        }
        self.dirs.contains(&key)
            || self
                .files
                .keys()
                .any(|file| file.starts_with(&format!("{key}/")))
            || self.is_root_ancestor(&key)
    }

    pub fn exists(&self, path: &str) -> bool {
        self.is_file(path) || self.is_dir(path)
    }

    pub fn read(&self, path: &str) -> Option<Vec<u8>> {
        if self.disk() {
            return std::fs::read(self.real(path)?).ok();
        }
        self.files.get(&join("", path)).cloned()
    }

    pub fn write(&mut self, path: &str, bytes: &[u8], append: bool) -> bool {
        if self.disk() {
            let Some(real) = self.real(path) else {
                return false;
            };
            if let Some(dir) = real.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if !append {
                // A truncating write replaces the file: write a temporary
                // sibling and rename it into place, so a crash never leaves a
                // half-written config.
                return hfc_core::atomic_write(&real, bytes).is_ok();
            }
            use std::io::Write;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(true)
                .open(real);
            return file.and_then(|mut f| f.write_all(bytes)).is_ok();
        }
        let entry = self.files.entry(join("", path)).or_default();
        if !append {
            entry.clear();
        }
        entry.extend_from_slice(bytes);
        true
    }

    pub fn create_dirs(&mut self, path: &str) -> bool {
        if self.disk() {
            return self
                .real(path)
                .is_some_and(|p| std::fs::create_dir_all(p).is_ok());
        }
        let mut current = String::new();
        for part in join("", path).split('/').filter(|p| !p.is_empty()) {
            current = format!("{current}/{part}");
            self.dirs.insert(current.clone());
        }
        true
    }

    pub fn delete(&mut self, path: &str) -> bool {
        if self.disk() {
            return self.real(path).is_some_and(|p| {
                std::fs::remove_file(&p)
                    .or_else(|_| std::fs::remove_dir(&p))
                    .is_ok()
            });
        }
        let key = join("", path);
        self.files.remove(&key).is_some() | self.dirs.remove(&key)
    }

    /// Names (not full paths) directly inside `path`, sorted.
    pub fn list(&self, path: &str) -> Vec<String> {
        if self.disk() {
            let mut names: Vec<String> = self
                .real(path)
                .and_then(|p| std::fs::read_dir(p).ok())
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            return names;
        }
        let prefix = format!("{}/", join("", path));
        let mut names = BTreeSet::new();
        for key in self.files.keys().chain(self.dirs.iter()) {
            if let Some(rest) = key.strip_prefix(&prefix) {
                names.insert(rest.split('/').next().unwrap_or(rest).to_string());
            }
        }
        names.into_iter().collect()
    }
}

/// What `java.io.File` asks about a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub is_file: bool,
    pub is_dir: bool,
    pub len: u64,
    /// Milliseconds since the epoch; 0 when the file system has none (memory).
    pub modified_ms: i64,
    pub hidden: bool,
    pub readable: bool,
    pub writable: bool,
}

fn ancestor_meta() -> Meta {
    Meta {
        is_file: false,
        is_dir: true,
        len: 0,
        modified_ms: 0,
        hidden: false,
        readable: true,
        writable: false,
    }
}

impl Vfs {
    /// The canonical form of `path`: `.`/`..` resolved, and the part that
    /// exists on disk resolved through symlinks. A path that leaves the root
    /// (or cannot be resolved) keeps its lexical form, so a mod never gains
    /// access outside the game folder through it. Never `None`: a JVM can
    /// canonicalize a path that does not exist yet.
    pub fn canonicalize(&self, path: &str) -> String {
        let normalized = join("", path);
        if !self.disk() {
            return normalized;
        }
        let Some(real) = self.real(&normalized) else {
            return normalized;
        };
        let mut probe = real.clone();
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        let resolved = loop {
            match probe.canonicalize() {
                Ok(c) => break Some(c),
                Err(_) => match (probe.parent(), probe.file_name()) {
                    (Some(parent), Some(name)) => {
                        tail.push(name.to_os_string());
                        probe = parent.to_path_buf();
                    }
                    _ => break None,
                },
            }
        };
        let root = self.root.as_ref().and_then(|r| r.canonicalize().ok());
        let (Some(mut resolved), Some(root)) = (resolved, root) else {
            return normalized;
        };
        for name in tail.iter().rev() {
            resolved.push(name);
        }
        if resolved.starts_with(&root) {
            resolved.to_string_lossy().to_string()
        } else {
            normalized
        }
    }

    /// One directory level, as `File.mkdir` asks: the parent has to exist.
    pub fn create_dir(&mut self, path: &str) -> bool {
        let key = join("", path);
        if key.is_empty() || key == "/" {
            return false;
        }
        if self.disk() {
            return self
                .real(&key)
                .is_some_and(|p| std::fs::create_dir(p).is_ok());
        }
        if self.exists(&key) {
            return false;
        }
        let parent = parent(&key).unwrap_or_default();
        if !parent.is_empty() && parent != "/" && !self.is_dir(&parent) {
            return false;
        }
        self.dirs.insert(key)
    }

    /// `File.createNewFile`: fail when the file (or a directory) is already there.
    pub fn create_file_exclusive(&mut self, path: &str) -> bool {
        let key = join("", path);
        if self.disk() {
            let Some(real) = self.real(&key) else {
                return false;
            };
            return std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(real)
                .is_ok();
        }
        if self.exists(&key) {
            return false;
        }
        self.files.insert(key, Vec::new());
        true
    }

    /// `File.renameTo`: within the root only, and the target's parent must exist.
    pub fn rename(&mut self, from: &str, to: &str) -> bool {
        let (from, to) = (join("", from), join("", to));
        if self.disk() {
            let (Some(from), Some(to)) = (self.real(&from), self.real(&to)) else {
                return false;
            };
            return std::fs::rename(from, to).is_ok();
        }
        if !self.exists(&from) || self.exists(&to) {
            return false;
        }
        if let Some(parent) = parent(&to)
            && parent != "/"
            && !self.is_dir(&parent)
        {
            return false;
        }
        if let Some(bytes) = self.files.remove(&from) {
            self.files.insert(to, bytes);
            return true;
        }
        if !self.dirs.remove(&from) {
            return false;
        }
        let prefix = format!("{from}/");
        let files: Vec<String> = self
            .files
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .cloned()
            .collect();
        for key in files {
            let rest = key[from.len()..].to_string();
            if let Some(bytes) = self.files.remove(&key) {
                self.files.insert(format!("{to}{rest}"), bytes);
            }
        }
        let dirs: Vec<String> = self
            .dirs
            .iter()
            .filter(|key| key.starts_with(&prefix))
            .cloned()
            .collect();
        for key in dirs {
            let rest = key[from.len()..].to_string();
            self.dirs.remove(&key);
            self.dirs.insert(format!("{to}{rest}"));
        }
        self.dirs.insert(to);
        true
    }

    /// `File.exists`/`isFile`/`isDirectory`/`length`/`lastModified`/`canRead`/
    /// `canWrite` in one look, or `None` when nothing is there.
    pub fn metadata(&self, path: &str) -> Option<Meta> {
        let key = join("", path);
        if self.disk() {
            let Some(real) = self.real(&key) else {
                return self.is_root_ancestor(&key).then_some(ancestor_meta());
            };
            let meta = std::fs::metadata(&real).ok()?;
            let permissions = meta.permissions();
            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as i64);
            return Some(Meta {
                is_file: meta.is_file(),
                is_dir: meta.is_dir(),
                len: meta.len(),
                modified_ms,
                hidden: file_name(&key).starts_with('.'),
                readable: true,
                writable: !permissions.readonly(),
            });
        }
        if let Some(bytes) = self.files.get(&key) {
            return Some(Meta {
                is_file: true,
                is_dir: false,
                len: bytes.len() as u64,
                modified_ms: 0,
                hidden: file_name(&key).starts_with('.'),
                readable: true,
                writable: true,
            });
        }
        if self.is_root_ancestor(&key)
            && !self.dirs.contains(&key)
            && !self.files.contains_key(&key)
        {
            return Some(ancestor_meta());
        }
        let is_dir = self.dirs.contains(&key)
            || self
                .files
                .keys()
                .any(|file| file.starts_with(&format!("{key}/")));
        is_dir.then(|| Meta {
            is_file: false,
            is_dir: true,
            len: 0,
            modified_ms: 0,
            hidden: file_name(&key).starts_with('.'),
            readable: true,
            writable: true,
        })
    }

    /// `File.setLastModified`: real on disk, not tracked in memory.
    pub fn set_last_modified(&self, path: &str, millis: i64) -> bool {
        if !self.disk() {
            return self.exists(path);
        }
        let Some(real) = self.real(&join("", path)) else {
            return false;
        };
        let Ok(file) = std::fs::OpenOptions::new().write(true).open(&real) else {
            return false;
        };
        let time = std::time::UNIX_EPOCH
            .checked_add(std::time::Duration::from_millis(millis.max(0) as u64))
            .unwrap_or(std::time::UNIX_EPOCH);
        file.set_modified(time).is_ok()
    }

    /// `File.setReadOnly`.
    pub fn set_read_only(&self, path: &str) -> bool {
        if !self.disk() {
            return self.exists(path);
        }
        let Some(real) = self.real(&join("", path)) else {
            return false;
        };
        let Ok(meta) = std::fs::metadata(&real) else {
            return false;
        };
        let mut permissions = meta.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&real, permissions).is_ok()
    }

    /// `File.setReadable`/`setWritable`/`setExecutable` on a real file.
    pub fn set_permission(&self, path: &str, access: i32, enable: bool, owner_only: bool) -> bool {
        if !self.disk() {
            return self.exists(path);
        }
        let Some(real) = self.real(&join("", path)) else {
            return false;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let Ok(meta) = std::fs::metadata(&real) else {
                return false;
            };
            let mut mode = meta.permissions().mode();
            let mask = (if access & 0x04 != 0 { 0o4 } else { 0 })
                | (if access & 0x02 != 0 { 0o2 } else { 0 })
                | (if access & 0x01 != 0 { 0o1 } else { 0 });
            if mask == 0 {
                return false;
            }
            let bits = if owner_only {
                mask
            } else {
                mask | (mask << 3) | (mask << 6)
            };
            if enable {
                mode |= bits;
            } else {
                mode &= !bits;
            }
            std::fs::set_permissions(&real, std::fs::Permissions::from_mode(mode)).is_ok()
        }
        #[cfg(not(unix))]
        {
            let _ = (access, enable, owner_only);
            std::fs::metadata(&real).is_ok()
        }
    }

    /// Whether `path` may be opened for `access` (1 execute, 2 write, 4 read).
    pub fn check_access(&self, path: &str, access: i32) -> bool {
        let Some(meta) = self.metadata(path) else {
            return false;
        };
        if access & 0x04 != 0 && !meta.readable {
            return false;
        }
        if access & 0x02 != 0 && !meta.writable {
            return false;
        }
        if access & 0x01 != 0 {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let Some(real) = self.real(&join("", path)) else {
                    return false;
                };
                let Ok(disk) = std::fs::metadata(&real) else {
                    return false;
                };
                if disk.permissions().mode() & 0o111 == 0 {
                    return false;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_normalizes() {
        assert_eq!(join("/game/config", "a/../b.json"), "/game/config/b.json");
        assert_eq!(join("/game", "/etc"), "/etc");
        assert_eq!(parent("/game/config"), Some("/game".into()));
        assert_eq!(file_name("/game/config/x.json"), "x.json");
    }

    #[test]
    fn memory_files_round_trip() {
        let mut fs = Vfs::default();
        assert!(!fs.exists("/g/c/a.json"));
        assert!(fs.write("/g/c/a.json", b"{}", false));
        assert!(fs.write("/g/c/a.json", b"x", true));
        assert_eq!(fs.read("/g/c/a.json"), Some(b"{}x".to_vec()));
        assert!(fs.is_dir("/g/c") && fs.is_file("/g/c/a.json"));
        assert_eq!(fs.list("/g/c"), vec!["a.json".to_string()]);
        assert!(fs.delete("/g/c/a.json"));
        assert!(!fs.exists("/g/c/a.json"));
    }

    #[test]
    fn canonicalize_resolves_dots_and_the_root() {
        let dir = std::env::temp_dir().join(format!("hfc-vfs-canon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("game/config")).unwrap();
        let mut fs = Vfs::default();
        fs.set_root(Some(dir.join("game")));
        let root = dir.join("game").canonicalize().unwrap();
        let expected = root.join("config/a.json");
        let expected = expected.to_string_lossy().to_string();
        assert_eq!(
            fs.canonicalize("config/./x/../a.json"),
            expected,
            "a relative path resolves against the game folder"
        );
        assert_eq!(fs.canonicalize(&format!("{expected}/../a.json")), expected);
        // A path that leaves the root keeps its lexical form and gains no access.
        let outside = dir.join("outside.txt");
        let outside = outside.to_string_lossy().to_string();
        assert_eq!(fs.canonicalize(&outside), outside);
        assert!(!fs.exists(&outside));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn file_operations_report_what_the_disk_has() {
        let dir = std::env::temp_dir().join(format!("hfc-vfs-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut fs = Vfs::default();
        fs.set_root(Some(dir.clone()));
        let base = dir.to_string_lossy().to_string();

        // mkdirs is one level at a time; a missing parent fails as on a JVM.
        let sub = format!("{base}/a/b");
        assert!(!fs.create_dir(&sub));
        assert!(fs.create_dir(&format!("{base}/a")));
        assert!(fs.create_dir(&sub));
        assert!(
            !fs.create_dir(&sub),
            "an existing directory is not created again"
        );
        assert!(fs.is_dir(&sub));

        let file = format!("{sub}/notes.txt");
        assert!(fs.create_file_exclusive(&file));
        assert!(
            !fs.create_file_exclusive(&file),
            "an existing file is not created again"
        );
        assert!(fs.write(&file, b"hello", false));
        let meta = fs.metadata(&file).expect("the file exists");
        assert!(meta.is_file && !meta.is_dir);
        assert_eq!(meta.len, 5);
        assert!(fs.check_access(&file, 4) && !fs.check_access(&file, 1));

        let moved = format!("{sub}/moved.txt");
        assert!(fs.rename(&file, &moved));
        assert!(!fs.exists(&file) && fs.exists(&moved));
        assert_eq!(fs.list(&sub), vec!["moved.txt".to_string()]);
        assert!(fs.delete(&moved));
        assert!(fs.delete(&sub));

        // The ancestors of the game folder exist, so a walk up them ends; the
        // folders beside it are still invisible.
        assert!(fs.exists("/"));
        assert!(fs.is_dir(&base));
        assert!(!fs.exists("/etc"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn disk_access_stays_inside_the_root() {
        let dir = std::env::temp_dir().join(format!("hfc-vfs-{}", std::process::id()));
        let mut fs = Vfs::default();
        fs.set_root(Some(dir.clone()));
        let inside = dir.join("config/a.txt").to_string_lossy().to_string();
        assert!(fs.write(&inside, b"hi", false));
        assert_eq!(fs.read(&inside), Some(b"hi".to_vec()));
        assert!(!fs.write("/tmp/hfc-escape.txt", b"no", false));
        assert!(!fs.exists("/etc/passwd"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
