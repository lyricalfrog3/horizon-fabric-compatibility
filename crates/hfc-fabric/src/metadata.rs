//! `fabric.mod.json`, read as data and never executed.
//!
//! The model follows Fabric's published specification (schema version 1). It
//! keeps what the compatibility layer needs: identity, licence, side,
//! entrypoints, mixins, access wideners, nested jars and the five dependency
//! maps. Unknown keys are tolerated, as the specification requires.

use crate::version::VersionRange;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const FABRIC_MOD_JSON: &str = "fabric.mod.json";

/// A person in `authors` or `contributors`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Person {
    Name(String),
    Detailed {
        name: String,
        #[serde(default)]
        contact: BTreeMap<String, String>,
    },
}

impl Person {
    pub fn name(&self) -> &str {
        match self {
            Person::Name(name) => name,
            Person::Detailed { name, .. } => name,
        }
    }
}

/// One entrypoint: the default adapter's class name, or a named adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Entrypoint {
    Value(String),
    Detailed {
        #[serde(default)]
        adapter: Option<String>,
        value: String,
    },
}

impl Entrypoint {
    pub fn value(&self) -> &str {
        match self {
            Entrypoint::Value(value) => value,
            Entrypoint::Detailed { value, .. } => value,
        }
    }

    pub fn adapter(&self) -> &str {
        match self {
            Entrypoint::Value(_) => "default",
            Entrypoint::Detailed { adapter, .. } => adapter.as_deref().unwrap_or("default"),
        }
    }
}

/// A declared mixin configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MixinConfig {
    Path(String),
    Detailed {
        config: String,
        #[serde(default)]
        environment: Option<String>,
    },
}

impl MixinConfig {
    pub fn path(&self) -> &str {
        match self {
            MixinConfig::Path(path) => path,
            MixinConfig::Detailed { config, .. } => config,
        }
    }

    pub fn environment(&self) -> Option<&str> {
        match self {
            MixinConfig::Path(_) => None,
            MixinConfig::Detailed { environment, .. } => environment.as_deref(),
        }
    }
}

/// A nested jar Fabric Loader loads alongside the outer mod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NestedJar {
    pub file: String,
}

/// The side a mod declares.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Client,
    Server,
    #[default]
    Both,
}

/// A dependency map entry: the id and its declared range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependency {
    pub id: String,
    pub range: VersionRange,
}

/// Parsed Fabric metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub authors: Vec<String>,
    pub contributors: Vec<String>,
    pub contact: BTreeMap<String, String>,
    pub license: Vec<String>,
    pub environment: Environment,
    pub entrypoints: BTreeMap<String, Vec<Entrypoint>>,
    pub mixins: Vec<MixinConfig>,
    pub access_widener: Option<String>,
    pub jars: Vec<NestedJar>,
    pub depends: Vec<Dependency>,
    pub recommends: Vec<Dependency>,
    pub suggests: Vec<Dependency>,
    pub conflicts: Vec<Dependency>,
    pub breaks: Vec<Dependency>,
    /// The raw `custom` object, kept for provenance but never interpreted.
    pub custom: Value,
    /// The raw icon value (a path or width-to-path map).
    pub icon: Option<Value>,
}

