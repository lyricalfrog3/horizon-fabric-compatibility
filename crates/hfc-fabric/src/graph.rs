//! Bringing many scans together: providers, conflicts and a load order.
//!
//! The graph is intentionally conservative. It never turns an unknown version
//! comparison into a success, and it keeps two separate answers for every mod:
//!
//! - `installable`: the jar itself may be read and its content carried.
//! - `runtime_ready`: HFC believes the mod's Fabric behaviour is actually
//!   satisfied in this environment.
//!
//! A content mod that declares a dependency on Fabric API can still have its
//! blocks, recipes and textures carried (`installable`), while the report says
//! plainly that its code cannot run (`runtime_ready = false`). That distinction
//! is what stops the compatibility layer from making claims it cannot prove.

use crate::archive::JarScan;
use crate::metadata::{Environment, Metadata};
use crate::version::Match;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The versions HFC compares dependencies against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentInfo {
    pub minecraft: String,
    pub fabric_loader: String,
    pub java: String,
}

impl Default for EnvironmentInfo {
    fn default() -> Self {
        Self {
            minecraft: "26.3".into(),
            fabric_loader: "0.19.3".into(),
            java: "25".into(),
        }
    }
}

impl EnvironmentInfo {
    /// The version HFC should use for a built-in provider id.
    pub fn version_of(&self, id: &str) -> Option<&str> {
        match id {
            "minecraft" => Some(&self.minecraft),
            "fabricloader" | "quilt_loader" => Some(&self.fabric_loader),
            "java" => Some(&self.java),
            "fabric-api" | "fabric" => Some("0.165.0+26.3"),
            _ => None,
        }
    }
}

/// How serious an issue is. Only errors make a jar uninstallable; warnings
/// always leave a trace in the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// A stable machine-readable issue kind, so a UI can group findings without
/// matching on English text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IssueCode {
    UnreadableArchive,
    NotFabric,
    InvalidMetadata,
    DuplicateId,
    MissingDependency,
    VersionMismatch,
    UnresolvedVersion,
    Conflict,
    Break,
    DependencyCycle,
    ServerOnly,
    NestedProvider,
    MissingRecommendation,
}

/// One finding about one mod (or a pair of mods).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub severity: Severity,
    pub code: IssueCode,
    /// The mod the issue is reported against.
    pub mod_id: Option<String>,
    /// Other mods involved (the other half of a duplicate or conflict).
    pub related: Vec<String>,
    pub message: String,
}

impl Issue {
    fn new(severity: Severity, code: IssueCode, mod_id: Option<&str>, message: String) -> Self {
        Self {
            severity,
            code,
            mod_id: mod_id.map(str::to_owned),
            related: Vec::new(),
            message,
        }
    }

    fn with_related(mut self, related: impl IntoIterator<Item = String>) -> Self {
        self.related.extend(related);
        self
    }
}

/// Where a provider came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Origin {
    /// A jar directly in the mods folder.
    Jar { file_name: String, sha256: String },
    /// A jar inside another jar (`META-INF/jars/*.jar`). It is a provider for
    /// dependency resolution but is not installed as a sibling folder.
    NestedJar {
        parent_file: String,
        path: String,
        sha256: String,
    },
}

impl Origin {
    pub fn file_name(&self) -> &str {
        match self {
            Origin::Jar { file_name, .. } => file_name,
            Origin::NestedJar { parent_file, .. } => parent_file,
        }
    }

    pub fn sha256(&self) -> &str {
        match self {
            Origin::Jar { sha256, .. } => sha256,
            Origin::NestedJar { sha256, .. } => sha256,
        }
    }

    pub fn is_nested(&self) -> bool {
        matches!(self, Origin::NestedJar { .. })
    }
}

/// One mod HFC knows about, whether or not it will be installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModNode {
    pub id: String,
    pub version: String,
    pub name: String,
    pub license: String,
    pub environment: Environment,
    pub origin: Origin,
    /// The jar is intact enough to carry its content.
    pub installable: bool,
    /// HFC can claim the Fabric behaviour of this mod is satisfied.
    pub runtime_ready: bool,
    /// Entry points and mixin counts, useful in reports without re-reading.
    pub entrypoints: usize,
    pub mixin_configs: usize,
    pub has_access_widener: bool,
}

impl ModNode {
    /// A one-line status for a report.
    pub fn status(&self) -> &'static str {
        match (self.installable, self.runtime_ready) {
            (true, true) => "ready",
            (true, false) => "content-only",
            (false, _) => "blocked",
        }
    }
}

