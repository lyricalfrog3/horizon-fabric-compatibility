//! The machine-readable and human-readable result of one run.

use hfc_core::{Error, Result};
use hfc_fabric::{Issue, Severity, SkippedJar};
use serde::{Deserialize, Serialize};
use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

pub const REPORT_FORMAT: u32 = 1;

/// What HFC did with one mod in this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModStatus {
    /// A generated folder was written or replaced.
    Installed,
    /// The generated folder already matched this jar exactly.
    Reused,
    /// A conversion would have happened, but `--dry-run` was set.
    WouldInstall,
    /// The jar has content but the port pipeline found nothing usable.
    NotPortable,
    /// The mod cannot be installed for a structural reason (duplicate id,
    /// unsafe id, graph error, conflicting namespace, user folder).
    Blocked,
    /// Conversion or installation failed.
    Failed,
    /// The jar was analyzed; installation is disabled in the config.
    Scanned,
}

impl ModStatus {
    pub fn label(self) -> &'static str {
        match self {
            ModStatus::Installed => "installed",
            ModStatus::Reused => "reused",
            ModStatus::WouldInstall => "would install",
            ModStatus::NotPortable => "not portable",
            ModStatus::Blocked => "blocked",
            ModStatus::Failed => "failed",
            ModStatus::Scanned => "scanned",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CacheState {
    Hit,
    Miss,
    Disabled,
    NotReached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModReport {
    pub id: Option<String>,
    pub name: String,
    pub version: String,
    pub file_name: String,
    pub sha256: String,
    pub status: ModStatus,
    pub runtime_ready: bool,
    /// `Works`, `Partly` or `Doesn't run` from the carry pipeline.
    pub verdict: Option<String>,
    pub carry_summary: Option<String>,
    pub installed_as: Option<String>,
    pub cache: CacheState,
    pub ports: Vec<String>,
    pub skipped_features: Vec<String>,
    pub issues: Vec<Issue>,
}

impl ModReport {
    /// The two questions the report never conflates: whether this run put the
    /// mod's data in the mods folder (`status`), and whether HFC can claim its
    /// Fabric behaviour is satisfied (`runtime_ready`). A content-only install
    /// is `installed` *and* `content-only`, never `ready`.
    pub fn runtime_label(&self) -> &'static str {
        match (self.status, self.runtime_ready) {
            (ModStatus::Blocked, _) => "blocked",
            (_, true) => "ready",
            _ => "content-only",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub jars_found: usize,
    pub jars_analyzed: usize,
    pub installed: usize,
    pub reused: usize,
    pub would_install: usize,
    pub not_portable: usize,
    pub blocked: usize,
    pub failed: usize,
    pub scanned: usize,
    pub errors: usize,
    pub warnings: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub format: u32,
    pub hfc_version: String,
    pub compat_api: String,
    pub pipeline_version: u32,
    pub horizon_version: String,
    pub platform: String,
    pub mods_dir: String,
    pub generated_unix: u64,
    pub dry_run: bool,
    pub summary: Summary,
    pub mods: Vec<ModReport>,
    pub issues: Vec<Issue>,
    pub skipped: Vec<SkippedJar>,
    pub notes: Vec<String>,
}

impl Report {
    pub fn new(mods_dir: &Path, dry_run: bool, generated_unix: u64) -> Self {
        Self {
            format: REPORT_FORMAT,
            hfc_version: hfc_core::HFC_VERSION.into(),
            compat_api: hfc_core::COMPAT_API.into(),
            pipeline_version: hfc_core::PIPELINE_VERSION,
            horizon_version: horizon_core::version::VERSION_ID.into(),
            platform: hfc_core::platform(),
            mods_dir: mods_dir.display().to_string(),
            generated_unix,
            dry_run,
            summary: Summary::default(),
            mods: Vec::new(),
            issues: Vec::new(),
            skipped: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn recompute(&mut self) {
        let mut summary = Summary {
            jars_found: self.mods.len() + self.skipped.len(),
            jars_analyzed: self.mods.len(),
            errors: self
                .issues
                .iter()
                .filter(|i| i.severity == Severity::Error)
                .count(),
            warnings: self
                .issues
                .iter()
                .filter(|i| i.severity == Severity::Warning)
                .count(),
            ..Summary::default()
        };
        for m in &self.mods {
            match m.status {
                ModStatus::Installed => summary.installed += 1,
                ModStatus::Reused => summary.reused += 1,
                ModStatus::WouldInstall => summary.would_install += 1,
                ModStatus::NotPortable => summary.not_portable += 1,
                ModStatus::Blocked => summary.blocked += 1,
                ModStatus::Failed => summary.failed += 1,
                ModStatus::Scanned => summary.scanned += 1,
            }
        }
        self.summary = summary;
    }

    pub fn write(&self, compat_dir: &Path) -> Result<()> {
        hfc_core::atomic_write_json(&compat_dir.join(hfc_core::REPORT_JSON), self)?;
        hfc_core::atomic_write(
            &compat_dir.join(hfc_core::REPORT_MD),
            self.markdown().as_bytes(),
        )
    }

    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| Error::new(path.display().to_string(), e.to_string()))
    }

    pub fn markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# Horizon Fabric Compatibility");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Generated {} · HFC {} · compatibility API {} · pipeline {}  ",
            timestamp(self.generated_unix),
            self.hfc_version,
            self.compat_api,
            self.pipeline_version
        );
        let _ = writeln!(
            out,
            "Horizon {} · {} · mods: `{}`{}",
            self.horizon_version,
            self.platform,
            self.mods_dir,
            if self.dry_run { " · dry run" } else { "" }
        );
        let _ = writeln!(out);
        let s = &self.summary;
        let _ = writeln!(out, "## Summary");
        let _ = writeln!(out);
        let _ = writeln!(out, "| | |");
        let _ = writeln!(out, "|---|---:|");
        for (label, value) in [
            ("Jars found", s.jars_found),
            ("Jars analyzed", s.jars_analyzed),
            ("Installed", s.installed),
            ("Reused", s.reused),
            ("Would install", s.would_install),
            ("Not portable", s.not_portable),
            ("Blocked", s.blocked),
            ("Failed", s.failed),
            ("Scanned only", s.scanned),
            ("Errors", s.errors),
            ("Warnings", s.warnings),
        ] {
            let _ = writeln!(out, "| {label} | {value} |");
        }
        let _ = writeln!(out);

        let _ = writeln!(out, "## Mods");
        let _ = writeln!(out);
        if self.mods.is_empty() {
            let _ = writeln!(out, "No Fabric mods were analyzed.");
        } else {
            let _ = writeln!(
                out,
                "| Mod | Version | Status | Runtime | Carry verdict | Installed as |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|---|");
            for m in &self.mods {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} |",
                    cell(m.id.as_deref().unwrap_or(&m.file_name)),
                    cell(&m.version),
                    m.status.label(),
                    m.runtime_label(),
                    m.verdict.as_deref().unwrap_or("—"),
                    m.installed_as
                        .as_deref()
                        .map(cell)
                        .unwrap_or_else(|| "—".into()),
                );
            }
            let _ = writeln!(out);
            for m in &self.mods {
                let _ = writeln!(
                    out,
                    "### {} {}",
                    m.id.as_deref().unwrap_or(&m.file_name),
                    if m.version.is_empty() { "" } else { &m.version }
                );
                let _ = writeln!(out);
                let _ = writeln!(out, "- File: `{}`", m.file_name);
                let _ = writeln!(out, "- Status: {}", m.status.label());
                if let Some(verdict) = &m.verdict {
                    let _ = writeln!(out, "- Carry verdict: {verdict}");
                }
                if let Some(summary) = &m.carry_summary {
                    let _ = writeln!(out, "- {summary}");
                }
                let _ = writeln!(
                    out,
                    "- Fabric code: {}",
                    if m.runtime_ready {
                        "none declared (no entrypoints, mixins or access widener), so there is \
                         no Fabric behaviour HFC could run or fail to run; the metadata's \
                         dependency contract resolved"
                    } else if m.installed_as.is_some() {
                        "declared but never executed; only data was carried, and the mod's Java \
                         behaviour is not provided"
                    } else {
                        "declared but never executed, and no data was installed in this run"
                    }
                );
                if !m.ports.is_empty() {
                    let _ = writeln!(out, "- Ported: {}", m.ports.join(", "));
                }
                if !m.skipped_features.is_empty() {
                    let _ = writeln!(out, "- Not ported: {}", m.skipped_features.join("; "));
                }
                for issue in &m.issues {
                    let _ = writeln!(
                        out,
                        "- {}: {}",
                        severity_label(issue.severity),
                        issue.message
                    );
                }
                let _ = writeln!(out);
            }
        }

        if !self.issues.is_empty() {
            let _ = writeln!(out, "## Findings");
            let _ = writeln!(out);
            for severity in [Severity::Error, Severity::Warning, Severity::Info] {
                let any = self.issues.iter().any(|i| i.severity == severity);
                if !any {
                    continue;
                }
                let _ = writeln!(out, "### {}", severity_label(severity));
                let _ = writeln!(out);
                for issue in self.issues.iter().filter(|i| i.severity == severity) {
                    let _ = writeln!(out, "- {}", issue.message);
                }
                let _ = writeln!(out);
            }
        }

        if !self.skipped.is_empty() {
            let _ = writeln!(out, "## Skipped jars");
            let _ = writeln!(out);
            for jar in &self.skipped {
                let _ = writeln!(out, "- `{}`: {}", jar.file_name, jar.reason);
            }
            let _ = writeln!(out);
        }

        if !self.notes.is_empty() {
            let _ = writeln!(out, "## Notes");
            let _ = writeln!(out);
            for note in &self.notes {
                let _ = writeln!(out, "- {note}");
            }
            let _ = writeln!(out);
        }

        let _ = writeln!(out, "---");
        let _ = writeln!(
            out,
            "HFC reads jars as data, never decompiles or executes them, and never \
             modifies the original files. A mod with Runtime `content-only` has had its \
             data carried, not its Java code run; `ready` only means it declared no code \
             and its metadata's dependency contract resolved."
        );
        out
    }
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "Error",
        Severity::Warning => "Warning",
        Severity::Info => "Info",
    }
}

