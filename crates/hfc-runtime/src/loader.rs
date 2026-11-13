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

/// Read `reader` through a limit instead of trusting a declared size.
///
/// The declared size of a zip entry is attacker-controlled: an entry can claim
/// a kilobyte and expand to a gigabyte. Reading at most `limit + 1` bytes
/// bounds the allocation no matter what the entry says, and the extra byte is
/// how an over-limit entry is told apart from one that exactly fits. This is
/// the same shape as `hfc_fabric::archive`'s entry reader.
fn read_limited(reader: impl Read, limit: u64, context: impl Into<String>) -> Result<Vec<u8>> {
    let context = context.into();
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| Error::new(&context, format!("read error: {e}")))?;
    if bytes.len() as u64 > limit {
        return Err(Error::new(
            &context,
            format!(
                "entry expands past the {limit} byte limit (a declared size cannot bound this)"
            ),
        ));
    }
    Ok(bytes)
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

    /// The JAR files registered with the loader, in registration order. Their
    /// bytes are what the interpreter loads classes from, so anything cached
    /// per "classpath" is keyed by their hashes.
    pub fn jar_paths(&self) -> Vec<&Path> {
        self.jars.iter().map(|jar| jar.path.as_path()).collect()
    }

    /// The classes parsed so far, by binary name.
    pub(crate) fn classes(&self) -> impl Iterator<Item = (&str, &crate::class::Class)> {
        self.classes
            .iter()
            .map(|(name, class)| (name.as_str(), class))
    }

    /// Whether any loaded class's static field refers to a heap id `>= mark`.
    pub fn static_refs_into(&self, mark: usize) -> bool {
        self.classes.values().any(|class| {
            class
                .static_fields
                .values()
                .any(|value| value.as_obj().is_some_and(|id| id >= mark))
        })
    }

    /// The JDK images registered with the loader, in registration order.
    pub fn jimage_paths(&self) -> Vec<&Path> {
        self.jimages.iter().map(|image| image.path()).collect()
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
        // The uncompressed bytes of nested jars read for this archive, so the
        // total stays inside `max_uncompressed_bytes` and not just each entry.
        let mut nested_bytes_read: u64 = 0;

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
                    // The declared size is only a hint: the read is bounded by
                    // the per-jar limit *and* by what is left of the total
                    // across every nested jar, so a jar full of entries that
                    // each claim to fit cannot expand without bound.
                    let remaining = self
                        .limits
                        .max_uncompressed_bytes
                        .saturating_sub(nested_bytes_read);
                    let limit = self.limits.max_jar_bytes.min(remaining);
                    if limit == 0 {
                        break;
                    }
                    let Ok(bytes) = read_limited(
                        &mut entry,
                        limit,
                        format!("{}:{nested_name}", path.display()),
                    ) else {
                        continue;
                    };
                    nested_bytes_read += bytes.len() as u64;
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

    /// The text of every `path` entry in the registered jars and their nested
    /// jars, in registration order (a `META-INF/services/...` file, for one).
    pub fn text_resources(&mut self, path: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut read = |archive_entry: Result<Vec<u8>>| {
            if let Ok(bytes) = archive_entry {
                out.push(String::from_utf8_lossy(&bytes).into_owned());
            }
        };
        let limit = self.limits.max_metadata_bytes;
        let entry_bytes = |file: &mut dyn Read, context: &str| -> Result<Vec<u8>> {
            // `take(limit + 1)` rather than `take(limit)`: the extra byte is
            // what lets an over-limit entry fail instead of being silently
            // truncated into a plausible-looking (but wrong) resource.
            let mut bytes = Vec::new();
            file.take(limit.saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|e| Error::new("loader", e.to_string()))?;
            if bytes.len() as u64 > limit {
                return Err(Error::new(
                    context,
                    format!("resource expands past the {limit} byte limit"),
                ));
            }
            Ok(bytes)
        };
        for jar in &mut self.jars {
            let context = format!("{}:{path}", jar.path.display());
            if let Ok(mut file) = jar.archive.by_name(path) {
                read(entry_bytes(&mut file, &context));
            }
        }
        for nested in &mut self.nested_archives {
            let context = format!("{}:{path}", nested.name);
            if let Ok(mut file) = nested.archive.by_name(path) {
                read(entry_bytes(&mut file, &context));
            }
        }
        out
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
                let limit = self.limits.max_metadata_bytes;
                let context = format!("{}:{filename}", jar.path.display());
                if let Ok(mut zip_file) = jar.archive.by_name(&filename) {
                    let size = zip_file.size();
                    if size > limit {
                        return Err(Error::new(&context, "class file exceeds limit"));
                    }
                    return read_limited(&mut zip_file, limit, context);
                }
            }
            ClassLocation::Nested(nested_idx) => {
                let nested = &mut self.nested_archives[nested_idx];
                let limit = self.limits.max_metadata_bytes;
                let context = format!("{}:{filename}", nested.name);
                if let Ok(mut zip_file) = nested.archive.by_name(&filename) {
                    let size = zip_file.size();
                    if size > limit {
                        return Err(Error::new(&context, "class file exceeds limit"));
                    }
                    return read_limited(&mut zip_file, limit, context);
                }
            }
        }

        Err(Error::new("loader", format!("class not found: {name}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared size of an entry proves nothing: the read is bounded by the
    /// limit even when the stream holds far more.
    #[test]
    fn read_limited_bounds_what_it_allocates() {
        let data = vec![7u8; 64 * 1024];
        let bytes = read_limited(data.as_slice(), 1024, "test").expect_err("over the limit");
        assert!(
            bytes.to_string().contains("expands past"),
            "the error names the limit: {bytes}"
        );

        let exactly = read_limited(&data[..1024], 1024, "test").expect("exactly at the limit");
        assert_eq!(exactly.len(), 1024, "an entry that fits is read in full");

        let empty = read_limited(&b""[..], 1024, "test").expect("an empty entry is empty");
        assert!(empty.is_empty());
    }

    /// A stream that is longer than its declared size still stops at the limit:
    /// the point is that the reader never sees more than `limit + 1` bytes.
    #[test]
    fn read_limited_never_pulls_more_than_the_limit() {
        use std::io::Read;
        /// A reader that would report an unbounded length if asked.
        struct Endless {
            remaining: usize,
            served: std::rc::Rc<std::cell::Cell<usize>>,
        }
        impl Read for Endless {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = buf.len().min(self.remaining);
                buf[..n].fill(1);
                self.remaining -= n;
                self.served.set(self.served.get() + n);
                Ok(n)
            }
        }
        let served = std::rc::Rc::new(std::cell::Cell::new(0));
        let endless = Endless {
            remaining: usize::MAX / 2,
            served: served.clone(),
        };
        let error = read_limited(endless, 512, "test").expect_err("must stop at the limit");
        assert!(error.to_string().contains("expands past"), "{error}");
        assert_eq!(
            served.get(),
            513,
            "the reader stops at limit + 1, not at the stream's own end"
        );
    }
}