impl Metadata {
    /// Parse the bytes of a `fabric.mod.json`. The specification allows a
    /// top-level object, or an array of objects; Loader uses the first.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|e| format!("{FABRIC_MOD_JSON}: not valid JSON: {e}"))?;
        let value = match value {
            Value::Array(mut items) if !items.is_empty() => items.remove(0),
            Value::Object(_) => value,
            Value::Array(_) => return Err(format!("{FABRIC_MOD_JSON}: an empty array")),
            _ => return Err(format!("{FABRIC_MOD_JSON}: not an object")),
        };
        Self::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| format!("{FABRIC_MOD_JSON}: not an object"))?;
        let schema_version = schema_version(object)?;
        if schema_version > 1 {
            return Err(format!(
                "{FABRIC_MOD_JSON}: schemaVersion {schema_version} is newer than HFC reads (1)"
            ));
        }
        let id = string(object, "id")
            .ok_or_else(|| format!("{FABRIC_MOD_JSON}: missing the mandatory \"id\""))?;
        validate_id(&id)?;
        let version = string(object, "version")
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| format!("{FABRIC_MOD_JSON}: missing the mandatory \"version\""))?;

        let license = match object.get("license") {
            Some(Value::String(text)) => vec![text.clone()],
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            Some(other) => {
                return Err(format!(
                    "{FABRIC_MOD_JSON}: \"license\" is {}, not a string or array",
                    kind(other)
                ));
            }
            None => Vec::new(),
        };

        let authors = people(object.get("authors"));
        let contributors = people(object.get("contributors"));
        let contact = string_map(object.get("contact"));

        let environment = match object.get("environment") {
            None => Environment::Both,
            Some(Value::String(text)) => parse_environment(text)?,
            Some(Value::Array(items)) => {
                let mut client = false;
                let mut server = false;
                for item in items {
                    match item.as_str().map(parse_environment) {
                        Some(Ok(Environment::Client)) => client = true,
                        Some(Ok(Environment::Server)) => server = true,
                        Some(Ok(Environment::Both)) => {
                            client = true;
                            server = true;
                        }
                        _ => {
                            return Err(format!(
                                "{FABRIC_MOD_JSON}: \"environment\" holds a value that is not \
                                 client, server or *"
                            ));
                        }
                    }
                }
                match (client, server) {
                    (true, true) => Environment::Both,
                    (true, false) => Environment::Client,
                    (false, true) => Environment::Server,
                    (false, false) => Environment::Both,
                }
            }
            Some(other) => {
                return Err(format!(
                    "{FABRIC_MOD_JSON}: \"environment\" is {}, not a string or array",
                    kind(other)
                ));
            }
        };

        let entrypoints = match object.get("entrypoints") {
            None => BTreeMap::new(),
            Some(Value::Object(map)) => {
                let mut out = BTreeMap::new();
                for (key, list) in map {
                    let parsed: Result<Vec<Entrypoint>, String> = match list {
                        Value::Array(items) => items
                            .iter()
                            .map(|item| {
                                serde_json::from_value::<Entrypoint>(item.clone()).map_err(|e| {
                                    format!(
                                        "{FABRIC_MOD_JSON}: entrypoint {key:?} is malformed: {e}"
                                    )
                                })
                            })
                            .collect(),
                        Value::String(_) | Value::Object(_) => {
                            Ok(vec![serde_json::from_value(list.clone()).map_err(|e| {
                                format!("{FABRIC_MOD_JSON}: entrypoint {key:?} is malformed: {e}")
                            })?])
                        }
                        other => Err(format!(
                            "{FABRIC_MOD_JSON}: entrypoint {key:?} is {}, not a string or array",
                            kind(other)
                        )),
                    };
                    let parsed = parsed?;
                    if !parsed.is_empty() {
                        out.insert(key.clone(), parsed);
                    }
                }
                out
            }
            Some(other) => {
                return Err(format!(
                    "{FABRIC_MOD_JSON}: \"entrypoints\" is {}, not an object",
                    kind(other)
                ));
            }
        };

        let mixins = match object.get("mixins") {
            None => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    serde_json::from_value::<MixinConfig>(item.clone()).map_err(|e| {
                        format!("{FABRIC_MOD_JSON}: a mixin configuration is malformed: {e}")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(other) => {
                return Err(format!(
                    "{FABRIC_MOD_JSON}: \"mixins\" is {}, not an array",
                    kind(other)
                ));
            }
        };

        let access_widener = match object.get("accessWidener") {
            None | Some(Value::Null) => None,
            Some(Value::String(path)) => Some(path.clone()),
            Some(other) => {
                return Err(format!(
                    "{FABRIC_MOD_JSON}: \"accessWidener\" is {}, not a string",
                    kind(other)
                ));
            }
        };

        let jars = match object.get("jars") {
            None => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    serde_json::from_value::<NestedJar>(item.clone()).map_err(|e| {
                        format!("{FABRIC_MOD_JSON}: a nested jar entry is malformed: {e}")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(other) => {
                return Err(format!(
                    "{FABRIC_MOD_JSON}: \"jars\" is {}, not an array",
                    kind(other)
                ));
            }
        };

        let depends = dependencies(object, "depends")?;
        let recommends = dependencies(object, "recommends")?;
        let suggests = dependencies(object, "suggests")?;
        let conflicts = dependencies(object, "conflicts")?;
        let breaks = dependencies(object, "breaks")?;

        Ok(Self {
            schema_version,
            id,
            version,
            name: string(object, "name").unwrap_or_default(),
            description: string(object, "description").unwrap_or_default(),
            authors,
            contributors,
            contact,
            license,
            environment,
            entrypoints,
            mixins,
            access_widener,
            jars,
            depends,
            recommends,
            suggests,
            conflicts,
            breaks,
            custom: object.get("custom").cloned().unwrap_or(Value::Null),
            icon: object.get("icon").cloned(),
        })
    }

    /// The number of declared entrypoints, across all types.
    pub fn entrypoint_count(&self) -> usize {
        self.entrypoints.values().map(Vec::len).sum()
    }

    /// Whether the mod declares code Fabric Loader would run.
    pub fn has_code_signals(&self) -> bool {
        self.entrypoint_count() > 0 || !self.mixins.is_empty() || self.access_widener.is_some()
    }

    pub fn license_text(&self) -> String {
        if self.license.is_empty() {
            "unknown".into()
        } else {
            self.license.join(" OR ")
        }
    }

    pub fn environment_label(&self) -> &'static str {
        match self.environment {
            Environment::Client => "client",
            Environment::Server => "server",
            Environment::Both => "both",
        }
    }
}

fn string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// `schemaVersion`, which is optional (absent means 0, Loader's inferred
/// version). A present value must be a non-negative integer that fits `u32`:
/// truncating a larger number would wrap `4294967296` to `0` and
/// `4294967297` to `1`, silently passing the "newer than HFC reads" guard.
fn schema_version(object: &serde_json::Map<String, Value>) -> Result<u32, String> {
    let Some(value) = object.get("schemaVersion") else {
        return Ok(0);
    };
    let number = value.as_u64().ok_or_else(|| {
        format!(
            "{FABRIC_MOD_JSON}: \"schemaVersion\" is {}, not a non-negative integer",
            kind(value)
        )
    })?;
    u32::try_from(number).map_err(|_| {
        format!(
            "{FABRIC_MOD_JSON}: schemaVersion {number} does not fit a 32-bit integer; \
             this metadata is not readable as schema 1"
        )
    })
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn people(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(name) => Some(name.clone()),
                Value::Object(object) => object
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                _ => None,
            })
            .collect(),
        Some(Value::String(name)) => vec![name.clone()],
        _ => Vec::new(),
    }
}