fn timestamp(unix: u64) -> String {
    // A dependency-free UTC formatting of a Unix timestamp. Not worth a date
    // crate for one report line.
    let days = unix / 86_400;
    let seconds = unix % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

/// Howard Hinnant's civil-from-days algorithm.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + i64::from(m <= 2), m, d)
}

/// The report path inside a compatibility folder.
pub fn json_path(compat_dir: &Path) -> PathBuf {
    compat_dir.join(hfc_core::REPORT_JSON)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc() {
        assert_eq!(timestamp(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(timestamp(1_700_000_000), "2023-11-14 22:13:20 UTC");
    }

    #[test]
    fn markdown_does_not_count_errors_twice() {
        let mut report = Report::new(Path::new("/mods"), false, 0);
        report.mods.push(ModReport {
            id: Some("ruby".into()),
            name: "Ruby".into(),
            version: "1".into(),
            file_name: "ruby.jar".into(),
            sha256: "0".repeat(64),
            status: ModStatus::Installed,
            runtime_ready: false,
            verdict: Some("Partly".into()),
            carry_summary: Some("3 blocks port".into()),
            installed_as: Some("ruby".into()),
            cache: CacheState::Miss,
            ports: vec!["3 blocks".into()],
            skipped_features: vec!["a command".into()],
            issues: Vec::new(),
        });
        report.issues.push(Issue {
            severity: Severity::Warning,
            code: hfc_fabric::IssueCode::MissingRecommendation,
            mod_id: Some("ruby".into()),
            related: Vec::new(),
            message: "recommends cloth-config".into(),
        });
        report.recompute();
        assert_eq!(report.summary.warnings, 1);
        assert_eq!(report.summary.installed, 1);
        let text = report.markdown();
        assert!(
            text.contains("| ruby | 1 | installed | content-only | Partly | ruby |"),
            "{text}"
        );
        assert!(text.contains("recommends cloth-config"));
        // The report never claims a content-only install runs Fabric code.
        assert!(text.contains("declared but never executed"), "{text}");
        assert!(
            !text.contains("satisfied as far as HFC can tell"),
            "the old blanket claim survived: {text}"
        );
    }
}
