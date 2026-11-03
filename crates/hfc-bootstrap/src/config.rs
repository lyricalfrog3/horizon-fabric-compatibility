//! `HorizonFabricCompat/hfc.toml`: the switch that makes HFC run.

use hfc_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const COMPAT_DIR_NAME: &str = hfc_core::COMPAT_DIR;
pub const CONFIG_FILE: &str = hfc_core::MARKER;

/// The compatibility layer's own configuration, beside the ordinary mods.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub compat: Compat,
    pub carry: Carry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Compat {
    /// When false HFC only scans and reports; nothing is installed.
    pub enabled: bool,
    /// The Minecraft version the carried data is upgraded for.
    pub minecraft: String,
    /// The Fabric Loader version HFC reports mods against.
    pub fabric_loader: String,
    /// The Java version HFC reports mods against.
    pub java: String,
    /// A hard cap on how many jars one run will scan.
    pub max_mods: usize,
    /// Set false to disable the conversion cache (every run reconverts).
    pub cache: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    /// Set false to scan and report without writing generated mods.
    pub enabled: bool,
}

impl Default for Compat {
    fn default() -> Self {
        Self {
            enabled: true,
            minecraft: "26.3".into(),
            fabric_loader: "0.19.3".into(),
            java: "25".into(),
            max_mods: 512,
            cache: true,
        }
    }
}

impl Default for Carry {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).map_err(|e| Error::new(CONFIG_FILE, e.to_string()))
    }

    pub fn load(compat_dir: &Path) -> Result<Self> {
        let path = compat_dir.join(CONFIG_FILE);
        let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        Self::parse(&text)
    }

    /// A starter configuration with comments, written by `hfc install`.
    pub fn template() -> String {
        let config = Self::default();
        format!(
            r#"# Horizon Fabric Compatibility.
#
# This file is the switch: while it exists, the launcher runs HFC before the
# game starts. Delete it (or set compat.enabled = false) to turn HFC off.
#
# Drop ordinary Fabric .jar files beside this folder, in the mods folder.
# Originals are only ever read. Generated mods are named after the mod's id
# and carry a hidden .hfc-generated.json marker so HFC can replace them safely.

[compat]
enabled = {}
# The Minecraft version the carried data is upgraded for.
minecraft = "{}"
fabric_loader = "{}"
java = "{}"
# Refuse to scan more jars than this in one run.
max_mods = {}
cache = {}

[carry]
# false = analyze and report only, install nothing.
enabled = {}
"#,
            config.compat.enabled,
            config.compat.minecraft,
            config.compat.fabric_loader,
            config.compat.java,
            config.compat.max_mods,
            config.compat.cache,
            config.carry.enabled,
        )
    }
}

/// The compatibility folder for a mods folder.
pub fn compat_dir(mods_dir: &Path) -> PathBuf {
    mods_dir.join(COMPAT_DIR_NAME)
}

/// The marker file that makes a folder an HFC installation.
pub fn marker(mods_dir: &Path) -> PathBuf {
    compat_dir(mods_dir).join(CONFIG_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_on_and_parse_tolerates_missing_keys() {
        let config = Config::parse("[compat]\nminecraft = \"1.20.1\"\n").unwrap();
        assert!(config.compat.enabled);
        assert!(config.carry.enabled);
        assert_eq!(config.compat.minecraft, "1.20.1");
        assert_eq!(config.compat.max_mods, Config::default().compat.max_mods);
    }

    #[test]
    fn malformed_config_is_an_error_with_the_file_name() {
        let error = Config::parse("compat = [").unwrap_err();
        assert!(error.to_string().contains(CONFIG_FILE));
    }

    #[test]
    fn template_parses_back_to_the_defaults() {
        let parsed = Config::parse(&Config::template()).unwrap();
        assert_eq!(parsed, Config::default());
    }
}