/// A jar that could not even be given an identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedJar {
    pub file_name: String,
    pub sha256: String,
    pub reason: String,
}

/// The result of resolving every scan in a folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Resolution {
    pub mods: Vec<ModNode>,
    /// Provider ids in dependency-first load order. Nested providers and
    /// blocked mods are excluded; a cycle is reported and its members are left
    /// out rather than given an arbitrary order.
    pub order: Vec<String>,
    pub issues: Vec<Issue>,
    pub skipped: Vec<SkippedJar>,
}

impl Resolution {
    pub fn errors(&self) -> impl Iterator<Item = &Issue> {
        self.issues
            .iter()
            .filter(|issue| issue.severity == Severity::Error)
    }

    pub fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }

    pub fn node(&self, id: &str) -> Option<&ModNode> {
        self.mods.iter().find(|m| m.id == id)
    }

    pub fn issues_for(&self, id: &str) -> impl Iterator<Item = &Issue> {
        self.issues.iter().filter(move |issue| {
            issue.mod_id.as_deref() == Some(id) || issue.related.iter().any(|r| r == id)
        })
    }

    /// Mods that will be installed as folders, in load order.
    pub fn installed(&self) -> impl Iterator<Item = &ModNode> {
        self.order
            .iter()
            .filter_map(|id| self.mods.iter().find(|m| &m.id == id))
    }
}

struct Candidate {
    node: ModNode,
    metadata: Metadata,
}

