//! Class loading from JAR archives and in-memory class bytes.

use crate::class::Class;
use crate::jimage::JImage;
use hfc_core::{Error, Limits, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use zip::ZipArchive;

/// Outcome of walking a mod's transitive class references.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ClosureReport {
    pub resolved: Vec<String>,
    pub missing: Vec<String>,
    pub truncated: bool,
}

const MAX_CLOSURE_CLASSES: usize = 50_000;

#[derive(Clone, Copy)]
enum ClassLocation {
    TopLevel(usize),
    Nested(usize),
}

/// An open JAR, kept open so a walk over thousands of classes does not reopen
/// and re-parse the archive's central directory for every class.
struct JarSource {
    path: PathBuf,
    archive: ZipArchive<File>,
}

struct NestedSource {
    name: String,
    archive: ZipArchive<Cursor<Vec<u8>>>,
}

/// Manages class discovery and bytecode caching across loaded mod JARs.
pub struct ClassLoader {
    jars: Vec<JarSource>,
    nested_archives: Vec<NestedSource>,
    jimages: Vec<JImage>,
    class_index: HashMap<String, ClassLocation>,
    classes: HashMap<String, Class>,
    limits: Limits,
}

impl Default for ClassLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl ClassLoader {
    pub fn new() -> Self {
        Self {
            jars: Vec::new(),
            nested_archives: Vec::new(),
            jimages: Vec::new(),
            class_index: HashMap::new(),
            classes: HashMap::new(),
            limits: Limits::default(),
        }
    }

    pub fn with_limits(limits: Limits) -> Self {
        Self {
            jars: Vec::new(),
            nested_archives: Vec::new(),
            jimages: Vec::new(),
            class_index: HashMap::new(),
            classes: HashMap::new(),
            limits,
        }
    }

    /// Register a JDK image (`lib/modules`) as a class source. The JDK's own
    /// classes are then interpreted from the same bytecode a JVM would run.
    pub fn add_jimage(&mut self, path: impl AsRef<Path>) -> Result<()> {
        self.jimages.push(JImage::open(path)?);
        Ok(())
    }

    /// Whether any JDK image was registered.
    pub fn has_jimage(&self) -> bool {
        !self.jimages.is_empty()
    }

    /// Register a JAR file to search for classes and index its contents (including nested JARs).
    pub fn add_jar(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let meta = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
        if meta.len() > self.limits.max_jar_bytes {
            return Err(Error::new(
                path.display().to_string(),
                format!(
                    "JAR size {} exceeds maximum allowed {}",
                    meta.len(),
                    self.limits.max_jar_bytes
                ),
            ));
        }
        let jar_idx = self.jars.len();

        let file = File::open(path).map_err(|e| Error::io(path, e))?;
        let mut archive = ZipArchive::new(file).map_err(|e| {
            Error::new(
                path.display().to_string(),
                format!("failed to open zip: {e}"),
            )
        })?;

        let mut nested_to_read = Vec::new();

        for i in 0..archive.len() {
            if let Ok(entry) = archive.by_index(i) {
                if let Ok(name_cow) = entry.name() {
                    let name = name_cow.to_string();
                    if name.ends_with(".class") {
                        let class_name = name.trim_end_matches(".class").to_string();
                        self.class_index
                            .entry(class_name)
                            .or_insert(ClassLocation::TopLevel(jar_idx));
                    } else if name.ends_with(".jar") && !name.contains("..") {
                        nested_to_read.push((name, entry.size()));
                    }
                }
            }
        }

        for (nested_name, size) in nested_to_read {
            if size <= self.limits.max_jar_bytes {
                if let Ok(mut entry) = archive.by_name(&nested_name) {
                    let mut bytes = Vec::with_capacity(size as usize);
                    if entry.read_to_end(&mut bytes).is_ok() {
                        let nested_idx = self.nested_archives.len();
                        if let Ok(mut sub_archive) = ZipArchive::new(Cursor::new(bytes)) {
                            for j in 0..sub_archive.len() {
                                if let Ok(sub_entry) = sub_archive.by_index(j) {
                                    if let Ok(sub_cow) = sub_entry.name() {
                                        if sub_cow.ends_with(".class") {
                                            let class_name =
                                                sub_cow.trim_end_matches(".class").to_string();
                                            self.class_index
                                                .entry(class_name)
                                                .or_insert(ClassLocation::Nested(nested_idx));
                                        }
                                    }
                                }
                            }
                            self.nested_archives.push(NestedSource {
                                name: nested_name,
                                archive: sub_archive,
                            });
                        }
                    }
                }
            }
        }

        self.jars.push(JarSource {
            path: path.to_path_buf(),
            archive,
        });
        Ok(())
    }

    /// Add a pre-parsed class directly to the loader.
    pub fn insert_class(&mut self, class: Class) {
        self.classes.insert(class.this_class.clone(), class);
    }

    /// Load raw class bytes directly.
    pub fn load_class_bytes(&mut self, bytes: &[u8]) -> Result<&Class> {
        let class = Class::from_bytes(bytes)?;
        let name = class.this_class.clone();
        self.classes.insert(name.clone(), class);
        Ok(self.classes.get(&name).expect("just inserted"))
    }

