//! Explicit resource and parser-option configuration for the native runtime.
//!
//! No product defaults are supplied here. Callers select every capacity and
//! the resulting values become parser admission and incremental-reuse identity.

use rootlight_adapter_sdk::{DescriptorError, LabelError};

use crate::RegistryError;

const HARD_MAX_SOURCE_BYTES: usize = 256 * 1024 * 1024;
const HARD_MAX_SYNTAX_NODES: usize = 16 * 1024 * 1024;
const HARD_MAX_SYNTAX_DEPTH: usize = 4096;
const HARD_MAX_INCLUDED_RANGES: usize = 65_536;
const HARD_MAX_INCREMENTAL_EDITS: usize = 65_536;
const HARD_MAX_CONCURRENT_PARSES: usize = 256;
const HARD_MAX_CACHE_BYTES: usize = u32::MAX as usize;
const HARD_MAX_INPUT_CHUNK_BYTES: usize = 16 * 1024 * 1024;

/// Parser input scheduling options included in incremental reuse identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ParserSettings {
    input_chunk_bytes: usize,
}

impl ParserSettings {
    /// Creates parser settings with a nonzero input callback chunk size.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeConfigError::Zero`] when `input_chunk_bytes` is zero.
    pub fn new(input_chunk_bytes: usize) -> Result<Self, RuntimeConfigError> {
        require_nonzero("input_chunk_bytes", input_chunk_bytes)?;
        require_hard_maximum(
            "input_chunk_bytes",
            input_chunk_bytes,
            HARD_MAX_INPUT_CHUNK_BYTES,
        )?;
        Ok(Self { input_chunk_bytes })
    }

    /// Returns the maximum bytes supplied by one Tree-sitter input callback.
    #[must_use]
    pub const fn input_chunk_bytes(self) -> usize {
        self.input_chunk_bytes
    }
}

/// Explicit capacities for one bounded in-process Tree-sitter provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    max_source_bytes: usize,
    max_syntax_nodes: usize,
    max_syntax_depth: usize,
    max_included_ranges: usize,
    max_incremental_edits: usize,
    max_concurrent_parses: usize,
    max_cache_bytes: usize,
    default_settings: ParserSettings,
}

impl RuntimeConfig {
    /// Creates a checked runtime configuration without policy defaults.
    ///
    /// The cache ceiling accounts retained source bytes and a conservative
    /// logical tree weight. Tree-sitter's safe API cannot hard-cap its native
    /// allocator, so the provider still advertises unavailable memory
    /// enforcement until M13 process isolation.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeConfigError`] for zero capacities, including the edit
    /// ceiling, source extents above Tree-sitter's 32-bit offset domain, or an
    /// input chunk larger than a file.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        max_source_bytes: usize,
        max_syntax_nodes: usize,
        max_syntax_depth: usize,
        max_included_ranges: usize,
        max_incremental_edits: usize,
        max_concurrent_parses: usize,
        max_cache_bytes: usize,
        default_settings: ParserSettings,
    ) -> Result<Self, RuntimeConfigError> {
        for (field, value) in [
            ("max_source_bytes", max_source_bytes),
            ("max_syntax_nodes", max_syntax_nodes),
            ("max_syntax_depth", max_syntax_depth),
            ("max_included_ranges", max_included_ranges),
            ("max_incremental_edits", max_incremental_edits),
            ("max_concurrent_parses", max_concurrent_parses),
            ("max_cache_bytes", max_cache_bytes),
        ] {
            require_nonzero(field, value)?;
        }
        if max_source_bytes > u32::MAX as usize {
            return Err(RuntimeConfigError::SourceOffsetTooLarge {
                observed: max_source_bytes,
                maximum: u32::MAX as usize,
            });
        }
        for (field, value, maximum) in [
            ("max_source_bytes", max_source_bytes, HARD_MAX_SOURCE_BYTES),
            ("max_syntax_nodes", max_syntax_nodes, HARD_MAX_SYNTAX_NODES),
            ("max_syntax_depth", max_syntax_depth, HARD_MAX_SYNTAX_DEPTH),
            (
                "max_included_ranges",
                max_included_ranges,
                HARD_MAX_INCLUDED_RANGES,
            ),
            (
                "max_incremental_edits",
                max_incremental_edits,
                HARD_MAX_INCREMENTAL_EDITS,
            ),
            (
                "max_concurrent_parses",
                max_concurrent_parses,
                HARD_MAX_CONCURRENT_PARSES,
            ),
            ("max_cache_bytes", max_cache_bytes, HARD_MAX_CACHE_BYTES),
        ] {
            require_hard_maximum(field, value, maximum)?;
        }
        if default_settings.input_chunk_bytes > max_source_bytes {
            return Err(RuntimeConfigError::InputChunkTooLarge {
                observed: default_settings.input_chunk_bytes,
                maximum: max_source_bytes,
            });
        }
        Ok(Self {
            max_source_bytes,
            max_syntax_nodes,
            max_syntax_depth,
            max_included_ranges,
            max_incremental_edits,
            max_concurrent_parses,
            max_cache_bytes,
            default_settings,
        })
    }

    /// Returns the admitted source byte ceiling.
    #[must_use]
    pub const fn max_source_bytes(&self) -> usize {
        self.max_source_bytes
    }

    /// Returns the processed syntax node ceiling.
    #[must_use]
    pub const fn max_syntax_nodes(&self) -> usize {
        self.max_syntax_nodes
    }

    /// Returns the processed syntax depth ceiling.
    #[must_use]
    pub const fn max_syntax_depth(&self) -> usize {
        self.max_syntax_depth
    }

    /// Returns the included-range ceiling.
    #[must_use]
    pub const fn max_included_ranges(&self) -> usize {
        self.max_included_ranges
    }

    /// Returns the sequential incremental edit ceiling.
    #[must_use]
    pub const fn max_incremental_edits(&self) -> usize {
        self.max_incremental_edits
    }

    /// Returns the parser permit count.
    #[must_use]
    pub const fn max_concurrent_parses(&self) -> usize {
        self.max_concurrent_parses
    }

    /// Returns the retained incremental-cache byte ceiling.
    #[must_use]
    pub const fn max_cache_bytes(&self) -> usize {
        self.max_cache_bytes
    }

    /// Returns the settings used by the [`rootlight_adapter_sdk::ParseProvider`] path.
    #[must_use]
    pub const fn default_settings(&self) -> ParserSettings {
        self.default_settings
    }
}

