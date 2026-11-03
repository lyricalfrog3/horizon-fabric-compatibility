//! Fabric's version ranges.
//!
//! Fabric Loader compares versions with a superset of Semantic Versioning: an
//! arbitrary number of numeric components compares left to right, an empty
//! pre-release is the earliest pre-release, and the operators `=`, `>=`, `>`,
//! `<=`, `<`, `^`, `~` plus X-ranges (`1.20.x`) are supported. A range is a
//! string of space-separated AND-ed comparators, and a JSON array of them is
//! OR-ed. This module implements that contract without pulling in a dependency
//! and returns [`Match::Unknown`] for versions that are not comparable, so a
//! scanner never guesses that an unusual version satisfies a constraint.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A Fabric version. Missing components are treated as zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    components: Vec<u64>,
    /// `None` for a release, `Some("")` for the empty pre-release, which is
    /// earlier than every other component, `Some("alpha.1")` otherwise.
    pre: Option<String>,
}

impl Version {
    /// Parse a version, or return None when it is not a comparable Semantic
    /// Versioning superset.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let without_build = text.split_once('+').map_or(text, |(v, _)| v);
        let (core, pre) = match without_build.split_once('-') {
            Some((core, pre)) => (core, Some(pre.to_owned())),
            None => (without_build, None),
        };
        if core.is_empty() {
            return None;
        }
        let mut components = Vec::new();
        for part in core.split('.') {
            if part.is_empty() {
                return None;
            }
            components.push(part.parse::<u64>().ok()?);
        }
        if components.is_empty() {
            return None;
        }
        Some(Self { components, pre })
    }

    fn component(&self, index: usize) -> u64 {
        self.components.get(index).copied().unwrap_or(0)
    }

    fn with_prefix(&self, len: usize, next: u64) -> Self {
        let mut components: Vec<u64> = (0..len).map(|i| self.component(i)).collect();
        if let Some(last) = components.last_mut() {
            *last = next;
        }
        Self {
            components,
            pre: None,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, c) in self.components.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            write!(f, "{c}")?;
        }
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let len = self.components.len().max(other.components.len());
        for i in 0..len {
            match self.component(i).cmp(&other.component(i)) {
                std::cmp::Ordering::Equal => {}
                order => return order,
            }
        }
        match (&self.pre, &other.pre) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (Some(a), Some(b)) => compare_pre(a, b),
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// SemVer pre-release precedence, with Fabric's empty pre-release first.
fn compare_pre(a: &str, b: &str) -> std::cmp::Ordering {
    if a == b {
        return std::cmp::Ordering::Equal;
    }
    if a.is_empty() {
        return std::cmp::Ordering::Less;
    }
    if b.is_empty() {
        return std::cmp::Ordering::Greater;
    }
    let mut left = a.split('.');
    let mut right = b.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => {
                let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// The outcome of matching a declared constraint against a provided version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Match {
    Yes,
    No,
    /// The versions could not be compared (a non-semver provider version, or a
    /// constraint HFC does not understand).
    Unknown,
}

impl Match {
    pub fn as_bool(self) -> Option<bool> {
        match self {
            Match::Yes => Some(true),
            Match::No => Some(false),
            Match::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
    Caret,
    Tilde,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Comparator {
    op: Op,
    version: Version,
}

/// A parsed version range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionRange {
    raw: String,
    /// OR of AND-ed comparators. An empty outer list means `*`.
    #[serde(skip)]
    alternatives: Vec<Vec<Comparator>>,
    /// Set when the range used syntax this implementation could not parse;
    /// matching then returns [`Match::Unknown`] rather than a false answer.
    #[serde(skip)]
    malformed: bool,
}

impl VersionRange {
    pub fn any() -> Self {
        Self {
            raw: "*".into(),
            alternatives: Vec::new(),
            malformed: false,
        }
    }

    /// Parse one Fabric version range string. A string may hold AND-ed
    /// comparators separated by spaces and/or commas.
    pub fn parse(raw: impl Into<String>) -> Self {
        let raw = raw.into();
        let mut alternatives = Vec::new();
        let mut malformed = false;
        for alternative in raw.split("||") {
            let mut comparators = Vec::new();
            for word in alternative.split(|c: char| c.is_whitespace() || c == ',') {
                let word = word.trim();
                if word.is_empty() {
                    continue;
                }
                match parse_comparator(word) {
                    Some(parsed) => comparators.extend(parsed),
                    None => malformed = true,
                }
            }
            if !comparators.is_empty() {
                alternatives.push(comparators);
            }
        }
        if alternatives.is_empty() && !malformed {
            alternatives.push(Vec::new());
        }
        Self {
            raw,
            alternatives,
            malformed,
        }
    }

    /// A range from a Fabric JSON value: a string, or an array of strings
    /// (OR-ed, with each string's comparators AND-ed).
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        match value {
            serde_json::Value::String(text) => Some(Self::parse(text)),
            serde_json::Value::Array(items) => {
                let parts: Vec<String> = items
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_owned)
                    .collect();
                if parts.is_empty() {
                    return Some(Self::parse(""));
                }
                let mut joined = Vec::new();
                let mut malformed = false;
                for part in parts {
                    let parsed = Self::parse(part);
                    if parsed.malformed {
                        malformed = true;
                    }
                    joined.extend(parsed.alternatives);
                }
                Some(Self {
                    raw: items
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(" || "),
                    alternatives: joined,
                    malformed,
                })
            }
            _ => None,
        }
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn is_any(&self) -> bool {
        self.raw.trim() == "*" || (self.alternatives.iter().all(Vec::is_empty) && !self.malformed)
    }

    /// Whether `provided` satisfies the range.
    pub fn matches(&self, provided: &str) -> Match {
        if self.malformed {
            return Match::Unknown;
        }
        let Some(provided) = Version::parse(provided) else {
            // Fabric accepts non-semver versions for equality only. We can
            // still answer an exact textual match, but nothing else.
            return if self
                .alternatives
                .iter()
                .filter(|a| a.len() == 1 && a[0].op == Op::Eq)
                .any(|a| a[0].version.to_string() == provided)
            {
                Match::Yes
            } else {
                Match::Unknown
            };
        };
        if self.alternatives.is_empty() {
            return Match::Yes;
        }
        let yes = self
            .alternatives
            .iter()
            .any(|comparators| comparators.iter().all(|c| comparator_matches(c, &provided)));
        if yes { Match::Yes } else { Match::No }
    }
}

impl fmt::Display for VersionRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

fn parse_comparator(word: &str) -> Option<Vec<Comparator>> {
    if word == "*" || word.eq_ignore_ascii_case("x") {
        return Some(Vec::new());
    }
    let (op, rest) = if let Some(rest) = word.strip_prefix(">=") {
        (Op::Ge, rest)
    } else if let Some(rest) = word.strip_prefix("<=") {
        (Op::Le, rest)
    } else if let Some(rest) = word.strip_prefix('>') {
        (Op::Gt, rest)
    } else if let Some(rest) = word.strip_prefix('<') {
        (Op::Lt, rest)
    } else if let Some(rest) = word.strip_prefix('=') {
        (Op::Eq, rest)
    } else if let Some(rest) = word.strip_prefix('^') {
        (Op::Caret, rest)
    } else if let Some(rest) = word.strip_prefix('~') {
        (Op::Tilde, rest)
    } else {
        (Op::Eq, word)
    };
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    if rest == "*" || rest.eq_ignore_ascii_case("x") {
        return Some(Vec::new());
    }
    // Strip build metadata (+...) before inspecting components or wildcards.
    // SemVer 2.0.0 and Fabric Loader ignore build metadata for precedence.
    let without_build = rest.split_once('+').map_or(rest, |(v, _)| v);
    // An X-range: the first `.x`, `.X` or `.*` component and everything after
    // it are a wildcard.
    let parts: Vec<&str> = without_build.split('.').collect();
    let wildcard = parts
        .iter()
        .position(|p| p.eq_ignore_ascii_case("x") || *p == "*");
    if let Some(index) = wildcard {
        if index == 0 {
            return Some(Vec::new());
        }
        let numeric: Option<Vec<u64>> = parts[..index]
            .iter()
            .map(|p| p.parse::<u64>().ok())
            .collect();
        let numeric = numeric?;
        let lower = Version {
            components: numeric,
            pre: None,
        };
        let upper = lower.with_prefix(index, lower.component(index - 1) + 1);
        return Some(vec![
            Comparator {
                op: Op::Ge,
                version: lower,
            },
            Comparator {
                op: Op::Lt,
                version: upper,
            },
        ]);
    }
    let version = Version::parse(without_build)?;
    let comparators = match op {
        Op::Caret => {
            let upper = version.with_prefix(1, version.component(0) + 1);
            vec![
                Comparator {
                    op: Op::Ge,
                    version: version.clone(),
                },
                Comparator {
                    op: Op::Lt,
                    version: upper,
                },
            ]
        }
        Op::Tilde => {
            let upper = if version.components.len() >= 2 {
                version.with_prefix(2, version.component(1) + 1)
            } else {
                version.with_prefix(1, version.component(0) + 1)
            };
            vec![
                Comparator {
                    op: Op::Ge,
                    version: version.clone(),
                },
                Comparator {
                    op: Op::Lt,
                    version: upper,
                },
            ]
        }
        op => vec![Comparator { op, version }],
    };
    Some(comparators)
}

fn comparator_matches(comparator: &Comparator, provided: &Version) -> bool {
    match comparator.op {
        Op::Eq => provided == &comparator.version,
        Op::Gt => provided > &comparator.version,
        Op::Ge => provided >= &comparator.version,
        Op::Lt => provided < &comparator.version,
        Op::Le => provided <= &comparator.version,
        Op::Caret | Op::Tilde => unreachable!("expanded while parsing"),
    }
}

/// The provider versions HFC treats as part of the environment rather than as
/// mods that must be present in the folder.
pub const BUILTIN_PROVIDERS: [&str; 6] = [
    "minecraft",
    "java",
    "fabricloader",
    "quilt_loader",
    "forge",
    "neoforge",
];

pub fn is_builtin_provider(id: &str) -> bool {
    BUILTIN_PROVIDERS.contains(&id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yes(range: &str, version: &str) -> bool {
        assert_eq!(
            VersionRange::parse(range).matches(version),
            Match::Yes,
            "{range} vs {version}"
        );
        true
    }

    fn no(range: &str, version: &str) -> bool {
        assert_eq!(
            VersionRange::parse(range).matches(version),
            Match::No,
            "{range} vs {version}"
        );
        true
    }

    #[test]
    fn comparators_follow_fabric() {
        assert!(yes("*", "0.1"));
        yes(">=1.20", "1.21.1");
        no(">=1.21", "1.20.4");
        yes(">=1.20 <1.22", "1.21.1");
        no(">=1.20 <1.21", "1.21.1");
        yes("1.20.x", "1.20.6");
        no("1.20.x", "1.21.0");
        yes("1.x", "1.99.99");
        no("1.x", "2.0.0");
        yes("^1.2.3", "1.9.0");
        no("^1.2.3", "2.0.0");
        yes("~1.2.3", "1.2.99");
        no("~1.2.3", "1.3.0");
        yes(">=1.20,<1.21", "1.20.1");
        yes(">=2.3.1+26.3.x", "2.3.1+26.3.x");
        yes(">=2.3.1+26.3.x", "2.3.2");
        no(">=2.3.1+26.3.x", "2.3.0");
        yes("~26.3-", "26.3");
        yes(">=26.1-rc.2", "26.3");
    }

    #[test]
    fn arrays_are_or_and_empty_prerelease_is_earliest() {
        let value = serde_json::json!([">=1.21", "<1.20"]);
        let range = VersionRange::from_json(&value).unwrap();
        assert_eq!(range.matches("1.20.1"), Match::No);
        assert_eq!(range.matches("1.22"), Match::Yes);
        let value = serde_json::json!([">=1.21", "<1.20"]);
        assert_eq!(
            VersionRange::from_json(&value).unwrap().matches("1.19"),
            Match::Yes
        );
        let early = Version::parse("1.2-").unwrap();
        let alpha = Version::parse("1.2-alpha").unwrap();
        let release = Version::parse("1.2").unwrap();
        assert!(early < alpha && alpha < release);
    }

    #[test]
    fn incomparable_versions_are_unknown_not_guessed() {
        assert_eq!(
            VersionRange::parse(">=1.20").matches("twenty"),
            Match::Unknown
        );
        assert_eq!(
            VersionRange::parse("weird ^ x").matches("1.0"),
            Match::Unknown
        );
    }
}
