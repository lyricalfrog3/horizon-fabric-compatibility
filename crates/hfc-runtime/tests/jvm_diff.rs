//! Differential tests of the bytecode interpreter against a real JVM.
//!
//! Each case in `tests/jvm/hfc/cases/` is a Java class with a `run()` that
//! returns deterministic labelled lines. `hfc.Harness` runs them on the
//! machine's JVM; the same classes run in the interpreter, over the JDK image
//! the client uses at play time (`~/.local/share/horizon-launcher/minecraft`'s
//! `runtime/.../lib/modules`), and the outputs must match line by line.
//!
//! `known_divergences.toml` lists `Case.label` entries the interpreter is known
//! to get wrong, with a reason. A listed label that diverges is reported but
//! does not fail; a listed label that now matches (or vanished) fails, so the
//! list can only shrink. A missing Java (`HFC_SKIP_JVM_DIFF=1` skips) or a
//! missing Minecraft installation fails with a clear message.

mod common;

use hfc_runtime::bridge::FabricBridge;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Instructions one case may run. The cases are small; a mis-decoded loop must
/// fail rather than hang.
const CASE_BUDGET: u64 = 100_000_000;

/// `java` from `JAVA_HOME`, else the first one on `PATH`.
fn java() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let java = PathBuf::from(home).join("bin").join("java");
        if java.is_file() {
            return Some(java);
        }
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("java"))
            .find(|java| java.is_file())
    })
}

fn skip() -> bool {
    std::env::var_os("HFC_SKIP_JVM_DIFF").is_some_and(|value| value == "1")
}

/// The compiled cases and the jar the interpreter loads.
struct Cases {
    classes: PathBuf,
    jar: PathBuf,
    names: Vec<String>,
}