/// Resolve every top-level jar plus every nested provider into a plan.
///
/// `scans` is the output of [`crate::archive::discover`] +
/// [`crate::archive::scan_path`], in any order: the result is deterministic.
pub fn resolve(scans: &[JarScan], env: &EnvironmentInfo) -> Result<Resolution, String> {
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut skipped = Vec::new();
    let mut issues = Vec::new();

    for scan in scans {
        let Some(metadata) = scan.metadata.clone() else {
            skipped.push(SkippedJar {
                file_name: scan.file_name.clone(),
                sha256: scan.sha256.clone(),
                reason: scan
                    .error
                    .clone()
                    .unwrap_or_else(|| "no fabric.mod.json".into()),
            });
            if let Some(error) = &scan.error {
                let code = if error.contains("fabric.mod.json") || error.contains("Fabric") {
                    IssueCode::InvalidMetadata
                } else {
                    IssueCode::UnreadableArchive
                };
                issues.push(Issue::new(
                    Severity::Error,
                    code,
                    None,
                    format!("{}: {error}", scan.file_name),
                ));
            }
            continue;
        };
        let installable = scan.error.is_none();
        if let Some(error) = &scan.error {
            issues.push(Issue::new(
                Severity::Error,
                IssueCode::UnreadableArchive,
                Some(&metadata.id),
                format!("{}: {error}", scan.file_name),
            ));
        }
        candidates.push(Candidate {
            node: node_from(
                &metadata,
                Origin::Jar {
                    file_name: scan.file_name.clone(),
                    sha256: scan.sha256.clone(),
                },
                installable,
                &scan.warnings,
                &mut issues,
            ),
            metadata: metadata.clone(),
        });
        for nested in &scan.nested {
            let Some(nested_metadata) = nested.metadata.clone() else {
                if let Some(error) = &nested.error {
                    issues.push(Issue::new(
                        Severity::Warning,
                        IssueCode::UnreadableArchive,
                        Some(&metadata.id),
                        format!("{}: {error}", scan.file_name),
                    ));
                }
                continue;
            };
            let mut node = node_from(
                &nested_metadata,
                Origin::NestedJar {
                    parent_file: scan.file_name.clone(),
                    path: nested.path.clone(),
                    sha256: nested.sha256.clone(),
                },
                false,
                &[],
                &mut issues,
            );
            node.runtime_ready = false;
            issues.push(Issue::new(
                Severity::Info,
                IssueCode::NestedProvider,
                Some(&nested_metadata.id),
                format!(
                    "provided by {}/{}; it counts as a dependency but is not installed as a \
                     separate folder",
                    scan.file_name, nested.path
                ),
            ));
            candidates.push(Candidate {
                node,
                metadata: nested_metadata,
            });
        }
    }

    // Deterministic order before duplicate selection: a top-level jar wins
    // over a nested provider, then the lexically first file.
    candidates.sort_by(|a, b| {
        (
            a.node.origin.is_nested(),
            a.node.origin.file_name(),
            a.node.id.as_str(),
        )
            .cmp(&(
                b.node.origin.is_nested(),
                b.node.origin.file_name(),
                b.node.id.as_str(),
            ))
    });

    let mut by_id: BTreeMap<String, Candidate> = BTreeMap::new();
    let mut duplicates: Vec<(String, Candidate)> = Vec::new();
    for candidate in candidates {
        if let Some(existing) = by_id.get(&candidate.node.id) {
            let winner = existing.node.id.clone();
            let loser = candidate.node.id.clone();
            let winner_file = existing.node.origin.file_name().to_owned();
            let loser_file = candidate.node.origin.file_name().to_owned();
            if candidate.node.origin.is_nested() {
                issues.push(
                    Issue::new(
                        Severity::Info,
                        IssueCode::DuplicateId,
                        Some(&loser),
                        format!(
                            "nested provider {loser:?} in {loser_file} is already provided by \
                             {winner_file}; using {winner_file}"
                        ),
                    )
                    .with_related([winner.clone()]),
                );
            } else {
                issues.push(
                    Issue::new(
                        Severity::Error,
                        IssueCode::DuplicateId,
                        Some(&loser),
                        format!(
                            "mod id {loser:?} is provided more than once ({winner_file} and \
                             {loser_file}); only {winner_file} is used"
                        ),
                    )
                    .with_related([winner.clone()]),
                );
                duplicates.push((winner, candidate));
            }
        } else {
            by_id.insert(candidate.node.id.clone(), candidate);
        }
    }

    // A duplicate does not make the winner uninstallable, but the loser is
    // reported as skipped so the user can delete one of the two files.
    for (winner, candidate) in duplicates {
        skipped.push(SkippedJar {
            file_name: candidate.node.origin.file_name().to_owned(),
            sha256: candidate.node.origin.sha256().to_owned(),
            reason: format!(
                "duplicate mod id {:?} (keeping {winner})",
                candidate.node.id
            ),
        });
    }

    let mut mods: Vec<ModNode> = by_id.values().map(|c| c.node.clone()).collect();

    // Built-in environment dependencies are checked, never listed as mods.
    let provider_version = |id: &str| -> Option<String> {
        if let Some(version) = env.version_of(id) {
            return Some(version.to_owned());
        }
        by_id.get(id).map(|c| c.node.version.clone())
    };

    for candidate in by_id.values() {
        let node = match mods.iter_mut().find(|m| m.id == candidate.node.id) {
            Some(node) => node,
            None => continue,
        };
        let metadata = &candidate.metadata;
        let mut installable = node.installable;
        let mut runtime_ready = node.runtime_ready;

        if node.environment == Environment::Server {
            runtime_ready = false;
            issues.push(Issue::new(
                Severity::Warning,
                IssueCode::ServerOnly,
                Some(&node.id),
                "the mod declares server-side code; Horizon's session does not run Fabric \
                 server entrypoints, so only its data can be carried"
                    .into(),
            ));
        }

        for dependency in &metadata.depends {
            match provider_version(&dependency.id) {
                None => {
                    runtime_ready = false;
                    issues.push(Issue::new(
                        Severity::Error,
                        IssueCode::MissingDependency,
                        Some(&node.id),
                        format!(
                            "requires {:?} {} but no such mod is installed",
                            dependency.id,
                            dependency.range.raw()
                        ),
                    ));
                }
                Some(version) => match dependency.range.matches(&version) {
                    Match::Yes => {}
                    Match::No => {
                        runtime_ready = false;
                        issues.push(Issue::new(
                            Severity::Error,
                            IssueCode::VersionMismatch,
                            Some(&node.id),
                            format!(
                                "requires {:?} {}, but {} is provided",
                                dependency.id,
                                dependency.range.raw(),
                                version
                            ),
                        ));
                    }
                    Match::Unknown => {
                        runtime_ready = false;
                        issues.push(Issue::new(
                            Severity::Warning,
                            IssueCode::UnresolvedVersion,
                            Some(&node.id),
                            format!(
                                "requires {:?} {}, which HFC could not compare with the \
                                 provided {version}",
                                dependency.id,
                                dependency.range.raw()
                            ),
                        ));
                    }
                },
            }
        }

        for recommendation in &metadata.recommends {
            if provider_version(&recommendation.id).is_none() {
                issues.push(Issue::new(
                    Severity::Info,
                    IssueCode::MissingRecommendation,
                    Some(&node.id),
                    format!("recommends {:?}, which is not installed", recommendation.id),
                ));
            }
        }

        // A conflict or break blocks the declaring mod here. The symmetric
        // pass below blocks the other side once every mod has been examined.
        for (dependency, code, label) in [
            (&metadata.conflicts, IssueCode::Conflict, "conflicts with"),
            (&metadata.breaks, IssueCode::Break, "breaks"),
        ] {
            for other in dependency {
                let Some(version) = provider_version(&other.id) else {
                    continue;
                };
                if other.id == node.id {
                    continue;
                }
                if other.range.matches(&version) != Match::No {
                    // `Unknown` is treated as a possible conflict; reporting
                    // it is safer than installing two mods that declare a
                    // conflicting range HFC cannot evaluate.
                    installable = false;
                    runtime_ready = false;
                    issues.push(
                        Issue::new(
                            Severity::Error,
                            code,
                            Some(&node.id),
                            format!(
                                "{label} {:?} {}, which is installed as {version}",
                                other.id,
                                other.range.raw()
                            ),
                        )
                        .with_related([other.id.clone()]),
                    );
                }
            }
        }

        node.installable = installable;
        node.runtime_ready = runtime_ready;
    }

    // Conflicts and breaks are symmetric in the installed set: two mods that
    // cannot coexist are both blocked, because picking one arbitrarily would
    // silently change behaviour.
    let blocked_pairs: Vec<(String, String)> = issues
        .iter()
        .filter(|issue| {
            matches!(issue.code, IssueCode::Conflict | IssueCode::Break)
                && issue.severity == Severity::Error
        })
        .filter_map(|issue| {
            let source = issue.mod_id.clone()?;
            Some(
                issue
                    .related
                    .iter()
                    .map(|other| (source.clone(), other.clone()))
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect();
    for (a, b) in blocked_pairs {
        for id in [a, b] {
            if let Some(node) = mods.iter_mut().find(|m| m.id == id) {
                node.installable = false;
                node.runtime_ready = false;
            }
        }
    }

    // Build the dependency graph over the winners. Edges point from a
    // dependency to its dependent so a topological order is dependency-first.
    let ids: BTreeSet<String> = by_id.keys().cloned().collect();
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut indegree: BTreeMap<String, usize> = ids.iter().map(|id| (id.clone(), 0)).collect();
    for candidate in by_id.values() {
        if candidate.node.origin.is_nested() {
            continue;
        }
        for dependency in &candidate.metadata.depends {
            if !ids.contains(&dependency.id) || dependency.id == candidate.node.id {
                continue;
            }
            let inserted = edges
                .entry(dependency.id.clone())
                .or_default()
                .insert(candidate.node.id.clone());
            if inserted {
                *indegree.entry(candidate.node.id.clone()).or_default() += 1;
            }
        }
    }

    let mut queue: VecDeque<String> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(id, _)| id.clone())
        .collect();
    let mut order = Vec::new();
    let mut placed: BTreeSet<String> = BTreeSet::new();
    while let Some(id) = queue.pop_front() {
        if !placed.insert(id.clone()) {
            continue;
        }
        order.push(id.clone());
        if let Some(dependents) = edges.get(&id) {
            for dependent in dependents {
                if let Some(degree) = indegree.get_mut(dependent) {
                    *degree = degree.saturating_sub(1);
                    if *degree == 0 {
                        queue.push_back(dependent.clone());
                    }
                }
            }
        }
    }

    let remaining: Vec<String> = ids
        .iter()
        .filter(|id| !placed.contains(*id))
        .cloned()
        .collect();
    if !remaining.is_empty() {
        // Find one readable cycle so the report is actionable, then mark every
        // remaining node as not runtime-ready. Their content can still be
        // carried, so installability is untouched.
        let cycle = find_cycle(&remaining, &edges);
        let cycle_text = cycle.join(" -> ");
        issues.push(Issue::new(
            Severity::Error,
            IssueCode::DependencyCycle,
            cycle.first().cloned().as_deref(),
            if cycle_text.is_empty() {
                "dependency cycle between: ".to_owned() + &remaining.join(", ")
            } else {
                format!("dependency cycle: {cycle_text}")
            },
        ));
        for id in &remaining {
            if let Some(node) = mods.iter_mut().find(|m| &m.id == id) {
                // Without a valid order HFC will not install; content could
                // still be carried later, but not automatically.
                node.installable = false;
                node.runtime_ready = false;
            }
        }
        // Blocked/cyclic mods are still ordered last deterministically so a
        // caller can decide; the `order` contract is "dependency-first where
        // possible".
        order.extend(remaining);
    }

    // Only installable, top-level providers belong in the install order. The
    // order is already dependency-first; keeping it stable matters.
    order.retain(|id| {
        mods.iter()
            .find(|m| &m.id == id)
            .is_some_and(|m| m.installable)
    });

    Ok(Resolution {
        mods,
        order,
        issues,
        skipped,
    })
}

fn node_from(
    metadata: &Metadata,
    origin: Origin,
    installable: bool,
    scan_warnings: &[String],
    issues: &mut Vec<Issue>,
) -> ModNode {
    for warning in scan_warnings {
        issues.push(Issue::new(
            Severity::Warning,
            IssueCode::InvalidMetadata,
            Some(&metadata.id),
            warning.clone(),
        ));
    }
    ModNode {
        id: metadata.id.clone(),
        version: metadata.version.clone(),
        name: if metadata.name.is_empty() {
            metadata.id.clone()
        } else {
            metadata.name.clone()
        },
        license: metadata.license_text(),
        environment: metadata.environment,
        origin,
        installable,
        runtime_ready: installable && !metadata.has_code_signals(),
        entrypoints: metadata.entrypoint_count(),
        mixin_configs: metadata.mixins.len(),
        has_access_widener: metadata.access_widener.is_some(),
    }
}

/// Walk remaining nodes depth-first until a node repeats; return the cycle in
/// order, closing back on its first member.
fn find_cycle(remaining: &[String], edges: &BTreeMap<String, BTreeSet<String>>) -> Vec<String> {
    let allowed: BTreeSet<&str> = remaining.iter().map(String::as_str).collect();
    fn visit<'a>(
        node: &'a str,
        edges: &'a BTreeMap<String, BTreeSet<String>>,
        allowed: &BTreeSet<&'a str>,
        path: &mut Vec<&'a str>,
        seen: &mut BTreeSet<&'a str>,
    ) -> Option<Vec<String>> {
        if let Some(position) = path.iter().position(|n| *n == node) {
            let mut cycle: Vec<String> = path[position..].iter().map(|n| (*n).to_owned()).collect();
            cycle.push(node.to_owned());
            return Some(cycle);
        }
        if !seen.insert(node) {
            return None;
        }
        path.push(node);
        if let Some(next) = edges.get(node) {
            for dependent in next {
                if allowed.contains(dependent.as_str())
                    && let Some(cycle) = visit(dependent, edges, allowed, path, seen)
                {
                    return Some(cycle);
                }
            }
        }
        path.pop();
        None
    }
    let mut seen = BTreeSet::new();
    for node in remaining {
        let mut path = Vec::new();
        if let Some(cycle) = visit(node, edges, &allowed, &mut path, &mut seen) {
            return cycle;
        }
    }
    Vec::new()
}

/// Supply a dependency edge for tests and callers that already know a mod's
/// metadata. Kept private to the module's own tests; the public entry point is
/// [`resolve`].
#[cfg(test)]
fn dependency(id: &str, range: &str) -> crate::metadata::Dependency {
    crate::metadata::Dependency {
        id: id.to_owned(),
        range: crate::version::VersionRange::parse(range),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{Census, Loader};
    use std::path::PathBuf;

    fn scan(id: &str, version: &str, depends: &[(&str, &str)]) -> JarScan {
        let metadata = Metadata {
            schema_version: 1,
            id: id.into(),
            version: version.into(),
            name: id.into(),
            description: String::new(),
            authors: Vec::new(),
            contributors: Vec::new(),
            contact: BTreeMap::new(),
            license: vec!["MIT".into()],
            environment: Environment::Both,
            entrypoints: BTreeMap::new(),
            mixins: Vec::new(),
            access_widener: None,
            jars: Vec::new(),
            depends: depends.iter().map(|(id, r)| dependency(id, r)).collect(),
            recommends: Vec::new(),
            suggests: Vec::new(),
            conflicts: Vec::new(),
            breaks: Vec::new(),
            custom: serde_json::Value::Null,
            icon: None,
        };
        JarScan {
            path: PathBuf::from(format!("{id}.jar")),
            file_name: format!("{id}.jar"),
            sha256: format!("{id:0>64}"),
            size: 1,
            loader: Loader::Fabric,
            metadata: Some(metadata),
            nested: Vec::new(),
            census: Census::default(),
            mixin_targets: Vec::new(),
            warnings: Vec::new(),
            error: None,
        }
    }

    #[test]
    fn orders_dependencies_first() {
        let scans = [
            scan("app", "1", &[("lib", "*")]),
            scan("lib", "2", &[("core", ">=1")]),
            scan("core", "1.5", &[]),
        ];
        let resolution = resolve(&scans, &EnvironmentInfo::default()).unwrap();
        assert_eq!(resolution.order, ["core", "lib", "app"]);
        assert!(resolution.mods.iter().all(|m| m.runtime_ready));
        assert!(!resolution.has_errors());
    }

    #[test]
    fn missing_and_mismatched_dependencies_block_runtime_not_content() {
        let scans = [
            scan("app", "1", &[("lib", ">=2"), ("missing-dep", "*")]),
            scan("lib", "1", &[]),
        ];
        let resolution = resolve(&scans, &EnvironmentInfo::default()).unwrap();
        let app = resolution.node("app").unwrap();
        assert!(app.installable);
        assert!(!app.runtime_ready);
        assert_eq!(app.status(), "content-only");
        let codes: Vec<IssueCode> = resolution
            .issues_for("app")
            .map(|issue| issue.code)
            .collect();
        assert!(codes.contains(&IssueCode::MissingDependency));
        assert!(codes.contains(&IssueCode::VersionMismatch));
    }

    #[test]
    fn duplicate_ids_keep_the_first_and_report_the_second() {
        let mut second = scan("ruby", "2", &[]);
        second.file_name = "zzz-ruby.jar".into();
        let scans = [scan("ruby", "1", &[]), second];
        let resolution = resolve(&scans, &EnvironmentInfo::default()).unwrap();
        assert_eq!(resolution.mods.len(), 1);
        assert_eq!(resolution.node("ruby").unwrap().version, "1");
        assert!(resolution.skipped[0].reason.contains("duplicate"));
        assert!(resolution.has_errors());
        assert!(resolution.node("ruby").unwrap().installable);
    }

    #[test]
    fn conflicts_block_both_mods() {
        let mut a = scan("a", "1", &[]);
        a.metadata.as_mut().unwrap().conflicts = vec![dependency("b", "*")];
        let scans = [a, scan("b", "1", &[])];
        let resolution = resolve(&scans, &EnvironmentInfo::default()).unwrap();
        assert!(!resolution.node("a").unwrap().installable);
        assert!(!resolution.node("b").unwrap().installable);
        assert!(resolution.order.is_empty());
    }

    #[test]
    fn cycles_are_reported_without_arbitrary_order() {
        let scans = [scan("a", "1", &[("b", "*")]), scan("b", "1", &[("a", "*")])];
        let resolution = resolve(&scans, &EnvironmentInfo::default()).unwrap();
        let issue = resolution
            .issues
            .iter()
            .find(|i| i.code == IssueCode::DependencyCycle)
            .expect("cycle issue");
        assert!(issue.message.contains("a"));
        assert!(resolution.order.is_empty());
        assert!(resolution.mods.iter().all(|m| !m.runtime_ready));
    }

    #[test]
    fn malformed_jars_are_skipped_not_panics() {
        let mut bad = scan("bad", "1", &[]);
        bad.metadata = None;
        bad.error = Some("not a readable zip".into());
        let resolution = resolve(&[bad], &EnvironmentInfo::default()).unwrap();
        assert!(resolution.mods.is_empty());
        assert_eq!(resolution.skipped.len(), 1);
        assert!(resolution.has_errors());
    }

    #[test]
    fn environment_versions_are_used_for_builtin_dependencies() {
        let env = EnvironmentInfo {
            minecraft: "1.21.1".into(),
            fabric_loader: "0.16.0".into(),
            java: "21".into(),
        };
        let scans = [scan(
            "ok",
            "1",
            &[
                ("minecraft", "^1.21"),
                ("fabricloader", ">=0.15"),
                ("java", ">=21"),
                ("fabric-api", ">=0.160"),
                ("fabric", "*"),
            ],
        )];
        let resolution = resolve(&scans, &env).unwrap();
        assert!(resolution.node("ok").unwrap().runtime_ready);
        assert!(!resolution.has_errors());
    }
}
