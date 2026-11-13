//! Fabric's version ranges.
//!
//! Fabric Loader compares versions with a superset of Semantic Versioning: an
//! arbitrary number of numeric components compares left to right, an empty
//! pre-release is the earliest pre-release, and the operators `=`, `>=`, `>`,
//! `<=`, `<`, `^`, `~` plus X-ranges (`1.20.x`) are supported. A range is a
//! string of space-separated AND-ed comparators, and a JSON array of them is
//! OR-ed. This module implements that contract without pulling in a dependency.
//!
//! [`Match::Unknown`] is reserved for a range whose syntax HFC (like Fabric)
//! cannot parse; it is never used to mean "probably not". The answers here were
//! checked cell for cell against Fabric Loader 0.19.5 (see the golden table in
//! the tests). Three details follow Fabric rather than npm:
//!
//! - `^` is Fabric's `SAME_TO_NEXT_MAJOR`: the major component must be equal,
//!   with no special case for `0.x` (so `^0.1.2` reaches `0.9`). `~` is
//!   `SAME_TO_NEXT_MINOR`: both the major and the minor component must be
//!   equal, so `~1` only matches `1.0.x`, not `1.1`.
//! - A non-semver version (`20w14a`) is compared by friendly-string equality
//!   only. `>`/`<` with such a bound is an invalid range in Fabric, and an
//!   inclusive bound (`>=`, `<=`, `=`, `^`, `~`) is reduced to equality.
//! - A version component that does not fit Java's `int` makes the whole string
//!   non-semver, so it is never compared numerically by HFC either.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A Fabric version. Missing components are treated as zero, so `1.0` and
/// `1.0.0` are the same version (Fabric's `EQUAL` is `compareTo == 0`, not a
/// field-wise comparison); equality is defined through [`Ord`] to keep the two
/// consistent.
#[derive(Debug, Clone)]
pub struct Version {
    components: Vec<u64>,
    /// `None` for a release, `Some("")` for the empty pre-release, which is
    /// earlier than every other component, `Some("alpha.1")` otherwise.
    pre: Option<String>,
}

impl Version {
    /// Parse a version, or return None when it is not a comparable Semantic
    /// Versioning superset. A wildcard (`1.x`) is not a concrete version and is
    /// rejected here; only a range may hold one.
    pub fn parse(text: &str) -> Option<Self> {
        match parse_semver_shape(text.trim())? {
            Shape::Numeric(version) => Some(version),
            Shape::Wildcard { .. } => None,
        }
    }

    fn component(&self, index: usize) -> u64 {
        self.components.get(index).copied().unwrap_or(0)
    }