impl Cases {
    /// Compile `tests/jvm/**/*.java` under `dir` and zip the classes.
    fn compile(java: &Path, dir: &Path) -> Result<Self, String> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/jvm");
        let mut sources = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .flatten()
            {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|e| e.to_str()) == Some("java") {
                    sources.push(path);
                }
            }
        }
        sources.sort();
        if sources.is_empty() {
            return Err("tests/jvm has no .java sources".into());
        }
        let classes = dir.join("classes");
        std::fs::create_dir_all(&classes).map_err(|e| format!("{}: {e}", classes.display()))?;
        let output = Command::new(java)
            .arg("-m")
            .arg("jdk.compiler/com.sun.tools.javac.Main")
            .arg("-d")
            .arg(&classes)
            .args(&sources)
            .output()
            .map_err(|e| format!("running {}: {e}", java.display()))?;
        if !output.status.success() {
            return Err(format!(
                "javac failed:\n{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let mut files = Vec::new();
        let mut stack = vec![classes.clone()];
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .flatten()
            {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files.sort();
        let jar = dir.join("cases.jar");
        let file = std::fs::File::create(&jar).map_err(|e| format!("{}: {e}", jar.display()))?;
        let mut zip = zip::ZipWriter::new(file);
        for file in &files {
            let name = file
                .strip_prefix(&classes)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .map_err(|e| format!("zip {e}"))?;
            let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
            zip.write_all(&bytes).map_err(|e| format!("zip {e}"))?;
        }
        zip.finish().map_err(|e| format!("zip {e}"))?;
        let mut names: Vec<String> = sources
            .iter()
            .filter_map(|path| {
                let name = path.file_stem()?.to_str()?;
                // `hfc.Harness` is the JVM driver, not a case.
                (name != "Harness").then(|| format!("hfc.cases.{name}"))
            })
            .collect();
        names.sort();
        Ok(Self {
            classes,
            jar,
            names,
        })
    }

    /// The JVM's `=== <class>` sections.
    fn run_jvm(&self, java: &Path) -> Result<std::collections::BTreeMap<String, String>, String> {
        let output = Command::new(java)
            .arg("-cp")
            .arg(&self.classes)
            .arg("hfc.Harness")
            .args(&self.names)
            .output()
            .map_err(|e| format!("running the harness: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "the JVM harness failed:\n{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        let mut sections = std::collections::BTreeMap::new();
        let mut current: Option<(String, String)> = None;
        for line in text.lines() {
            if let Some(name) = line.strip_prefix("=== ") {
                if let Some((name, body)) = current.take() {
                    sections.insert(name, body);
                }
                current = Some((name.to_string(), String::new()));
            } else if let Some((_, body)) = current.as_mut() {
                body.push_str(line);
                body.push('\n');
            }
        }
        if let Some((name, body)) = current.take() {
            sections.insert(name, body);
        }
        Ok(sections)
    }
}

/// `known_divergences.toml`: `"Case.label" = "reason"` lines.
fn known_divergences() -> std::collections::BTreeMap<String, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("jvm")
        .join("known_divergences.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Default::default();
    };
    let mut out = std::collections::BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, reason)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"');
        let reason = reason.trim().trim_matches('"');
        out.insert(key.to_string(), reason.to_string());
    }
    out
}

/// The label of one output line (`name=value`), or the whole line.
fn label(line: &str) -> &str {
    line.split_once('=').map_or(line, |(label, _)| label)
}

#[test]
fn the_interpreter_matches_the_jvm_on_the_cases() {
    let Some(java) = java() else {
        if skip() {
            eprintln!("skipped: HFC_SKIP_JVM_DIFF=1 and no Java");
            return;
        }
        panic!("no java found (JAVA_HOME or PATH); set HFC_SKIP_JVM_DIFF=1 to skip");
    };
    let modules = Command::new(&java).arg("--list-modules").output();
    let has_compiler = modules.is_ok_and(|output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.starts_with("jdk.compiler@"))
    });
    if !has_compiler {
        if skip() {
            eprintln!(
                "skipped: HFC_SKIP_JVM_DIFF=1 and {} has no jdk.compiler",
                java.display()
            );
            return;
        }
        panic!(
            "{} has no jdk.compiler module; set HFC_SKIP_JVM_DIFF=1 to skip",
            java.display()
        );
    }
    let Some(minecraft) = common::minecraft_dir() else {
        if skip() {
            eprintln!("skipped: HFC_SKIP_JVM_DIFF=1 and no Minecraft installation");
            return;
        }
        panic!(
            "no Minecraft installation (HORIZON_MINECRAFT_DIR or ~/.local/share/horizon-launcher/minecraft) for the JDK image; set HFC_SKIP_JVM_DIFF=1 to skip"
        );
    };
    let Some(image) = common::image(&minecraft) else {
        if skip() {
            eprintln!("skipped: HFC_SKIP_JVM_DIFF=1 and no JDK image");
            return;
        }
        panic!(
            "the Minecraft installation has no runtime/.../lib/modules image; set HFC_SKIP_JVM_DIFF=1 to skip"
        );
    };

    let dir = std::env::temp_dir().join(format!("hfc-jvm-diff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());

    let cases = Cases::compile(&java, &dir).unwrap_or_else(|e| panic!("{e}"));
    let jvm = cases.run_jvm(&java).unwrap_or_else(|e| panic!("{e}"));
    let known = known_divergences();
    let mut failures = Vec::new();
    let mut matched_known = std::collections::BTreeSet::new();
    let mut reported = Vec::new();

    for name in &cases.names {
        let expected = jvm
            .get(name)
            .unwrap_or_else(|| panic!("the JVM harness printed no section for {name}"));
        let mut bridge = FabricBridge::new();
        bridge
            .add_jimage(&image)
            .unwrap_or_else(|e| panic!("{}: {e}", image.display()));
        bridge
            .add_jar(&cases.jar)
            .unwrap_or_else(|e| panic!("{}: {e}", cases.jar.display()));
        let class = name.replace('.', "/");
        let actual = bridge.debug_call_static(&class, "run", "()Ljava/lang/String;", CASE_BUDGET);
        let actual = match actual {
            Ok(value) => value,
            Err(error) => {
                let missing = bridge.debug_missing();
                panic!(
                    "{name}: the interpreter failed: {error}\nmissing: {}",
                    missing.join(", ")
                );
            }
        };
        let expected_lines: Vec<&str> = expected.lines().collect();
        let actual_lines: Vec<&str> = actual.lines().collect();
        let mut count = expected_lines.len().max(actual_lines.len());
        if count == 0 && !expected.is_empty() {
            count = 1;
        }
        for index in 0..count {
            let want = expected_lines
                .get(index)
                .copied()
                .unwrap_or("<missing line>");
            let got = actual_lines.get(index).copied().unwrap_or("<missing line>");
            if want == got {
                continue;
            }
            let key = format!("{}.{}", short_name(name), label(want));
            if let Some(reason) = known.get(&key) {
                matched_known.insert(key.clone());
                reported.push(format!(
                    "{key} (known: {reason})\n  JVM: {want}\n  HFC: {got}"
                ));
            } else {
                failures.push(format!(
                    "{key}\n  JVM: {want}\n  HFC: {got}\n  missing: {}",
                    bridge.debug_missing().join(", ")
                ));
            }
        }
    }

    for line in &reported {
        eprintln!("known divergence: {line}");
    }
    for (key, reason) in &known {
        if let Some((case, _)) = key.split_once('.')
            && cases.names.iter().any(|name| short_name(name) == case)
            && !matched_known.contains(key)
        {
            failures.push(format!(
                "{key} is listed in known_divergences.toml ({reason}) but did not diverge; remove it"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "the interpreter differs from the JVM:\n{}",
        failures.join("\n")
    );
}

/// `hfc.cases.Arith` is `Arith` in a divergence key.
fn short_name(class: &str) -> &str {
    class.rsplit('.').next().unwrap_or(class)
}