/// Invalid explicit Tree-sitter runtime configuration.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RuntimeConfigError {
    /// A required capacity was zero.
    #[error("{field} must be nonzero")]
    Zero {
        /// Invalid field.
        field: &'static str,
    },
    /// A configured source extent exceeded Tree-sitter's byte-offset domain.
    #[error("source limit {observed} exceeds Tree-sitter offset maximum {maximum}")]
    SourceOffsetTooLarge {
        /// Configured source bytes.
        observed: usize,
        /// Native runtime maximum.
        maximum: usize,
    },
    /// An input callback chunk exceeded the admitted source ceiling.
    #[error("input chunk {observed} exceeds source limit {maximum}")]
    InputChunkTooLarge {
        /// Configured chunk bytes.
        observed: usize,
        /// Configured source bytes.
        maximum: usize,
    },
    /// Grammar registry initialization failed.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// SDK capability construction failed.
    #[error(transparent)]
    Descriptor(#[from] DescriptorError),
    /// A built-in SDK label failed validation.
    #[error(transparent)]
    Label(#[from] LabelError),
    /// The process-local provider identity space was exhausted.
    #[error("Tree-sitter provider identity space is exhausted")]
    ProviderIdentityExhausted,
    /// A capacity exceeded the audited in-process hard maximum.
    #[error("{field} value {observed} exceeds hard maximum {maximum}")]
    AboveHardMaximum {
        /// Invalid capacity field.
        field: &'static str,
        /// Requested capacity.
        observed: usize,
        /// Audited hard maximum.
        maximum: usize,
    },
}

fn require_nonzero(field: &'static str, value: usize) -> Result<(), RuntimeConfigError> {
    if value == 0 {
        Err(RuntimeConfigError::Zero { field })
    } else {
        Ok(())
    }
}

fn require_hard_maximum(
    field: &'static str,
    value: usize,
    maximum: usize,
) -> Result<(), RuntimeConfigError> {
    if value > maximum {
        Err(RuntimeConfigError::AboveHardMaximum {
            field,
            observed: value,
            maximum,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extreme_capacities_are_rejected_without_panicking_or_allocating() {
        let result = std::panic::catch_unwind(|| {
            RuntimeConfig::new(
                1024,
                1024,
                64,
                4,
                4,
                usize::MAX,
                4096,
                ParserSettings::new(64).expect("baseline setting is valid"),
            )
        });

        assert!(matches!(
            result,
            Ok(Err(RuntimeConfigError::AboveHardMaximum {
                field: "max_concurrent_parses",
                ..
            }))
        ));
        assert!(matches!(
            ParserSettings::new(usize::MAX),
            Err(RuntimeConfigError::AboveHardMaximum {
                field: "input_chunk_bytes",
                ..
            })
        ));
    }
}
