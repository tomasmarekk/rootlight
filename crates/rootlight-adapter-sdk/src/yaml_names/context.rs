//! Document-local tag resolution for grammar-reviewed YAML scalars.
//! Unknown application tags retain exact evidence, never invented constructors
//! or an assertion of semantic key equivalence.

use std::collections::HashMap;

use super::{YamlBlockScalar, append, append_quoted, canonical_value, decode_flow, numbers};

const CORE: &str = "tag:yaml.org,2002:";

/// The two collection kinds in the YAML representation graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum YamlCollectionKind {
    /// An unordered set of unique key/value pairs.
    Mapping,
    /// An ordered sequence of nodes.
    Sequence,
}

/// A document-resolved collection tag, with explicit application-schema opacity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct YamlCollectionTag {
    name: String,
    unrecognized: bool,
}

impl YamlCollectionTag {
    /// Returns the exact resolved tag, without URI decoding or normalization.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the tag's application-specific collection semantics are unknown.
    #[must_use]
    pub const fn has_unrecognized_tag(&self) -> bool {
        self.unrecognized
    }
}

/// Bounded YAML 1.2 Core interpretation and tag directives for one document.
///
/// Construct a fresh context for every document, including documents without
/// directives. Handles never inherit from an earlier document in a stream.
/// This is not a document parser, alias resolver or application-schema loader.
#[derive(Debug)]
pub struct YamlDocumentContext<'a> {
    handles: HashMap<&'a str, &'a str>,
    maximum_bytes: usize,
    version_warning: bool,
}

/// A type-preserving scalar identity, or explicitly opaque tagged evidence.
///
/// Identities are internal names, not YAML source. An unrecognized tag's name
/// combines the exact resolved tag with decoded string content; it does not
/// prove semantic equality or inequality under an unknown application schema.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct YamlScalarIdentity {
    name: String,
    unrecognized_tag: bool,
}

impl YamlScalarIdentity {
    /// Returns the bounded internal name without replacing its source evidence.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Consumes this identity and returns its internal name without copying.
    #[must_use]
    pub fn into_name(self) -> String {
        self.name
    }

    /// Whether callers must report unknown tag semantics instead of full analysis.
    #[must_use]
    pub const fn has_unrecognized_tag(&self) -> bool {
        self.unrecognized_tag
    }
}

impl<'a> YamlDocumentContext<'a> {
    /// Validates one document's version and raw `(handle, prefix)` directives.
    ///
    /// `version` omits `%YAML`; directive pairs omit `%TAG` and whitespace.
    /// The caller must reject duplicate version directives and misplaced
    /// directives. Absent or 1.2 versions use Core 1.2; 1.1 and higher 1.x
    /// versions use the same rules with [`Self::has_version_warning`] set.
    /// Version 1.0, other majors, malformed/duplicate handles, invalid prefix
    /// spelling, excess cumulative version/directive bytes or allocation failure
    /// return `None`. `maximum_bytes` also bounds each scalar's source, resolved
    /// tag and resulting name. No application code or URI is loaded.
    ///
    /// # Examples
    ///
    /// ```
    /// use rootlight_adapter_sdk::YamlDocumentContext;
    /// # fn example() -> Option<()> {
    /// let document = YamlDocumentContext::new(Some("1.2"), &[], 256)?;
    /// let scalar = document.flow_scalar("'12'", Some("!!int"))?;
    /// assert_eq!(scalar.name(), "int:12");
    /// assert!(!scalar.has_unrecognized_tag());
    /// # Some(())
    /// # }
    /// # assert!(example().is_some());
    /// ```
    #[must_use]
    pub fn new(
        version: Option<&str>,
        directives: &[(&'a str, &'a str)],
        maximum_bytes: usize,
    ) -> Option<Self> {
        let mut bytes = version.map_or(0, str::len);
        if bytes > maximum_bytes {
            return None;
        }
        let version_warning = match version {
            Some(version) => version_policy(version)?,
            None => false,
        };
        for (handle, prefix) in directives {
            bytes = bytes.checked_add(handle.len())?.checked_add(prefix.len())?;
            if bytes > maximum_bytes || !valid_handle(handle) || !valid_prefix(prefix) {
                return None;
            }
        }
        let mut handles = HashMap::new();
        handles.try_reserve(directives.len()).ok()?;
        for &(handle, prefix) in directives {
            if handles.insert(handle, prefix).is_some() {
                return None;
            }
        }
        Some(Self {
            handles,
            maximum_bytes,
            version_warning,
        })
    }

