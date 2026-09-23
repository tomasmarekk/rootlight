//! First-party parsing and ordering for product release versions.
//!
//! Exact version spellings cross installation and update trust boundaries.
//! Total ordering includes build metadata for persisted sets; precedence does not.

use std::{cmp::Ordering, fmt, str::FromStr};

/// A complete Semantic Versioning 2.0.0 version.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    /// Numeric major component.
    pub major: u64,
    /// Numeric minor component.
    pub minor: u64,
    /// Numeric patch component.
    pub patch: u64,
    /// Optional pre-release identifiers.
    pub pre: Prerelease,
    /// Optional build metadata identifiers.
    pub build: BuildMetadata,
}

/// Validated pre-release identifiers, or an empty stable-release suffix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Prerelease(String);

/// Validated build metadata identifiers, or an empty suffix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuildMetadata(String);

/// The portion of a version whose syntax could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionError {
    /// The required major.minor.patch core is invalid or exceeds `u64`.
    Core,
    /// A pre-release identifier is empty, malformed, or has a numeric leading zero.
    Prerelease,
    /// A build metadata identifier is empty or malformed.
    BuildMetadata,
}

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Core => "invalid semantic version core",
            Self::Prerelease => "invalid semantic version prerelease",
            Self::BuildMetadata => "invalid semantic version build metadata",
        };
        f.write_str(message)
    }
}

impl std::error::Error for VersionError {}

impl Version {
    /// Creates a stable version with the given numeric components.
    #[must_use]
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            pre: Prerelease::EMPTY,
            build: BuildMetadata::EMPTY,
        }
    }

    /// Parses a complete SemVer version without trimming or normalizing input.
    ///
    /// # Errors
    ///
    /// Returns [`VersionError`] for malformed or overflowing components.
    pub fn parse(value: &str) -> Result<Self, VersionError> {
        let (core_and_pre, build) = match value.split_once('+') {
            Some((before, after)) if !after.is_empty() => (before, BuildMetadata::new(after)?),
            Some(_) => return Err(VersionError::BuildMetadata),
            None => (value, BuildMetadata::EMPTY),
        };
        let (core, pre) = match core_and_pre.split_once('-') {
            Some((before, after)) if !after.is_empty() => (before, Prerelease::new(after)?),
            Some(_) => return Err(VersionError::Prerelease),
            None => (core_and_pre, Prerelease::EMPTY),
        };
        let mut parts = core.split('.');
        let major = parse_core_part(parts.next().ok_or(VersionError::Core)?)?;
        let minor = parse_core_part(parts.next().ok_or(VersionError::Core)?)?;
        let patch = parse_core_part(parts.next().ok_or(VersionError::Core)?)?;
        if parts.next().is_some() {
            return Err(VersionError::Core);
        }
        Ok(Self {
            major,
            minor,
            patch,
            pre,
            build,
        })
    }

    /// Compares SemVer precedence without build metadata.
    #[must_use]
    pub fn cmp_precedence(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch, &self.pre).cmp(&(
            other.major,
            other.minor,
            other.patch,
            &other.pre,
        ))
    }
}

impl FromStr for Version {
    type Err = VersionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.as_str())?;
        }
        if !self.build.is_empty() {
            write!(f, "+{}", self.build.as_str())?;
        }
        Ok(())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch, &self.pre, &self.build).cmp(&(
            other.major,
            other.minor,
            other.patch,
            &other.pre,
            &other.build,
        ))
    }
}

impl Prerelease {
    /// Empty suffix for a stable release.
    pub const EMPTY: Self = Self(String::new());

    /// Validates dot-separated pre-release identifiers.
    ///
    /// # Errors
    ///
    /// Returns [`VersionError::Prerelease`] for invalid syntax.
    pub fn new(value: &str) -> Result<Self, VersionError> {
        validate_identifiers(value, false).ok_or(VersionError::Prerelease)?;
        Ok(Self(value.to_owned()))
    }

    /// Returns whether the suffix is absent.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the validated authored suffix.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialOrd for Prerelease {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Prerelease {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.is_empty(), other.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => compare_identifiers(&self.0, &other.0, false),
        }
    }
}

impl BuildMetadata {
    /// Empty suffix for a version without build metadata.
    pub const EMPTY: Self = Self(String::new());

