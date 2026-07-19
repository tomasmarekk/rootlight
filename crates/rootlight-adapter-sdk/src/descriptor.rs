//! Parser and producer capability descriptors.
//!
//! Descriptors are bounded, deterministic, and independent of parser runtimes
//! so scheduling code can reject unsupported work before reading source.

use std::fmt;

use rootlight_ir::{AnalysisTier, ProducerIdentity, ProducerKind};

use crate::error::{DescriptorError, LabelError, LabelField, LabelViolation};

const MAX_CAPABILITY_ITEMS: usize = 128;
const MAX_LANGUAGE_BYTES: usize = 64;
const MAX_ENCODING_BYTES: usize = 32;
const UTF8_ENCODING: &str = "utf-8";

/// How an adapter enforces its advertised memory ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MemoryEnforcement {
    /// The adapter runs behind a hard operating-system process boundary.
    ///
    /// Process-tree ownership and hostile-provider termination are supplied by
    /// the isolated adapter host, not by this synchronous in-process SDK.
    HardProcess,
    /// The cooperative in-process adapter reports its own memory accounting.
    ///
    /// This post-hoc counter is useful for bounded trusted adapters, but it is
    /// not proof against a malicious or noncooperative provider.
    AccountedInProcess,
    /// The adapter cannot provide enforceable or accountable memory usage.
    Unavailable,
}

/// Caller-selected admission policy for adapter memory enforcement.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MemoryAdmissionPolicy {
    /// Reject adapters without hard or reported in-process enforcement.
    #[default]
    RequireHardOrAccounted,
    /// Intentionally admit the bounded fallback without memory enforcement.
    AllowUnavailableEnforcementFallback,
}

/// Memory-bound status attached by the SDK to committed adapter output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MemoryAdmissionStatus {
    /// The provider is owned by a hard process boundary.
    HardProcess,
    /// The trusted in-process provider supplied its required reported counter.
    AccountedInProcess,
    /// The caller explicitly admitted the unavailable-enforcement fallback.
    UnavailableEnforcementFallback,
}

/// A bounded normalized language identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LanguageId(String);

impl LanguageId {
    /// Creates a source-free language label.
    ///
    /// # Errors
    ///
    /// Returns [`LabelError`] for empty, oversized, whitespace-containing, or
    /// path-shaped input.
    pub fn new(value: &str) -> Result<Self, LabelError> {
        validated_label(LabelField::Language, value, MAX_LANGUAGE_BYTES).map(Self)
    }

    /// Returns the validated language label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LanguageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A bounded normalized source-encoding identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EncodingId(String);

impl EncodingId {
    /// Creates the canonical UTF-8 encoding identity.
    #[must_use]
    pub fn utf8() -> Self {
        Self(UTF8_ENCODING.to_owned())
    }

    /// Creates a source-free encoding label.
    ///
    /// # Errors
    ///
    /// Returns [`LabelError`] for empty, oversized, whitespace-containing, or
    /// path-shaped input.
    pub fn new(value: &str) -> Result<Self, LabelError> {
        validated_label(LabelField::Encoding, value, MAX_ENCODING_BYTES).map(Self)
    }

    /// Returns the validated encoding label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EncodingId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Parser-provider capabilities used for deterministic admission control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseCapabilities {
    languages: Vec<LanguageId>,
    encodings: Vec<EncodingId>,
    max_source_bytes: usize,
    max_syntax_nodes: usize,
    max_syntax_depth: usize,
    max_embedded_ranges: usize,
    embedded_ranges: bool,
    error_recovery: bool,
    cancellation_checkpoints: bool,
    max_concurrent_parses: usize,
    memory_enforcement: MemoryEnforcement,
}

impl ParseCapabilities {
    /// Creates a checked parser capability descriptor.
    ///
    /// Language and encoding sets are sorted and deduplicated so capability
    /// identity does not depend on registration order.
    ///
    /// # Errors
    ///
    /// Returns [`DescriptorError`] for empty or oversized sets and zero maxima.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mut languages: Vec<LanguageId>,
        mut encodings: Vec<EncodingId>,
        max_source_bytes: usize,
        max_syntax_nodes: usize,
        max_syntax_depth: usize,
        max_embedded_ranges: usize,
        embedded_ranges: bool,
        error_recovery: bool,
        cancellation_checkpoints: bool,
        max_concurrent_parses: usize,
        memory_enforcement: MemoryEnforcement,
    ) -> Result<Self, DescriptorError> {
        canonicalize_capability_set("languages", &mut languages)?;
        canonicalize_capability_set("encodings", &mut encodings)?;
        require_nonzero("max_source_bytes", max_source_bytes)?;
        require_nonzero("max_syntax_nodes", max_syntax_nodes)?;
        require_nonzero("max_syntax_depth", max_syntax_depth)?;
        if embedded_ranges {
            require_nonzero("max_embedded_ranges", max_embedded_ranges)?;
        }
        require_nonzero("max_concurrent_parses", max_concurrent_parses)?;
        Ok(Self {
            languages,
            encodings,
            max_source_bytes,
            max_syntax_nodes,
            max_syntax_depth,
            max_embedded_ranges,
            embedded_ranges,
            error_recovery,
            cancellation_checkpoints,
            max_concurrent_parses,
            memory_enforcement,
        })
    }

