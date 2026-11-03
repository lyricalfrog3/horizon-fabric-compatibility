//! Fabric jar discovery and analysis.
//!
//! Nothing in this crate executes Java or extracts a jar. It reads only
//! metadata and file names, so an untrusted `.jar` can be inspected before any
//! decision is made about it.
//!
//! The pieces are deliberately separate:
//!
//! - [`version`] implements Fabric's version-range contract.
//! - [`metadata`] parses `fabric.mod.json`.
//! - [`archive`] opens the zip safely, takes a census and follows declared
//!   nested jars to a bounded depth.
//! - [`graph`] turns many scans into providers, issues and a load order.

pub mod archive;
pub mod graph;
pub mod metadata;
pub mod version;

pub use archive::{
    Census, DiscoveredJar, JarScan, Loader, NestedScan, census_map, discover, scan_path,
};
pub use graph::{
    EnvironmentInfo, Issue, IssueCode, ModNode, Origin, Resolution, Severity, SkippedJar, resolve,
};
pub use metadata::{
    Dependency, Entrypoint, Environment, FABRIC_MOD_JSON, Metadata, MixinConfig, Person,
    validate_id,
};
pub use version::{BUILTIN_PROVIDERS, Match, Version, VersionRange, is_builtin_provider};