    /// Validates dot-separated build metadata identifiers.
    ///
    /// # Errors
    ///
    /// Returns [`VersionError::BuildMetadata`] for invalid syntax.
    pub fn new(value: &str) -> Result<Self, VersionError> {
        validate_identifiers(value, true).ok_or(VersionError::BuildMetadata)?;
        Ok(Self(value.to_owned()))
    }

    /// Returns whether the suffix is absent.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the validated authored suffix.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialOrd for BuildMetadata {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BuildMetadata {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_identifiers(&self.0, &other.0, true)
    }
}

fn parse_core_part(value: &str) -> Result<u64, VersionError> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(VersionError::Core);
    }
    value.parse().map_err(|_| VersionError::Core)
}

fn validate_identifiers(value: &str, allow_numeric_leading_zeroes: bool) -> Option<()> {
    if value.is_empty() {
        return Some(());
    }
    for identifier in value.split('.') {
        if identifier.is_empty()
            || !identifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || (!allow_numeric_leading_zeroes
                && identifier.len() > 1
                && identifier.starts_with('0')
                && identifier.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return None;
        }
    }
    Some(())
}

fn compare_identifiers(left: &str, right: &str, build: bool) -> Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        match (left.next(), right.next()) {
            (Some(a), Some(b)) => {
                let numeric_a = a.bytes().all(|byte| byte.is_ascii_digit());
                let numeric_b = b.bytes().all(|byte| byte.is_ascii_digit());
                let order = match (numeric_a, numeric_b) {
                    (true, true) if build => {
                        let value_a = a.trim_start_matches('0');
                        let value_b = b.trim_start_matches('0');
                        value_a
                            .len()
                            .cmp(&value_b.len())
                            .then_with(|| value_a.cmp(value_b))
                            .then_with(|| a.len().cmp(&b.len()))
                    }
                    (true, true) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (false, false) => a.cmp(b),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_core_prerelease_and_build_spelling_round_trip() {
        for value in [
            "0.0.0",
            "1.2.3",
            "18446744073709551615.0.0",
            "1.2.3-alpha.1",
            "1.2.3-x-y-z.--+001.abc-DEF",
            "1.2.3+0001",
        ] {
            assert_eq!(Version::parse(value).unwrap().to_string(), value);
        }
    }

    #[test]
    fn invalid_numeric_and_identifier_syntax_is_rejected() {
        for value in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.-3",
            "18446744073709551616.0.0",
            "1.2.3-",
            "1.2.3-01",
            "1.2.3-a..b",
            "1.2.3+",
            "1.2.3+a..b",
            "1.2.3-a+b+c",
            "1.2.3_a",
            "1.2.3+é",
        ] {
            assert!(Version::parse(value).is_err(), "{value}");
        }
    }

    #[test]
    fn precedence_ignores_build_but_total_order_keeps_it_distinct() {
        let one = Version::parse("1.0.0+001").unwrap();
        let two = Version::parse("1.0.0+1").unwrap();
        assert_eq!(one.cmp_precedence(&two), Ordering::Equal);
        assert!(one != two);
        assert!(one > two);
        let ordered = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for pair in ordered.windows(2) {
            assert!(Version::parse(pair[0]).unwrap() < Version::parse(pair[1]).unwrap());
        }
    }

    #[test]
    fn suffixes_preserve_numeric_and_text_identifier_order() {
        for pair in [
            ("1.0.0-9", "1.0.0-10"),
            ("1.0.0-18446744073709551616", "1.0.0-18446744073709551617"),
            ("1.0.0-1", "1.0.0-a"),
            ("1.0.0-a", "1.0.0-a.1"),
            ("1.0.0+0", "1.0.0+00"),
            ("1.0.0+00", "1.0.0+1"),
        ] {
            assert!(Version::parse(pair.0).unwrap() < Version::parse(pair.1).unwrap());
        }
        assert_eq!(Prerelease::new(""), Ok(Prerelease::EMPTY));
        assert_eq!(BuildMetadata::new(""), Ok(BuildMetadata::EMPTY));
        assert!(Prerelease::new("01").is_err());
        assert!(BuildMetadata::new("01").is_ok());
        assert!(Prerelease::new("a..b").is_err());
        assert!(BuildMetadata::new("a..b").is_err());
    }
}