    /// Returns the sorted supported language set.
    #[must_use]
    pub fn languages(&self) -> &[LanguageId] {
        &self.languages
    }

    /// Returns the sorted supported source encodings.
    #[must_use]
    pub fn encodings(&self) -> &[EncodingId] {
        &self.encodings
    }

    /// Returns the largest file this provider admits.
    #[must_use]
    pub const fn max_source_bytes(&self) -> usize {
        self.max_source_bytes
    }

    /// Returns the largest concrete-syntax node count this provider admits.
    #[must_use]
    pub const fn max_syntax_nodes(&self) -> usize {
        self.max_syntax_nodes
    }

    /// Returns the largest syntax nesting depth this provider admits.
    #[must_use]
    pub const fn max_syntax_depth(&self) -> usize {
        self.max_syntax_depth
    }

    /// Returns the largest embedded-range count this provider admits.
    #[must_use]
    pub const fn max_embedded_ranges(&self) -> usize {
        self.max_embedded_ranges
    }

    /// Reports whether the provider accepts embedded included ranges.
    #[must_use]
    pub const fn supports_embedded_ranges(&self) -> bool {
        self.embedded_ranges
    }

    /// Reports whether the provider emits bounded recovery facts.
    #[must_use]
    pub const fn supports_error_recovery(&self) -> bool {
        self.error_recovery
    }

    /// Reports whether the provider checks cancellation during parsing.
    #[must_use]
    pub const fn has_cancellation_checkpoints(&self) -> bool {
        self.cancellation_checkpoints
    }

    /// Returns the provider's bounded concurrent parse capacity.
    #[must_use]
    pub const fn max_concurrent_parses(&self) -> usize {
        self.max_concurrent_parses
    }

    /// Returns the provider's memory-bound enforcement class.
    #[must_use]
    pub const fn memory_enforcement(&self) -> MemoryEnforcement {
        self.memory_enforcement
    }
}

/// Identity and capability metadata for one normalized-IR producer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerDescriptor {
    identity: ProducerIdentity,
    kind: ProducerKind,
    language: LanguageId,
    tier: AnalysisTier,
    memory_enforcement: MemoryEnforcement,
    supports_noncritical_extensions: bool,
}

impl ProducerDescriptor {
    /// Creates a parser-independent normalized-IR producer descriptor.
    #[must_use]
    pub const fn new(
        identity: ProducerIdentity,
        kind: ProducerKind,
        language: LanguageId,
        tier: AnalysisTier,
        memory_enforcement: MemoryEnforcement,
        supports_noncritical_extensions: bool,
    ) -> Self {
        Self {
            identity,
            kind,
            language,
            tier,
            memory_enforcement,
            supports_noncritical_extensions,
        }
    }

    /// Returns the stable producer identity.
    #[must_use]
    pub const fn identity(&self) -> &ProducerIdentity {
        &self.identity
    }

    /// Returns the normalized producer class.
    #[must_use]
    pub const fn kind(&self) -> ProducerKind {
        self.kind
    }

    /// Returns the language supplied by the producer.
    #[must_use]
    pub const fn language(&self) -> &LanguageId {
        &self.language
    }

    /// Returns the highest analysis tier supplied by the producer.
    #[must_use]
    pub const fn tier(&self) -> AnalysisTier {
        self.tier
    }

    /// Returns the producer's memory-bound enforcement class.
    #[must_use]
    pub const fn memory_enforcement(&self) -> MemoryEnforcement {
        self.memory_enforcement
    }

    /// Reports generic support for noncritical extension envelopes.
    #[must_use]
    pub const fn supports_noncritical_extensions(&self) -> bool {
        self.supports_noncritical_extensions
    }
}

pub(crate) fn validated_label(
    field: LabelField,
    value: &str,
    maximum_bytes: usize,
) -> Result<String, LabelError> {
    let violation = if value.is_empty() {
        Some(LabelViolation::Empty)
    } else if value.len() > maximum_bytes {
        Some(LabelViolation::TooLong)
    } else if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+' | b'#')
    }) {
        Some(LabelViolation::InvalidByte)
    } else {
        None
    };
    match violation {
        Some(violation) => Err(LabelError { field, violation }),
        None => Ok(value.to_owned()),
    }
}

fn canonicalize_capability_set<T: Ord>(
    collection: &'static str,
    values: &mut Vec<T>,
) -> Result<(), DescriptorError> {
    if values.is_empty() {
        return Err(DescriptorError::EmptyCollection { collection });
    }
    if values.len() > MAX_CAPABILITY_ITEMS {
        return Err(DescriptorError::TooManyItems {
            collection,
            observed: values.len(),
            limit: MAX_CAPABILITY_ITEMS,
        });
    }
    values.sort_unstable();
    values.dedup();
    Ok(())
}

fn require_nonzero(field: &'static str, value: usize) -> Result<(), DescriptorError> {
    if value == 0 {
        Err(DescriptorError::ZeroMaximum { field })
    } else {
        Ok(())
    }
}