    /// Check if a class is already loaded or available in registered JARs.
    pub fn contains_class(&mut self, name: &str) -> bool {
        self.classes.contains_key(name) || self.has_class(name)
    }

    /// Retrieve a class, loading it from JARs if not yet loaded.
    pub fn get_or_load(&mut self, name: &str) -> Result<Option<&Class>> {
        if !self.classes.contains_key(name) {
            let _ = self.find_and_load_class(name);
        }
        Ok(self.classes.get(name))
    }

    /// Retrieve a mutable reference to a class.
    pub fn get_or_load_mut(&mut self, name: &str) -> Result<Option<&mut Class>> {
        if !self.classes.contains_key(name) {
            let _ = self.find_and_load_class(name);
        }
        Ok(self.classes.get_mut(name))
    }

    /// Get an already loaded class.
    pub fn get(&self, name: &str) -> Option<&Class> {
        self.classes.get(name)
    }

    /// Get an already loaded mutable class.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Class> {
        self.classes.get_mut(name)
    }

    /// The classes reached from `roots` through their constant pools. The walk
    /// reads raw class bytes rather than parsing every class, so it can cover
    /// an entire Minecraft installation at startup.
    pub fn closure(&mut self, roots: impl IntoIterator<Item = String>) -> ClosureReport {
        let mut seen = std::collections::HashSet::new();
        let mut queue: std::collections::VecDeque<String> = roots.into_iter().collect();
        let mut report = ClosureReport::default();
        while let Some(name) = queue.pop_front() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if report.resolved.len() + report.missing.len() >= MAX_CLOSURE_CLASSES {
                report.truncated = true;
                break;
            }
            if !self.has_class(&name) {
                report.missing.push(name);
                continue;
            }
            match self.read_class_bytes(&name) {
                Ok(bytes) => {
                    report.resolved.push(name);
                    for reference in crate::class::scan_constant_pool_classes(&bytes) {
                        if !seen.contains(&reference) {
                            queue.push_back(reference);
                        }
                    }
                }
                Err(_) => report.missing.push(name),
            }
        }
        report
    }

    /// Find and parse a class from registered JARs (including nested JARs) or
    /// from a registered JDK image.
    fn find_and_load_class(&mut self, name: &str) -> Result<()> {
        let bytes = self.read_class_bytes(name)?;
        let class = Class::from_bytes(&bytes)?;
        self.classes.insert(name.to_string(), class);
        Ok(())
    }

    /// Whether a class exists in a registered JAR, nested JAR, or JDK image.
    /// Unlike `contains_class`, this never parses it.
    pub fn has_class(&self, name: &str) -> bool {
        self.class_index.contains_key(name)
            || self.jimages.iter().any(|image| image.contains_class(name))
    }

    /// Read a class file's raw bytes without parsing it.
    pub fn read_class_bytes(&mut self, name: &str) -> Result<Vec<u8>> {
        let loc = match self.class_index.get(name) {
            Some(&l) => l,
            None => {
                for image in &self.jimages {
                    if image.contains_class(name) {
                        let bytes = image.class_bytes(name)?;
                        if bytes.len() as u64 > self.limits.max_metadata_bytes {
                            return Err(Error::new(
                                format!("jimage:{name}"),
                                "class file exceeds limit",
                            ));
                        }
                        return Ok(bytes);
                    }
                }
                return Err(Error::new("loader", format!("class not found: {name}")));
            }
        };

        let filename = format!("{name}.class");

        match loc {
            ClassLocation::TopLevel(jar_idx) => {
                let jar = &mut self.jars[jar_idx];
                if let Ok(mut zip_file) = jar.archive.by_name(&filename) {
                    let size = zip_file.size();
                    if size > self.limits.max_metadata_bytes {
                        return Err(Error::new(
                            format!("{}:{filename}", jar.path.display()),
                            "class file exceeds limit",
                        ));
                    }
                    let mut bytes = Vec::with_capacity(size as usize);
                    zip_file.read_to_end(&mut bytes).map_err(|e| {
                        Error::new(
                            format!("{}:{filename}", jar.path.display()),
                            format!("read error: {e}"),
                        )
                    })?;
                    return Ok(bytes);
                }
            }
            ClassLocation::Nested(nested_idx) => {
                let nested = &mut self.nested_archives[nested_idx];
                if let Ok(mut zip_file) = nested.archive.by_name(&filename) {
                    let size = zip_file.size();
                    if size > self.limits.max_metadata_bytes {
                        return Err(Error::new(
                            format!("{}:{filename}", nested.name),
                            "class file exceeds limit",
                        ));
                    }
                    let mut class_bytes = Vec::with_capacity(size as usize);
                    zip_file.read_to_end(&mut class_bytes).map_err(|e| {
                        Error::new(
                            format!("{}:{filename}", nested.name),
                            format!("read error: {e}"),
                        )
                    })?;
                    return Ok(class_bytes);
                }
            }
        }

        Err(Error::new("loader", format!("class not found: {name}")))
    }
}