fn string_map(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_environment(text: &str) -> Result<Environment, String> {
    match text {
        "*" => Ok(Environment::Both),
        "client" => Ok(Environment::Client),
        "server" => Ok(Environment::Server),
        other => Err(format!(
            "{FABRIC_MOD_JSON}: \"environment\" value {other:?} is not client, server or *"
        )),
    }
}

fn dependencies(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Vec<Dependency>, String> {
    let Some(value) = object.get(key) else {
        return Ok(Vec::new());
    };
    let map = value.as_object().ok_or_else(|| {
        format!(
            "{FABRIC_MOD_JSON}: \"{key}\" is {}, not an object",
            kind(value)
        )
    })?;
    let mut out = Vec::new();
    for (id, range) in map {
        let Some(range) = VersionRange::from_json(range) else {
            return Err(format!(
                "{FABRIC_MOD_JSON}: dependency {id:?} has {}, not a version range",
                kind(range)
            ));
        };
        out.push(Dependency {
            id: id.clone(),
            range,
        });
    }
    Ok(out)
}

/// Fabric's mod id pattern is `^[a-z][a-z0-9-_]{1,63}$`. We accept a leading
/// digit too, because real jars predate that rule, but never uppercase,
/// whitespace, a path separator or an empty id.
pub fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 64 {
        return Err(format!(
            "{FABRIC_MOD_JSON}: mod id {id:?} must be 1 to 64 characters"
        ));
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap_or_default();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(format!(
            "{FABRIC_MOD_JSON}: mod id {id:?} must start with a lower-case letter or a digit"
        ));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(format!(
            "{FABRIC_MOD_JSON}: mod id {id:?} may hold only lower-case letters, digits, '-' and '_'"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::Match;

    #[test]
    fn reads_a_full_manifest() {
        let json = br#"{
            "schemaVersion": 1,
            "id": "ruby",
            "version": "1.2.0",
            "name": "Ruby",
            "description": "Gems",
            "authors": ["Ada", {"name": "Grace", "contact": {"homepage": "https://x"}}],
            "license": ["MIT", "Apache-2.0"],
            "environment": "client",
            "entrypoints": {
                "main": ["ruby.Ruby", {"adapter": "kotlin", "value": "ruby.Kt::init"}],
                "client": {"value": "ruby.Client"}
            },
            "mixins": ["ruby.mixins.json", {"config": "ruby.client.mixins.json", "environment": "client"}],
            "accessWidener": "ruby.accesswidener",
            "jars": [{"file": "META-INF/jars/lib.jar"}],
            "depends": {"fabricloader": ">=0.15", "minecraft": "~1.21.1", "cloth-config": "*"},
            "conflicts": {"old-ruby": "<1.0"},
            "custom": {"modmenu": {"badges": ["x"]}}
        }"#;
        let m = Metadata::parse(json).unwrap();
        assert_eq!((m.id.as_str(), m.version.as_str()), ("ruby", "1.2.0"));
        assert_eq!(m.authors, ["Ada", "Grace"]);
        assert_eq!(m.license_text(), "MIT OR Apache-2.0");
        assert_eq!(m.environment, Environment::Client);
        assert_eq!(m.entrypoint_count(), 3);
        assert_eq!(m.mixins.len(), 2);
        assert_eq!(m.access_widener.as_deref(), Some("ruby.accesswidener"));
        assert_eq!(m.jars[0].file, "META-INF/jars/lib.jar");
        assert_eq!(m.depends.len(), 3);
        assert_eq!(
            m.depends
                .iter()
                .find(|d| d.id == "minecraft")
                .unwrap()
                .range
                .matches("1.21.1"),
            Match::Yes
        );
        assert!(m.has_code_signals());
    }

    #[test]
    fn missing_and_malformed_metadata_is_an_error_not_a_panic() {
        assert!(Metadata::parse(b"not json").is_err());
        assert!(
            Metadata::parse(br#"{"id": "x"}"#)
                .unwrap_err()
                .contains("version")
        );
        assert!(
            Metadata::parse(br#"{"version": "1"}"#)
                .unwrap_err()
                .contains("id")
        );
        assert!(Metadata::parse(br#"{"id": "Bad Id", "version": "1"}"#).is_err());
        assert!(Metadata::parse(br#"{"id": "x", "version": "1", "license": 3}"#).is_err());
        assert!(Metadata::parse(br#"[]"#).is_err());
        assert!(Metadata::parse(br#"{"id": "x", "version": "1", "schemaVersion": 9}"#).is_err());
    }

    #[test]
    fn schema_version_is_not_truncated() {
        // 2^32 wraps to 0 and 2^32 + 1 wraps to 1 under `as u32`; both are
        // newer than HFC reads and must be rejected, not accepted as 0/1.
        for number in ["4294967296", "4294967297", "18446744073709551615"] {
            let json = format!(r#"{{"id":"x","version":"1","schemaVersion":{number}}}"#);
            let error = Metadata::parse(json.as_bytes()).unwrap_err();
            assert!(error.contains("schemaVersion"), "{number}: {error}");
        }
        // A negative or fractional number is not a schema version either.
        for number in ["-1", "1.5", "-0.5"] {
            let json = format!(r#"{{"id":"x","version":"1","schemaVersion":{number}}}"#);
            assert!(
                Metadata::parse(json.as_bytes()).is_err(),
                "{number} was accepted"
            );
        }
        // A string is not a number, even when it looks like one.
        assert!(Metadata::parse(br#"{"id":"x","version":"1","schemaVersion":"1"}"#).is_err());
        // The supported values still parse, and an absent one means schema 0.
        assert_eq!(
            Metadata::parse(br#"{"id":"x","version":"1","schemaVersion":1}"#)
                .unwrap()
                .schema_version,
            1
        );
        assert_eq!(
            Metadata::parse(br#"{"id":"x","version":"1"}"#)
                .unwrap()
                .schema_version,
            0
        );
    }

    #[test]
    fn accepts_the_specified_array_form() {
        let m = Metadata::parse(br#"[{"id": "one", "version": "1"}]"#).unwrap();
        assert_eq!(m.id, "one");
        assert_eq!(m.environment, Environment::Both);
    }
}