    /// Whether the document requests a version processed with compatibility risk.
    ///
    /// Callers must preserve this scoped warning. It does not activate legacy
    /// 1.1 implicit types such as `yes`, timestamps or sexagesimal numbers.
    #[must_use]
    pub const fn has_version_warning(&self) -> bool {
        self.version_warning
    }

    /// Resolves a collection tag without interpreting application-specific types.
    ///
    /// Absent and non-specific `!` tags select the matching Core collection tag.
    /// Known scalar tags and a mismatched collection kind return `None`, as do
    /// malformed tags, unknown handles, allocation failure or excess tag bytes.
    /// Unrecognized tags preserve exact identity and require a scoped warning.
    #[must_use]
    pub fn collection_tag(
        &self,
        kind: YamlCollectionKind,
        tag: Option<&str>,
    ) -> Option<YamlCollectionTag> {
        let default = match kind {
            YamlCollectionKind::Mapping => "tag:yaml.org,2002:map",
            YamlCollectionKind::Sequence => "tag:yaml.org,2002:seq",
        };
        let name = match tag {
            None | Some("!") => {
                let mut name = String::new();
                append(&mut name, default, self.maximum_bytes)?;
                name
            }
            Some(tag) if tag.len() <= self.maximum_bytes => self.resolve_tag(tag)?,
            Some(_) => return None,
        };
        let unrecognized = name != default;
        if unrecognized
            && matches!(
                name.strip_prefix(CORE),
                Some("map" | "seq" | "str" | "int" | "float" | "bool" | "null")
            )
        {
            return None;
        }
        Some(YamlCollectionTag { name, unrecognized })
    }

    /// Interprets one grammar-reviewed flow scalar and its optional raw tag.
    ///
    /// `text` excludes tag/anchor properties and includes its original quotes.
    /// Empty text is valid only when the caller has identified an empty scalar
    /// node; it resolves to null unless a tag dictates otherwise. Explicit Core
    /// tags override quoted style. Unknown tags remain opaque evidence and must
    /// be reported via [`YamlScalarIdentity::has_unrecognized_tag`]. Preserve
    /// scalar, tag and directive source spans separately from the resulting name.
    ///
    /// Block scalars, aliases and collection keys are not accepted. Native block
    /// node ranges may omit trailing lines required for chomping: they must not
    /// be passed as flow text. Invalid spelling, unresolved handles, Core type
    /// mismatch, excess combined text/tag bytes or allocation failure return
    /// `None`. Tag percent escapes stay exact, without URI decoding or case folding.
    #[must_use]
    pub fn flow_scalar(&self, text: &str, tag: Option<&str>) -> Option<YamlScalarIdentity> {
        if text.len().checked_add(tag.map_or(0, str::len))? > self.maximum_bytes {
            return None;
        }
        let (value, plain) = decode_flow(text, self.maximum_bytes)?;
        self.scalar_value(&value, plain, tag)
    }

    /// Assigns a type-preserving identity to a source-complete block scalar.
    ///
    /// Untagged block values are strings, never implicitly Core numbers or null.
    /// Explicit tags use the same document context as [`Self::flow_scalar`].
    /// Preserve [`YamlBlockScalar::lexical_range`] as value evidence in addition
    /// to the original parser capture. Invalid tags, type mismatches, excess
    /// combined lexical/tag bytes, expanded names or allocation failure return
    /// `None`; unknown tags retain the explicit semantic-opacity flag.
    #[must_use]
    pub fn block_scalar(
        &self,
        block: &YamlBlockScalar<'_>,
        tag: Option<&str>,
    ) -> Option<YamlScalarIdentity> {
        if block
            .source_text()
            .len()
            .checked_add(tag.map_or(0, str::len))?
            > self.maximum_bytes
        {
            return None;
        }
        self.scalar_value(block.value(), false, tag)
    }

    fn scalar_value(
        &self,
        value: &str,
        plain: bool,
        tag: Option<&str>,
    ) -> Option<YamlScalarIdentity> {
        let (name, unrecognized_tag) = match tag {
            None => (canonical_value(value, plain, self.maximum_bytes)?, false),
            Some("!") => (canonical_value(value, false, self.maximum_bytes)?, false),
            Some(tag) => {
                let resolved = self.resolve_tag(tag)?;
                self.tagged_value(value, &resolved)?
            }
        };
        Some(YamlScalarIdentity {
            name,
            unrecognized_tag,
        })
    }