    /// The exclusive upper bound of a wildcard range: components `0..len`, with
    /// the last replaced by `next` and an empty pre-release (Fabric writes
    /// `<a.b.(c+1)-`, whose empty pre-release sorts before every other one).
    /// `None` when there is no component to cap.
    fn wildcard_upper(&self, len: usize, next: u64) -> Option<Self> {
        let mut components: Vec<u64> = (0..len).map(|i| self.component(i)).collect();
        let last = components.last_mut()?;
        *last = next;
        Some(Self {
            components,
            pre: Some(String::new()),
        })
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

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for Version {}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
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
                // Fabric: a numeric part is smaller than an alphanumeric one,
                // and two numeric parts compare by digit count then text (so
                // leading zeros and >64-bit runs keep Fabric's order).
                let order = match (is_digits(x), is_digits(y)) {
                    (true, true) => (x.len(), x).cmp(&(y.len(), y)),
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    (false, false) => x.cmp(y),
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
    /// The range used syntax this implementation cannot parse (Fabric rejects
    /// it too). Never a guess, and never treated as a match.
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

/// The right-hand side of one comparator. Fabric parses it as a semantic
/// version when it can, and keeps a non-semver version (`20w14a`) as a plain
/// string that only equality can answer.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Bound {
    Semver(Version),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Comparator {
    op: Op,
    bound: Bound,
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
    ///
    /// This follows Fabric Loader's `VersionPredicate`:
    ///
    /// - a semantic version is compared component-wise, with build metadata
    ///   ignored and an empty pre-release earlier than every other one;
    /// - a non-semver bound (`=20w14a`) is compared as the exact friendly
    ///   string, so `=20w14a` is Yes for `20w14a` and No for anything else.
    ///   An exclusive bound on such a version (`>20w14a`) is an invalid
    ///   predicate in Fabric, and HFC reports the range as malformed;
    /// - a non-semver *provider* is answered the same way Fabric does: `No`
    ///   unless its friendly string equals the bound, so `>=1.20` is `No` for
    ///   `twenty` and the resolver reports a mismatch instead of guessing.
    pub fn matches(&self, provided: &str) -> Match {
        if self.malformed {
            return Match::Unknown;
        }
        if self.alternatives.is_empty() {
            return Match::Yes;
        }
        let parsed = Version::parse(provided);
        let yes = self.alternatives.iter().any(|comparators| {
            comparators
                .iter()
                .all(|comparator| comparator_matches(comparator, parsed.as_ref(), provided))
        });
        if yes { Match::Yes } else { Match::No }
    }
}

impl fmt::Display for VersionRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

fn parse_comparator(word: &str) -> Option<Vec<Comparator>> {
    // Only a bare `*` means "any". A bare `x` is not a Fabric version at all:
    // VersionParser keeps it as a plain string, so `x` is equality with "x".
    if word == "*" {
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
    // Build metadata (`+...`) is ignored for precedence. A non-semver version
    // keeps it: Fabric compares that one by its exact friendly string.
    let without_build = rest.split_once('+').map_or(rest, |(v, _)| v);
    match parse_semver_shape(without_build) {
        Some(Shape::Wildcard { components, index }) => {
            // Fabric rejects an X-range with any operator but equality:
            // `>=1.20.x` is an invalid predicate, not ">=1.20".
            if op != Op::Eq {
                return None;
            }
            wildcard_range(components, index)
        }
        Some(Shape::Numeric(version)) => expand(op, version),
        None => {
            // Not a semantic version: Fabric keeps the raw string and reduces
            // every inclusive bound (`=`, `>=`, `<=`, `^`, `~`) to equality.
            // An exclusive bound has no meaning on a string, and Fabric
            // rejects that predicate.
            if matches!(op, Op::Gt | Op::Lt) {
                return None;
            }
            Some(vec![Comparator {
                op: Op::Eq,
                bound: Bound::Text(rest.to_owned()),
            }])
        }
    }
}

/// The semver superset Fabric's `SemanticVersionImpl` accepts, with a trailing
/// X-range collapsed to its numeric prefix.
enum Shape {
    Numeric(Version),
    /// `1.x`, `1.20.x`, `1.2.3.x`: `index` numeric components, then a wildcard.
    Wildcard {
        components: Vec<u64>,
        index: usize,
    },
}

fn parse_semver_shape(text: &str) -> Option<Shape> {
    if text.is_empty() {
        return None;
    }
    // Fabric splits build metadata first, then the pre-release, so a `-` inside
    // `+build-x` stays part of the build.
    let without_build = text.split_once('+').map_or(text, |(v, _)| v);
    let (core, pre) = match without_build.split_once('-') {
        Some((core, pre)) => (core, Some(pre.to_owned())),
        None => (without_build, None),
    };
    if core.is_empty() || core.ends_with('.') || core.starts_with('.') {
        return None;
    }
    // Fabric validates the pre-release as dot-separated alphanumerics and `-`.
    if let Some(pre) = &pre
        && !pre.is_empty()
        && !pre.split('.').all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
    {
        return None;
    }
    let parts: Vec<&str> = core.split('.').collect();
    let is_wildcard = |part: &&str| part.eq_ignore_ascii_case("x") || *part == "*";
    let wildcard = parts.iter().position(is_wildcard);
    let numeric_end = wildcard.unwrap_or(parts.len());
    // Fabric's components are Java `int`s: a component that does not fit is a
    // `VersionParsingException`, which makes the whole string a plain
    // `StringVersion`. Modelling that keeps `>=1.20` from claiming a match
    // against `2147483648.0`, which Fabric answers No to.
    let components: Option<Vec<u64>> = parts[..numeric_end]
        .iter()
        .map(|part| {
            part.parse::<u64>()
                .ok()
                .filter(|component| *component <= i32::MAX as u64)
        })
        .collect();
    let components = components?;
    match wildcard {
        // Every tail component must be a wildcard (`1.x.x` is `1.x`), and a
        // range may not start with one: Fabric rejects `x` and `x.1`.
        Some(0) => None,
        Some(index) => parts[index..]
            .iter()
            .all(is_wildcard)
            .then_some(Shape::Wildcard { components, index }),
        None => Some(Shape::Numeric(Version { components, pre })),
    }
}

/// Fabric's X-range expansion: `a.x` behaves like `^a-`, `a.b.x` like `~a.b-`,
/// and `a.b.c.x` like `>=a.b.c- <a.b.(c+1)-`. The `-` is an empty pre-release,
/// which is earlier than every other pre-release, so `1.20.x` also matches
/// `1.20.0-alpha`.
fn wildcard_range(components: Vec<u64>, index: usize) -> Option<Vec<Comparator>> {
    let lower = Version {
        components,
        pre: Some(String::new()),
    };
    let next = lower.component(index - 1).checked_add(1)?;
    let upper = lower.wildcard_upper(index, next)?;
    Some(vec![
        Comparator {
            op: Op::Ge,
            bound: Bound::Semver(lower),
        },
        Comparator {
            op: Op::Lt,
            bound: Bound::Semver(upper),
        },
    ])
}

/// Expand one parsed comparator into the comparators it stores.
///
/// Fabric's `^` (`SAME_TO_NEXT_MAJOR`) and `~` (`SAME_TO_NEXT_MINOR`) are kept as
/// operators instead of being rewritten to a `>=`/`<` pair, because a pair
/// cannot express the pinning they do: `^0.1.2` reaches `0.9` (only the major
/// is pinned, with no `0.x` special case), while `~1` reaches only `1.0.x`
/// (major *and* minor are pinned). The bounds are evaluated in
/// [`comparator_matches`], so no component arithmetic happens at parse time and
/// no crafted component can overflow a bound.
fn expand(op: Op, version: Version) -> Option<Vec<Comparator>> {
    Some(vec![Comparator {
        op,
        bound: Bound::Semver(version),
    }])
}

fn comparator_matches(comparator: &Comparator, provided: Option<&Version>, raw: &str) -> bool {
    match &comparator.bound {
        // A non-semver bound is stored as equality on the raw string, so the
        // comparison is text-against-text and never a guess.
        Bound::Text(text) => raw == text,
        Bound::Semver(reference) => {
            let Some(provided) = provided else {
                // Fabric's `VersionComparisonOperator.test` falls back to
                // friendly-string equality when either side is not a semantic
                // version, for every inclusive operator; an exclusive one
                // answers false. So a non-semver provider is `No` unless the
                // strings match exactly.
                return match comparator.op {
                    Op::Gt | Op::Lt => false,
                    _ => raw == reference.to_string(),
                };
            };
            match comparator.op {
                Op::Eq => provided == reference,
                Op::Gt => provided > reference,
                Op::Ge => provided >= reference,
                Op::Lt => provided < reference,
                Op::Le => provided <= reference,
                // Fabric's SAME_TO_NEXT_MAJOR: at least the reference, and the
                // major component equal. It does not special-case `0.x`.
                Op::Caret => {
                    provided >= reference && provided.component(0) == reference.component(0)
                }
                // SAME_TO_NEXT_MINOR: major and minor component equal. A
                // one-component reference pins minor 0, so `~1` stops at 1.0.x.
                Op::Tilde => {
                    provided >= reference
                        && provided.component(0) == reference.component(0)
                        && provided.component(1) == reference.component(1)
                }
            }
        }
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

    /// Verbatim answers from Fabric Loader 0.19.5 (`VersionPredicate.parse(..)
    /// .test(VersionParser.parse(..))`), used as the oracle for the cases HFC
    /// previously guessed or answered from a normalised string.
    #[test]
    fn non_semver_versions_answer_by_friendly_string_equality() {
        // `=20w14a` is Yes for exactly `20w14a`, and No for anything else.
        yes("=20w14a", "20w14a");
        yes("20w14a", "20w14a");
        no("=20w14a", "20w14b");
        no("=20w14a", "1.20");
        // Friendly-string equality is case sensitive in Fabric.
        no("=20w14a", "20W14a");
        // Fabric reduces an inclusive bound on a non-semver version to
        // equality, so `>=20w14a` and `~20w14a` both mean `=20w14a`.
        yes(">=20w14a", "20w14a");
        yes("~20w14a", "20w14a");
        // An exclusive bound cannot be evaluated on a non-semver version;
        // Fabric rejects the predicate outright, so the range is malformed.
        assert_eq!(
            VersionRange::parse(">20w14a").matches("20w14a"),
            Match::Unknown
        );
        assert_eq!(
            VersionRange::parse("<20w14a").matches("20w14a"),
            Match::Unknown
        );
        // A non-semver provider is only answered by equality: Fabric answers
        // No for an order it cannot derive from the string, and never guesses.
        // (`1.20-snapshot` is a *valid* semver pre-release and is comparable.)
        assert_eq!(VersionRange::parse(">=1.20").matches("20w14a"), Match::No);
        assert_eq!(
            VersionRange::parse(">=1.20").matches("1.20-snapshot"),
            Match::No
        );
        assert_eq!(VersionRange::parse("*").matches("twenty"), Match::Yes);
    }

    #[test]
    fn tilde_pins_the_minor_component_like_fabric() {
        // Fabric's SAME_TO_NEXT_MINOR pins major *and* minor, so `~1` is
        // `1.0.x`, not `>=1.0 <2.0`.
        yes("~1", "1.0");
        yes("~1", "1.0.5");
        no("~1", "1.1.0");
        no("~1", "1.2");
        yes("~1.2", "1.2.9");
        no("~1.2", "1.3.0");
        yes("~0.19", "0.19.5");
        no("~0.19", "0.20.0");
        // An equal major is not enough: `2.0-alpha` is above the `<2-` bound
        // but outside the pinned minor, and Fabric answers No.
        no("~1", "1.0-alpha");
        no("~1", "1.1-alpha");
    }

    #[test]
    fn caret_does_not_special_case_zero() {
        // Fabric's SAME_TO_NEXT_MAJOR pins only the major component, so
        // `^0.1.2` reaches `0.9` (unlike npm's caret).
        yes("^0.1.2", "0.1.9");
        yes("^0.1.2", "0.2.0");
        yes("^0", "0.9");
        yes("^1.2.3", "1.99.99");
        no("^0.1.2", "1.0.0");
        no("^1.2.3", "2.0.0");
        no("^1", "2.0-alpha");
    }

    #[test]
    fn wildcards_keep_fabrics_bounds() {
        yes("1.x", "1.0-alpha");
        yes("1.20.x", "1.20.0-alpha");
        no("1.20.x", "1.21.0-alpha");
        yes("1.2.3.x", "1.2.3.9");
        no("1.2.3.x", "1.2.4");
        no("1.2.3.x", "1.2.4-alpha");
        // `>=1.20.x` is not a valid Fabric predicate.
        assert_eq!(
            VersionRange::parse(">=1.20.x").matches("1.20.5"),
            Match::Unknown
        );
    }

    #[test]
    fn extreme_components_never_panic() {
        // This input used to add one to a u64::MAX component while expanding
        // `^`, panicking in a debug build. Fabric's components are Java `int`s,
        // so a component this large makes the whole string a plain
        // `StringVersion` and `^` reduces to equality on the raw text; the
        // answer is definite and the bounds are never computed.
        let range = VersionRange::parse("^18446744073709551615.0.0");
        assert_eq!(range.matches("1.0"), Match::No);
        assert_eq!(range.matches("18446744073709551615.0.0"), Match::Yes);
        // The same "cap a component" arithmetic is reached by a wildcard and by
        // `~`; neither may panic on a crafted component.
        assert_eq!(
            VersionRange::parse("18446744073709551615.x").matches("1.0"),
            Match::No
        );
        assert_eq!(
            VersionRange::parse("~1.18446744073709551615").matches("1.0"),
            Match::No
        );
        // A component that fits `int` but not u32 still expands normally, and
        // its successor still fits, so no saturation is involved.
        assert_eq!(
            VersionRange::parse("^2147483647").matches("2147483647.5"),
            Match::Yes
        );
        assert_eq!(
            VersionRange::parse("~1.2147483647").matches("1.2147483647.9"),
            Match::Yes
        );
    }

    /// A golden table from running Fabric Loader 0.19.5 itself over a matrix of
    /// ranges and versions (`VersionPredicate.parse(range).test(
    /// VersionParser.parse(version, false))`). Every range HFC supports appears
    /// at least once, with the pre-release, build-metadata and non-semver rows
    /// kept. Predicates Fabric rejects are omitted and covered by
    /// [`unknown_ranges_are_unknown_not_guessed`].
    #[test]
    fn matches_fabric_loader_answers() {
        const FABRIC: &[(&str, &str, Match)] = &[
            ("*", "1.20", Match::Yes),
            ("*", "1.20.1", Match::Yes),
            ("*", "1.20.6", Match::Yes),
            ("*", "2.0.0-alpha", Match::Yes),
            ("*", "2.0-alpha", Match::Yes),
            ("*", "1.20.0-alpha", Match::Yes),
            (">=1.20", "1.20", Match::Yes),
            (">=1.20", "1.20.1", Match::Yes),
            (">=1.20", "1.20.6", Match::Yes),
            (">=1.20", "2.0.0-alpha", Match::Yes),
            (">=1.20", "2.0-alpha", Match::Yes),
            (">=1.20", "1.99.0-alpha", Match::Yes),
            (">=1.20", "1.0", Match::No),
            (">=1.20", "1.0.0", Match::No),
            (">=1.20", "1.20.0-alpha", Match::No),
            (">=1.20", "1.0-alpha", Match::No),
            (">=1.21", "1.21", Match::Yes),
            (">=1.21", "1.21.1", Match::Yes),
            (">=1.21", "1.22", Match::Yes),
            (">=1.21", "2.0.0-alpha", Match::Yes),
            (">=1.21", "2.0-alpha", Match::Yes),
            (">=1.21", "1.99.0-alpha", Match::Yes),
            (">=1.21", "1.20", Match::No),
            (">=1.21", "1.20.1", Match::No),
            (">=1.21", "1.20.0-alpha", Match::No),
            (">=1.21", "1.0-alpha", Match::No),
            (">=1.20 <1.22", "1.20", Match::Yes),
            (">=1.20 <1.22", "1.20.1", Match::Yes),
            (">=1.20 <1.22", "1.20.6", Match::Yes),
            (">=1.20 <1.22", "1.22", Match::No),
            (">=1.20 <1.22", "2.0.0", Match::No),
            (">=1.20 <1.22", "2.0.0-alpha", Match::No),
            (">=1.20 <1.22", "2.0-alpha", Match::No),
            (">=1.20 <1.21", "1.20", Match::Yes),
            (">=1.20 <1.21", "1.20.1", Match::Yes),
            (">=1.20 <1.21", "1.20.6", Match::Yes),
            (">=1.20 <1.21", "1.21", Match::No),
            (">=1.20 <1.21", "1.21.1", Match::No),
            (">=1.20 <1.21", "2.0.0-alpha", Match::No),
            (">=1.20 <1.21", "2.0-alpha", Match::No),
            ("1.20.x", "1.20", Match::Yes),
            ("1.20.x", "1.20.1", Match::Yes),
            ("1.20.x", "1.20.6", Match::Yes),
            ("1.20.x", "1.20.0-alpha", Match::Yes),
            ("1.20.x", "1.20-snapshot", Match::Yes),
            ("1.20.x", "1.21", Match::No),
            ("1.20.x", "1.21.1", Match::No),
            ("1.20.x", "2.0.0-alpha", Match::No),
            ("1.20.x", "2.0-alpha", Match::No),
            ("1.x", "1.20", Match::Yes),
            ("1.x", "1.20.1", Match::Yes),
            ("1.x", "1.20.6", Match::Yes),
            ("1.x", "1.20.0-alpha", Match::Yes),
            ("1.x", "1.0-alpha", Match::Yes),
            ("1.x", "1.0-", Match::Yes),
            ("1.x", "2.0.0", Match::No),
            ("1.x", "2.0", Match::No),
            ("1.x", "2.0.0-alpha", Match::No),
            ("1.x", "2.0-alpha", Match::No),
            ("^1.2.3", "1.20", Match::Yes),
            ("^1.2.3", "1.20.1", Match::Yes),
            ("^1.2.3", "1.20.6", Match::Yes),
            ("^1.2.3", "1.20.0-alpha", Match::Yes),
            ("^1.2.3", "1.99.0-alpha", Match::Yes),
            ("^1.2.3", "1.3.0-alpha", Match::Yes),
            ("^1.2.3", "2.0.0", Match::No),
            ("^1.2.3", "2.0", Match::No),
            ("^1.2.3", "2.0.0-alpha", Match::No),
            ("^1.2.3", "2.0-alpha", Match::No),
            ("~1.2.3", "1.2.9", Match::Yes),
            ("~1.2.3", "1.2.99", Match::Yes),
            ("~1.2.3", "1.2.999", Match::Yes),
            ("~1.2.3", "1.2.4-alpha", Match::Yes),
            ("~1.2.3", "1.2.3+build", Match::Yes),
            ("~1.2.3", "1.20", Match::No),
            ("~1.2.3", "1.20.1", Match::No),
            ("~1.2.3", "2.0.0-alpha", Match::No),
            ("~1.2.3", "2.0-alpha", Match::No),
            ("~1", "1.0", Match::Yes),
            ("~1", "1.0.0", Match::Yes),
            ("~1", "1.0.5", Match::Yes),
            ("~1", "1.20", Match::No),
            ("~1", "1.20.1", Match::No),
            ("~1", "2.0.0-alpha", Match::No),
            ("~1", "2.0-alpha", Match::No),
            ("~1.2", "1.2", Match::Yes),
            ("~1.2", "1.2.9", Match::Yes),
            ("~1.2", "1.2.99", Match::Yes),
            ("~1.2", "1.2.4-alpha", Match::Yes),
            ("~1.2", "1.2.3-alpha", Match::Yes),
            ("~1.2", "1.2.3-beta", Match::Yes),
            ("~1.2", "1.20", Match::No),
            ("~1.2", "1.20.1", Match::No),
            ("~1.2", "2.0.0-alpha", Match::No),
            ("~1.2", "2.0-alpha", Match::No),
            ("^0.1.2", "0.9", Match::Yes),
            ("^0.1.2", "0.1.9", Match::Yes),
            ("^0.1.2", "0.2.0", Match::Yes),
            ("^0.1.2", "1.20", Match::No),
            ("^0.1.2", "1.20.1", Match::No),
            ("^0.1.2", "2.0.0-alpha", Match::No),
            ("^0.1.2", "2.0-alpha", Match::No),
            ("^0", "0.9", Match::Yes),
            ("^0", "0.1.9", Match::Yes),
            ("^0", "0.2.0", Match::Yes),
            ("^0", "1.20", Match::No),
            ("^0", "1.20.1", Match::No),
            ("^0", "2.0.0-alpha", Match::No),
            ("^0", "2.0-alpha", Match::No),
            ("1.2.3.x", "1.2.3.9", Match::Yes),
            ("1.2.3.x", "1.2.3-alpha", Match::Yes),
            ("1.2.3.x", "1.2.3-beta", Match::Yes),
            ("1.2.3.x", "1.2.3+build", Match::Yes),
            ("1.2.3.x", "1.20", Match::No),
            ("1.2.3.x", "1.20.1", Match::No),
            ("1.2.3.x", "2.0.0-alpha", Match::No),
            ("1.2.3.x", "2.0-alpha", Match::No),
            ("=20w14a", "20w14a", Match::Yes),
            ("=20w14a", "1.20", Match::No),
            ("=20w14a", "1.20.1", Match::No),
            ("=20w14a", "2.0.0-alpha", Match::No),
            ("=20w14a", "2.0-alpha", Match::No),
            ("20w14a", "20w14a", Match::Yes),
            ("20w14a", "1.20", Match::No),
            ("20w14a", "1.20.1", Match::No),
            ("20w14a", "2.0.0-alpha", Match::No),
            ("20w14a", "2.0-alpha", Match::No),
            (">=20w14a", "20w14a", Match::Yes),
            (">=20w14a", "1.20", Match::No),
            (">=20w14a", "1.20.1", Match::No),
            (">=20w14a", "2.0.0-alpha", Match::No),
            (">=20w14a", "2.0-alpha", Match::No),
            ("~20w14a", "20w14a", Match::Yes),
            ("~20w14a", "1.20", Match::No),
            ("~20w14a", "1.20.1", Match::No),
            ("~20w14a", "2.0.0-alpha", Match::No),
            ("~20w14a", "2.0-alpha", Match::No),
            ("=1.0", "1.0", Match::Yes),
            ("=1.0", "1.0.0", Match::Yes),
            ("=1.0", "1.20", Match::No),
            ("=1.0", "1.20.1", Match::No),
            ("=1.0", "2.0.0-alpha", Match::No),
            ("=1.0", "2.0-alpha", Match::No),
            ("=1.0.0", "1.0", Match::Yes),
            ("=1.0.0", "1.0.0", Match::Yes),
            ("=1.0.0", "1.20", Match::No),
            ("=1.0.0", "1.20.1", Match::No),
            ("=1.0.0", "2.0.0-alpha", Match::No),
            ("=1.0.0", "2.0-alpha", Match::No),
            ("1.0", "1.0", Match::Yes),
            ("1.0", "1.0.0", Match::Yes),
            ("1.0", "1.20", Match::No),
            ("1.0", "1.20.1", Match::No),
            ("1.0", "2.0.0-alpha", Match::No),
            ("1.0", "2.0-alpha", Match::No),
            ("~1.0.0", "1.0", Match::Yes),
            ("~1.0.0", "1.0.0", Match::Yes),
            ("~1.0.0", "1.0.5", Match::Yes),
            ("~1.0.0", "1.20", Match::No),
            ("~1.0.0", "1.20.1", Match::No),
            ("~1.0.0", "2.0.0-alpha", Match::No),
            ("~1.0.0", "2.0-alpha", Match::No),
            ("^1.0.0", "1.20", Match::Yes),
            ("^1.0.0", "1.20.1", Match::Yes),
            ("^1.0.0", "1.20.6", Match::Yes),
            ("^1.0.0", "1.20.0-alpha", Match::Yes),
            ("^1.0.0", "1.99.0-alpha", Match::Yes),
            ("^1.0.0", "1.3.0-alpha", Match::Yes),
            ("^1.0.0", "2.0.0", Match::No),
            ("^1.0.0", "2.0", Match::No),
            ("^1.0.0", "2.0.0-alpha", Match::No),
            ("^1.0.0", "2.0-alpha", Match::No),
            ("=1.2.3-alpha", "1.2.3-alpha", Match::Yes),
            ("=1.2.3-alpha", "1.20", Match::No),
            ("=1.2.3-alpha", "1.20.1", Match::No),
            ("=1.2.3-alpha", "2.0.0-alpha", Match::No),
            ("=1.2.3-alpha", "2.0-alpha", Match::No),
            ("1.2.3-alpha", "1.2.3-alpha", Match::Yes),
            ("1.2.3-alpha", "1.20", Match::No),
            ("1.2.3-alpha", "1.20.1", Match::No),
            ("1.2.3-alpha", "2.0.0-alpha", Match::No),
            ("1.2.3-alpha", "2.0-alpha", Match::No),
            ("~1.2.3-alpha", "1.2.9", Match::Yes),
            ("~1.2.3-alpha", "1.2.99", Match::Yes),
            ("~1.2.3-alpha", "1.2.999", Match::Yes),
            ("~1.2.3-alpha", "1.2.4-alpha", Match::Yes),
            ("~1.2.3-alpha", "1.2.3-alpha", Match::Yes),
            ("~1.2.3-alpha", "1.2.3-beta", Match::Yes),
            ("~1.2.3-alpha", "1.20", Match::No),
            ("~1.2.3-alpha", "1.20.1", Match::No),
            ("~1.2.3-alpha", "2.0.0-alpha", Match::No),
            ("~1.2.3-alpha", "2.0-alpha", Match::No),
            ("^1.2.3-alpha", "1.20", Match::Yes),
            ("^1.2.3-alpha", "1.20.1", Match::Yes),
            ("^1.2.3-alpha", "1.20.6", Match::Yes),
            ("^1.2.3-alpha", "1.20.0-alpha", Match::Yes),
            ("^1.2.3-alpha", "1.99.0-alpha", Match::Yes),
            ("^1.2.3-alpha", "1.3.0-alpha", Match::Yes),
            ("^1.2.3-alpha", "2.0.0", Match::No),
            ("^1.2.3-alpha", "2.0", Match::No),
            ("^1.2.3-alpha", "2.0.0-alpha", Match::No),
            ("^1.2.3-alpha", "2.0-alpha", Match::No),
            (">=2.3.1+26.3.x", "26.3", Match::Yes),
            (">=2.3.1+26.3.x", "26.3.0", Match::Yes),
            (">=2.3.1+26.3.x", "26.3-rc.1", Match::Yes),
            (">=2.3.1+26.3.x", "2.3.1+26.3.x", Match::Yes),
            (">=2.3.1+26.3.x", "1.20", Match::No),
            (">=2.3.1+26.3.x", "1.20.1", Match::No),
            (">=2.3.1+26.3.x", "2.0.0-alpha", Match::No),
            (">=2.3.1+26.3.x", "2.0-alpha", Match::No),
            ("~26.3-", "26.3", Match::Yes),
            ("~26.3-", "26.3.0", Match::Yes),
            ("~26.3-", "26.3-rc.1", Match::Yes),
            ("~26.3-", "1.20", Match::No),
            ("~26.3-", "1.20.1", Match::No),
            ("~26.3-", "2.0.0-alpha", Match::No),
            ("~26.3-", "2.0-alpha", Match::No),
            (">=26.1-rc.2", "26.3", Match::Yes),
            (">=26.1-rc.2", "26.3.0", Match::Yes),
            (">=26.1-rc.2", "26.3-rc.1", Match::Yes),
            (">=26.1-rc.2", "1.20", Match::No),
            (">=26.1-rc.2", "1.20.1", Match::No),
            (">=26.1-rc.2", "2.0.0-alpha", Match::No),
            (">=26.1-rc.2", "2.0-alpha", Match::No),
            ("~0.19", "0.19.5", Match::Yes),
            ("~0.19", "1.20", Match::No),
            ("~0.19", "1.20.1", Match::No),
            ("~0.19", "2.0.0-alpha", Match::No),
            ("~0.19", "2.0-alpha", Match::No),
            ("^0.19", "0.19.5", Match::Yes),
            ("^0.19", "0.20.0", Match::Yes),
            ("^0.19", "1.20", Match::No),
            ("^0.19", "1.20.1", Match::No),
            ("^0.19", "2.0.0-alpha", Match::No),
            ("^0.19", "2.0-alpha", Match::No),
            ("x", "x", Match::Yes),
            ("x", "1.20", Match::No),
            ("x", "1.20.1", Match::No),
            ("x", "2.0.0-alpha", Match::No),
            ("x", "2.0-alpha", Match::No),
            ("=x", "x", Match::Yes),
            ("=x", "1.20", Match::No),
            ("=x", "1.20.1", Match::No),
            ("=x", "2.0.0-alpha", Match::No),
            ("=x", "2.0-alpha", Match::No),
            ("1.*", "1.20", Match::Yes),
            ("1.*", "1.20.1", Match::Yes),
            ("1.*", "1.20.6", Match::Yes),
            ("1.*", "1.20.0-alpha", Match::Yes),
            ("1.*", "1.0-alpha", Match::Yes),
            ("1.*", "1.0-", Match::Yes),
            ("1.*", "2.0.0", Match::No),
            ("1.*", "2.0", Match::No),
            ("1.*", "2.0.0-alpha", Match::No),
            ("1.*", "2.0-alpha", Match::No),
            ("1.2.*", "1.2", Match::Yes),
            ("1.2.*", "1.2.9", Match::Yes),
            ("1.2.*", "1.2.99", Match::Yes),
            ("1.2.*", "1.2.4-alpha", Match::Yes),
            ("1.2.*", "1.2.3-alpha", Match::Yes),
            ("1.2.*", "1.2.3-beta", Match::Yes),
            ("1.2.*", "1.20", Match::No),
            ("1.2.*", "1.20.1", Match::No),
            ("1.2.*", "2.0.0-alpha", Match::No),
            ("1.2.*", "2.0-alpha", Match::No),
            (">=*", "1.20", Match::No),
            (">=*", "1.20.1", Match::No),
            (">=*", "2.0.0-alpha", Match::No),
            (">=*", "2.0-alpha", Match::No),
            ("=1.x.2", "1.x.2", Match::Yes),
            ("=1.x.2", "1.20", Match::No),
            ("=1.x.2", "1.20.1", Match::No),
            ("=1.x.2", "2.0.0-alpha", Match::No),
            ("=1.x.2", "2.0-alpha", Match::No),
            ("1.2.3+foo", "1.2.3+build", Match::Yes),
            ("1.2.3+foo", "1.20", Match::No),
            ("1.2.3+foo", "1.20.1", Match::No),
            ("1.2.3+foo", "2.0.0-alpha", Match::No),
            ("1.2.3+foo", "2.0-alpha", Match::No),
            ("=1.2.3+foo", "1.2.3+build", Match::Yes),
            ("=1.2.3+foo", "1.20", Match::No),
            ("=1.2.3+foo", "1.20.1", Match::No),
            ("=1.2.3+foo", "2.0.0-alpha", Match::No),
            ("=1.2.3+foo", "2.0-alpha", Match::No),
            ("~1.2.3-", "1.2.9", Match::Yes),
            ("~1.2.3-", "1.2.99", Match::Yes),
            ("~1.2.3-", "1.2.999", Match::Yes),
            ("~1.2.3-", "1.2.4-alpha", Match::Yes),
            ("~1.2.3-", "1.2.3-alpha", Match::Yes),
            ("~1.2.3-", "1.2.3-beta", Match::Yes),
            ("~1.2.3-", "1.20", Match::No),
            ("~1.2.3-", "1.20.1", Match::No),
            ("~1.2.3-", "2.0.0-alpha", Match::No),
            ("~1.2.3-", "2.0-alpha", Match::No),
            ("^1-", "1.20", Match::Yes),
            ("^1-", "1.20.1", Match::Yes),
            ("^1-", "1.20.6", Match::Yes),
            ("^1-", "1.20.0-alpha", Match::Yes),
            ("^1-", "1.0-alpha", Match::Yes),
            ("^1-", "1.0-", Match::Yes),
            ("^1-", "2.0.0", Match::No),
            ("^1-", "2.0", Match::No),
            ("^1-", "2.0.0-alpha", Match::No),
            ("^1-", "2.0-alpha", Match::No),
            ("~1-", "1.0", Match::Yes),
            ("~1-", "1.0.0", Match::Yes),
            ("~1-", "1.0.5", Match::Yes),
            ("~1-", "1.0-alpha", Match::Yes),
            ("~1-", "1.0-", Match::Yes),
            ("~1-", "1.20", Match::No),
            ("~1-", "1.20.1", Match::No),
            ("~1-", "2.0.0-alpha", Match::No),
            ("~1-", "2.0-alpha", Match::No),
            ("1.2.3", "1.2.3+build", Match::Yes),
            ("1.2.3", "1.20", Match::No),
            ("1.2.3", "1.20.1", Match::No),
            ("1.2.3", "2.0.0-alpha", Match::No),
            ("1.2.3", "2.0-alpha", Match::No),
            ("<1.2.3-", "1.0", Match::Yes),
            ("<1.2.3-", "1.0.0", Match::Yes),
            ("<1.2.3-", "1.0.5", Match::Yes),
            ("<1.2.3-", "1.0-alpha", Match::Yes),
            ("<1.2.3-", "1.0-", Match::Yes),
            ("<1.2.3-", "1.20", Match::No),
            ("<1.2.3-", "1.20.1", Match::No),
            ("<1.2.3-", "2.0.0-alpha", Match::No),
            ("<1.2.3-", "2.0-alpha", Match::No),
        ];
        let mut mismatches = Vec::new();
        for (range, version, expected) in FABRIC {
            let got = VersionRange::parse(*range).matches(version);
            if got != *expected {
                mismatches.push(format!(
                    "{range:?} vs {version:?}: hfc={got:?} fabric={expected:?}"
                ));
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} mismatch(es):\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }

    #[test]
    fn any_is_only_star_and_x_ranges() {
        assert!(VersionRange::parse("*").is_any());
        assert!(VersionRange::parse("").is_any());
        // A bare `x` is a non-semver *version* in Fabric (equality with "x"),
        // not a wildcard, so it is not "any".
        assert!(!VersionRange::parse("x").is_any());
        assert!(!VersionRange::parse("1.x").is_any());
    }

    #[test]
    fn unknown_ranges_are_unknown_not_guessed() {
        // A range HFC cannot parse stays Unknown; it is never treated as a
        // match. Fabric rejects `weird` and the bare `x` operand outright.
        assert_eq!(
            VersionRange::parse("weird ^ x").matches("1.0"),
            Match::Unknown
        );
        assert_eq!(
            VersionRange::parse(">20w14a").matches("1.0"),
            Match::Unknown
        );
        // A non-semver provider is *answered* by Fabric (friendly-string
        // equality), not left unknown: `>=1.20` is false for `twenty`, and the
        // resolver reports a mismatch rather than an unresolved range.
        assert_eq!(VersionRange::parse(">=1.20").matches("twenty"), Match::No);
        assert_eq!(VersionRange::parse(">=1.20").matches("1.20.1"), Match::Yes);
    }
}
