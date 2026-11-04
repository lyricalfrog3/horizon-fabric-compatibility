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

    /// The real location of `path`, when the disk is in play and `path` is inside the root.
    fn real(&self, path: &str) -> Option<PathBuf> {
        let root = self.root.as_ref()?;
        let normalized = join("", path);
        let candidate = PathBuf::from(&normalized);
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

    pub fn is_dir(&self, path: &str) -> bool {
        if self.disk() {
            return self.real(path).is_some_and(|p| p.is_dir());
        }
        let key = join("", path);
        self.dirs.contains(&key)
            || self
                .files
                .keys()
                .any(|file| file.starts_with(&format!("{key}/")))
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
            use std::io::Write;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(append)
                .truncate(!append)
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