    fn resolve_tag(&self, tag: &str) -> Option<String> {
        let mut result = String::new();
        if let Some(verbatim) = tag.strip_prefix("!<") {
            append(&mut result, verbatim.strip_suffix('>')?, self.maximum_bytes)?;
        } else {
            let rest = tag.strip_prefix('!')?;
            let (handle, suffix) = if let Some(end) = rest.find('!') {
                let split = end.checked_add(2)?;
                (tag.get(..split)?, tag.get(split..)?)
            } else {
                ("!", rest)
            };
            if !valid_handle(handle) || !uri_characters(suffix, true) {
                return None;
            }
            let prefix = self.handles.get(handle).copied().or(match handle {
                "!" => Some("!"),
                "!!" => Some(CORE),
                _ => None,
            })?;
            append(&mut result, prefix, self.maximum_bytes)?;
            append(&mut result, suffix, self.maximum_bytes)?;
        }
        // YAML 1.2.2 section 5.6 requires exact tag comparison, including `%HH`.
        // URI decoding would merge application tags that the source distinguishes.
        valid_tag(&result).then_some(result)
    }

    fn tagged_value(&self, value: &str, tag: &str) -> Option<(String, bool)> {
        let maximum = self.maximum_bytes;
        let name = match tag.strip_prefix(CORE) {
            Some("str") => canonical_value(value, false, maximum)?,
            Some("null") if matches!(value, "" | "~" | "null" | "Null" | "NULL") => {
                canonical_value(value, true, maximum)?
            }
            Some("bool")
                if matches!(
                    value,
                    "true" | "True" | "TRUE" | "false" | "False" | "FALSE"
                ) =>
            {
                canonical_value(value, true, maximum)?
            }
            Some("int") => {
                let number @ numbers::Number::Integer { .. } = numbers::classify(value)? else {
                    return None;
                };
                number.canonical(maximum)?
            }
            Some("float") => {
                let number = match numbers::classify(value)? {
                    numbers::Number::Integer {
                        digits,
                        radix: 10,
                        negative,
                    } => numbers::Number::Decimal {
                        coefficient: digits,
                        exponent: "0",
                        negative,
                    },
                    numbers::Number::Integer { .. } => return None,
                    number @ (numbers::Number::Decimal { .. } | numbers::Number::Special(_)) => {
                        number
                    }
                };
                number.canonical(maximum)?
            }
            Some("null" | "bool" | "map" | "seq") => return None,
            _ => {
                let mut name = String::new();
                append(&mut name, "tag:", maximum)?;
                append_quoted(&mut name, tag, maximum)?;
                append(&mut name, ":", maximum)?;
                append_quoted(&mut name, value, maximum)?;
                return Some((name, true));
            }
        };
        Some((name, false))
    }
}

fn version_policy(version: &str) -> Option<bool> {
    let (major, minor) = version.split_once('.')?;
    if major.is_empty()
        || minor.is_empty()
        || !major
            .bytes()
            .chain(minor.bytes())
            .all(|b| b.is_ascii_digit())
        || major.trim_start_matches('0') != "1"
    {
        return None;
    }
    let minor = minor.trim_start_matches('0');
    (!minor.is_empty()).then_some(minor != "2")
}

fn valid_handle(handle: &str) -> bool {
    matches!(handle, "!" | "!!")
        || handle
            .strip_prefix('!')
            .and_then(|rest| rest.strip_suffix('!'))
            .is_some_and(|name| {
                !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
}

fn valid_prefix(prefix: &str) -> bool {
    uri_characters(prefix, false)
        && prefix
            .as_bytes()
            .first()
            .is_some_and(|b| !matches!(b, b'[' | b']' | b','))
}

fn valid_tag(tag: &str) -> bool {
    if !uri_characters(tag, false) || tag == "!" {
        return false;
    }
    if tag.starts_with('!') {
        return true;
    }
    tag.split_once(':').is_some_and(|(scheme, _)| {
        scheme
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
            && scheme
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    })
}

fn uri_characters(text: &str, suffix: bool) -> bool {
    if text.is_empty() {
        return false;
    }
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            if !bytes.next().is_some_and(|b| b.is_ascii_hexdigit())
                || !bytes.next().is_some_and(|b| b.is_ascii_hexdigit())
            {
                return false;
            }
        } else if !(byte.is_ascii_alphanumeric() || b"-#;/?:@&=+$,_.!~*'()[]".contains(&byte))
            || (suffix && b"![],".contains(&byte))
        {
            return false;
        }
    }
    true
}
