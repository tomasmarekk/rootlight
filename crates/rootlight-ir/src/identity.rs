//! Versioned semantic-identity recipes and producer-neutral claim envelopes.

use std::io::{self, BufReader, Read, Write};

use rootlight_ids::{
    ContentHash, FactId, FileId, FileIdentity, GenerationId, RepositoryId, SymbolId,
    SymbolIdentity, derive_fact, derive_file, derive_symbol,
};
use serde::{Deserialize, Serialize};

use crate::{
    ContainerRef, CoverageRecord, DiagnosticRecord, EntityKind, ExtensionCriticality,
    ExtensionEnvelope, FactEvidence, FactRef, MAX_LEXICAL_SIGNATURE_BYTES, OccurrenceRecord,
    ProvenanceRecord, RelationRecord, SkippedRegion, SourceMappingRecord, SourceRef,
};

/// Namespace carrying one unverified structured file-identity claim.
pub const FILE_IDENTITY_CLAIM_NAMESPACE: &str = "dev.rootlight.identity.file";
/// Namespace carrying one unverified structured symbol-identity claim.
pub const SYMBOL_IDENTITY_CLAIM_NAMESPACE: &str = "dev.rootlight.identity.symbol";
/// Legacy identity-claim payload version, still used for file claims.
pub const IDENTITY_CLAIM_VERSION: &str = "1.0";

const COMPACT_SYMBOL_IDENTITY_CLAIM_VERSION: &str = "1.1";
const POSITIONAL_SYMBOL_IDENTITY_CLAIM_VERSION: &str = "1.2";
const DENSE_SYMBOL_IDENTITY_CLAIM_VERSION: &str = "1.3";

const PROVENANCE_FACT_DOMAIN: &str = "rootlight.provenance/v2";
const OCCURRENCE_FACT_DOMAIN: &str = "rootlight.occurrence/v2";
const RELATION_FACT_DOMAIN: &str = "rootlight.relation/v2";
const SOURCE_MAPPING_FACT_DOMAIN: &str = "rootlight.source-mapping/v2";
const COVERAGE_FACT_DOMAIN: &str = "rootlight.coverage/v2";
const RUST_IMPL_HEADER_CONTEXT: &str = "rootlight/treesitter-scope-header/v1";
const RUST_SCOPE_IDENTITY_CONTEXT: &str = "rootlight/scope-container/v1";
const SKIPPED_REGION_FACT_DOMAIN: &str = "rootlight.skipped-region/v2";
const DIAGNOSTIC_FACT_DOMAIN: &str = "rootlight.diagnostic/v2";

/// Unverified structured inputs from which a consumer can recompute a [`FileId`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentityClaim {
    /// Claimed file identity.
    pub file: FileId,
    /// Repository owning the file.
    pub repository: RepositoryId,
    /// Canonical repository-relative presentation path.
    pub path: String,
    /// Lossless platform path identity bytes used by the VFS.
    pub path_identity: Vec<u8>,
    /// Immutable content hash bound to the manifest entry.
    pub content_hash: ContentHash,
    /// Immutable file size bound to the manifest entry.
    pub byte_length: u64,
}

impl FileIdentityClaim {
    /// Recomputes the file identity from the claim inputs.
    #[must_use]
    pub fn derived_file(&self) -> FileId {
        derive_file(FileIdentity {
            repository: self.repository,
            path_identity: &self.path_identity,
        })
        .id()
    }
}

/// Unverified structured inputs from which a consumer can recompute a [`SymbolId`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolIdentityClaim {
    /// Claimed symbol identity.
    pub symbol: SymbolId,
    /// Repository owning the symbol.
    pub repository: RepositoryId,
    /// Canonical language identity.
    pub language: String,
    /// Closed semantic kind retained by normalized IR.
    pub kind: EntityKind,
    /// Structured semantic container retained by normalized IR.
    pub container: Option<ContainerRef>,
    /// Canonical container discriminator used by the symbol recipe.
    pub container_identity: Vec<u8>,
    /// Canonical declared identity used by the symbol recipe.
    pub declared_identity: String,
    /// Canonical overload or signature discriminator.
    pub signature_discriminator: Vec<u8>,
    /// Canonical build-context discriminator.
    pub build_context_discriminator: Vec<u8>,
}

impl SymbolIdentityClaim {
    /// Recomputes the symbol identity from the claim inputs.
    #[must_use]
    pub fn derived_symbol(&self) -> SymbolId {
        derive_symbol(SymbolIdentity {
            repository: self.repository,
            language: &self.language,
            semantic_kind: entity_kind_identity_label(self.kind),
            container_identity: &self.container_identity,
            declared_identity: &self.declared_identity,
            signature_discriminator: &self.signature_discriminator,
            build_context_discriminator: &self.build_context_discriminator,
        })
        .id()
    }
}

// The wire encoding is separate from the public recipe so byte discriminators
// remain lossless without expanding signature text into numeric JSON arrays.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactSymbolIdentityClaim {
    symbol: SymbolId,
    repository: RepositoryId,
    language: String,
    kind: EntityKind,
    container: Option<ContainerRef>,
    container_identity: String,
    declared_identity: String,
    signature_discriminator: String,
    build_context_discriminator: String,
}

impl CompactSymbolIdentityClaim {
    #[cfg(test)]
    fn from_claim(claim: &SymbolIdentityClaim) -> Result<Self, IdentityClaimError> {
        Ok(Self {
            symbol: claim.symbol,
            repository: claim.repository,
            language: claim.language.clone(),
            kind: claim.kind,
            container: claim.container,
            container_identity: encode_claim_discriminator(&claim.container_identity)?,
            declared_identity: claim.declared_identity.clone(),
            signature_discriminator: encode_claim_discriminator(&claim.signature_discriminator)?,
            build_context_discriminator: encode_claim_discriminator(
                &claim.build_context_discriminator,
            )?,
        })
    }

    fn into_claim(
        self,
        checkpoint: &mut impl FnMut() -> bool,
    ) -> Result<SymbolIdentityClaim, IdentityClaimError> {
        Ok(SymbolIdentityClaim {
            symbol: self.symbol,
            repository: self.repository,
            language: self.language,
            kind: self.kind,
            container: self.container,
            container_identity: decode_claim_discriminator(&self.container_identity, checkpoint)?,
            declared_identity: self.declared_identity,
            signature_discriminator: decode_claim_discriminator(
                &self.signature_discriminator,
                checkpoint,
            )?,
            build_context_discriminator: decode_claim_discriminator(
                &self.build_context_discriminator,
                checkpoint,
            )?,
        })
    }
}

// Field order is the versioned wire contract. Repository and symbol IDs already
// belong to the envelope and its exact entity evidence, not a second claim copy.
#[derive(Serialize, Deserialize)]
struct PositionalSymbolIdentityClaim(
    String,
    EntityKind,
    Option<ContainerRef>,
    String,
    String,
    String,
    String,
);

impl PositionalSymbolIdentityClaim {
    #[cfg(test)]
    fn from_claim(claim: &SymbolIdentityClaim) -> Result<Self, IdentityClaimError> {
        let wire = CompactSymbolIdentityClaim::from_claim(claim)?;
        Ok(Self(
            wire.language,
            wire.kind,
            wire.container,
            wire.container_identity,
            wire.declared_identity,
            wire.signature_discriminator,
            wire.build_context_discriminator,
        ))
    }

    fn into_claim(
        self,
        repository: RepositoryId,
        symbol: SymbolId,
        checkpoint: &mut impl FnMut() -> bool,
    ) -> Result<SymbolIdentityClaim, IdentityClaimError> {
        self.into_claim_with_encoding(repository, symbol, checkpoint, false)
    }

    fn from_dense_claim(claim: &SymbolIdentityClaim) -> Result<Self, IdentityClaimError> {
        let mut checkpoint = || true;
        Ok(Self(
            claim.language.clone(),
            claim.kind,
            claim.container,
            encode_dense_claim_discriminator(&claim.container_identity, &mut checkpoint)?,
            claim.declared_identity.clone(),
            encode_dense_claim_discriminator(&claim.signature_discriminator, &mut checkpoint)?,
            encode_dense_claim_discriminator(&claim.build_context_discriminator, &mut checkpoint)?,
        ))
    }

    fn into_claim_with_encoding(
        self,
        repository: RepositoryId,
        symbol: SymbolId,
        checkpoint: &mut impl FnMut() -> bool,
        dense: bool,
    ) -> Result<SymbolIdentityClaim, IdentityClaimError> {
        let Self(
            language,
            kind,
            container,
            container_identity,
            declared_identity,
            signature_discriminator,
            build_context_discriminator,
        ) = self;
        Ok(SymbolIdentityClaim {
            symbol,
            repository,
            language,
            kind,
            container,
            container_identity: decode_versioned_claim_discriminator(
                &container_identity,
                checkpoint,
                dense,
            )?,
            declared_identity,
            signature_discriminator: decode_versioned_claim_discriminator(
                &signature_discriminator,
                checkpoint,
                dense,
            )?,
            build_context_discriminator: decode_versioned_claim_discriminator(
                &build_context_discriminator,
                checkpoint,
                dense,
            )?,
        })
    }
}

#[derive(Clone, Copy)]
enum DenseDiscriminatorEncoding {
    Utf8,
    Hex,
    Base64,
}

// Count serde's actual escaping without materializing competing JSON strings.
struct ClaimJsonLengthWriter<'checkpoint, F> {
    length: usize,
    checkpoint: &'checkpoint mut F,
    interrupted: bool,
}

impl<F: FnMut() -> bool> Write for ClaimJsonLengthWriter<'_, F> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        for chunk in buffer.chunks(4 * 1024) {
            if self.interrupted || !(self.checkpoint)() {
                self.interrupted = true;
                return Err(checkpoint_io_error());
            }
            self.length = self
                .length
                .checked_add(chunk.len())
                .ok_or_else(|| io::Error::other("identity claim encoded length overflow"))?;
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn claim_json_text_length(
    text: &str,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<usize, IdentityClaimError> {
    let mut writer = ClaimJsonLengthWriter {
        length: 0,
        checkpoint,
        interrupted: false,
    };
    if serde_json::to_writer(&mut writer, text).is_err() {
        return Err(if writer.interrupted {
            IdentityClaimError::Interrupted
        } else {
            IdentityClaimError::MalformedPayload
        });
    }
    writer
        .length
        .checked_add(2)
        .ok_or(IdentityClaimError::MalformedPayload)
}

fn claim_base64_length(length: usize) -> Result<usize, IdentityClaimError> {
    length
        .checked_add(2)
        .and_then(|value| value.checked_div(3))
        .and_then(|value| value.checked_mul(4))
        .ok_or(IdentityClaimError::MalformedPayload)
}

fn encode_dense_claim_discriminator(
    bytes: &[u8],
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<String, IdentityClaimError> {
    if !checkpoint() {
        return Err(IdentityClaimError::Interrupted);
    }
    let text = if claim_discriminator_is_utf8(bytes, checkpoint)? {
        Some(std::str::from_utf8(bytes).map_err(|_| IdentityClaimError::MalformedPayload)?)
    } else {
        None
    };
    let hex_length = bytes
        .len()
        .checked_mul(2)
        .and_then(|length| length.checked_add(4))
        .ok_or(IdentityClaimError::MalformedPayload)?;
    let mut encoding = DenseDiscriminatorEncoding::Hex;
    let mut json_length = hex_length;
    // Equal wire costs retain legacy priority: u: before h: before b:.
    if let Some(text) = text {
        let utf8_length = claim_json_text_length(text, checkpoint)?;
        if utf8_length <= json_length {
            encoding = DenseDiscriminatorEncoding::Utf8;
            json_length = utf8_length;
        }
    }
    let base64_length = claim_base64_length(bytes.len())?
        .checked_add(4)
        .ok_or(IdentityClaimError::MalformedPayload)?;
    if base64_length < json_length {
        encoding = DenseDiscriminatorEncoding::Base64;
    }
    if matches!(encoding, DenseDiscriminatorEncoding::Base64) {
        return encode_claim_base64(bytes, checkpoint);
    }
    let length = match encoding {
        DenseDiscriminatorEncoding::Utf8 => bytes.len().checked_add(2),
        DenseDiscriminatorEncoding::Hex => bytes
            .len()
            .checked_mul(2)
            .and_then(|length| length.checked_add(2)),
        DenseDiscriminatorEncoding::Base64 => unreachable!("base64 returns before text encoding"),
    }
    .ok_or(IdentityClaimError::MalformedPayload)?;
    let mut encoded = String::new();
    encoded
        .try_reserve_exact(length)
        .map_err(|_| IdentityClaimError::MalformedPayload)?;
    match encoding {
        DenseDiscriminatorEncoding::Utf8 => {
            encoded.push_str("u:");
            for (index, character) in text
                .ok_or(IdentityClaimError::MalformedPayload)?
                .chars()
                .enumerate()
            {
                if index.is_multiple_of(1024) && !checkpoint() {
                    return Err(IdentityClaimError::Interrupted);
                }
                encoded.push(character);
            }
        }
        DenseDiscriminatorEncoding::Hex => {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            encoded.push_str("h:");
            for chunk in bytes.chunks(2 * 1024) {
                if !checkpoint() {
                    return Err(IdentityClaimError::Interrupted);
                }
                for byte in chunk {
                    encoded.push(char::from(HEX[usize::from(byte >> 4)]));
                    encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
                }
            }
        }
        DenseDiscriminatorEncoding::Base64 => unreachable!("base64 returns before text encoding"),
    }
    Ok(encoded)
}

fn encode_claim_base64(
    bytes: &[u8],
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<String, IdentityClaimError> {
    if !checkpoint() {
        return Err(IdentityClaimError::Interrupted);
    }
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let length = claim_base64_length(bytes.len())?
        .checked_add(2)
        .ok_or(IdentityClaimError::MalformedPayload)?;
    let mut encoded = String::new();
    encoded
        .try_reserve_exact(length)
        .map_err(|_| IdentityClaimError::MalformedPayload)?;
    encoded.push_str("b:");
    for chunk in bytes.chunks(3 * 1024) {
        if !checkpoint() {
            return Err(IdentityClaimError::Interrupted);
        }
        for group in chunk.chunks(3) {
            let first = *group.first().ok_or(IdentityClaimError::MalformedPayload)?;
            let second = group.get(1).copied().unwrap_or(0);
            let third = group.get(2).copied().unwrap_or(0);
            encoded.push(char::from(ALPHABET[usize::from(first >> 2)]));
            encoded.push(char::from(
                ALPHABET[usize::from(((first & 3) << 4) | (second >> 4))],
            ));
            encoded.push(if group.len() > 1 {
                char::from(ALPHABET[usize::from(((second & 15) << 2) | (third >> 6))])
            } else {
                '='
            });
            encoded.push(if group.len() > 2 {
                char::from(ALPHABET[usize::from(third & 63)])
            } else {
                '='
            });
        }
    }
    Ok(encoded)
}

fn decode_claim_base64(
    content: &[u8],
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<Vec<u8>, IdentityClaimError> {
    if !checkpoint() {
        return Err(IdentityClaimError::Interrupted);
    }
    if !content.len().is_multiple_of(4) {
        return Err(IdentityClaimError::MalformedPayload);
    }
    let padding = if content.ends_with(b"==") {
        2
    } else if content.ends_with(b"=") {
        1
    } else {
        0
    };
    let length = (content.len() / 4)
        .checked_mul(3)
        .and_then(|length| length.checked_sub(padding))
        .ok_or(IdentityClaimError::MalformedPayload)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| IdentityClaimError::MalformedPayload)?;
    let groups = content.len() / 4;
    for (index, group) in content.chunks_exact(4).enumerate() {
        if index.is_multiple_of(1024) && !checkpoint() {
            return Err(IdentityClaimError::Interrupted);
        }
        let [first, second, third, fourth] = group else {
            return Err(IdentityClaimError::MalformedPayload);
        };
        let first = decode_claim_base64_digit(*first)?;
        let second = decode_claim_base64_digit(*second)?;
        bytes.push((first << 2) | (second >> 4));
        if *third == b'=' {
            if *fourth != b'=' || index != groups - 1 || second & 15 != 0 {
                return Err(IdentityClaimError::MalformedPayload);
            }
        } else {
            let third = decode_claim_base64_digit(*third)?;
            bytes.push(((second & 15) << 4) | (third >> 2));
            if *fourth == b'=' {
                if index != groups - 1 || third & 3 != 0 {
                    return Err(IdentityClaimError::MalformedPayload);
                }
            } else {
                bytes.push(((third & 3) << 6) | decode_claim_base64_digit(*fourth)?);
            }
        }
    }
    if bytes.len() != length {
        return Err(IdentityClaimError::MalformedPayload);
    }
    Ok(bytes)
}

fn decode_claim_base64_digit(byte: u8) -> Result<u8, IdentityClaimError> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(IdentityClaimError::MalformedPayload),
    }
}

fn decode_versioned_claim_discriminator(
    encoded: &str,
    checkpoint: &mut impl FnMut() -> bool,
    dense: bool,
) -> Result<Vec<u8>, IdentityClaimError> {
    if !dense {
        return decode_claim_discriminator(encoded, checkpoint);
    }
    if !checkpoint() {
        return Err(IdentityClaimError::Interrupted);
    }
    let bytes = if let Some(content) = encoded.strip_prefix("b:") {
        decode_claim_base64(content.as_bytes(), checkpoint)?
    } else if let Some(content) = encoded.strip_prefix("h:") {
        if !content.len().is_multiple_of(2) {
            return Err(IdentityClaimError::MalformedPayload);
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(content.len() / 2)
            .map_err(|_| IdentityClaimError::MalformedPayload)?;
        for chunk in content.as_bytes().chunks(4 * 1024) {
            if !checkpoint() {
                return Err(IdentityClaimError::Interrupted);
            }
            for pair in chunk.chunks_exact(2) {
                let [high, low] = pair else {
                    return Err(IdentityClaimError::MalformedPayload);
                };
                bytes.push((decode_claim_hex_digit(*high)? << 4) | decode_claim_hex_digit(*low)?);
            }
        }
        bytes
    } else {
        decode_claim_discriminator(encoded, checkpoint)?
    };
    if encode_dense_claim_discriminator(&bytes, checkpoint)? != encoded {
        return Err(IdentityClaimError::NoncanonicalPayload);
    }
    Ok(bytes)
}

#[cfg(test)]
fn encode_claim_discriminator(bytes: &[u8]) -> Result<String, IdentityClaimError> {
    let text = std::str::from_utf8(bytes).ok();
    let encoded_length = if text.is_some() {
        bytes.len()
    } else {
        bytes
            .len()
            .checked_mul(2)
            .ok_or(IdentityClaimError::MalformedPayload)?
    }
    .checked_add(2)
    .ok_or(IdentityClaimError::MalformedPayload)?;
    let mut encoded = String::new();
    encoded
        .try_reserve_exact(encoded_length)
        .map_err(|_| IdentityClaimError::MalformedPayload)?;
    if let Some(text) = text {
        encoded.push_str("u:");
        encoded.push_str(text);
    } else {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        encoded.push_str("h:");
        for byte in bytes {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    Ok(encoded)
}

fn decode_claim_discriminator(
    encoded: &str,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<Vec<u8>, IdentityClaimError> {
    if !checkpoint() {
        return Err(IdentityClaimError::Interrupted);
    }
    let (content, hexadecimal) = if let Some(text) = encoded.strip_prefix("u:") {
        (text.as_bytes(), false)
    } else if let Some(hex) = encoded.strip_prefix("h:") {
        if hex.len() % 2 != 0 {
            return Err(IdentityClaimError::MalformedPayload);
        }
        (hex.as_bytes(), true)
    } else {
        return Err(IdentityClaimError::MalformedPayload);
    };
    let decoded_length = if hexadecimal {
        content.len() / 2
    } else {
        content.len()
    };
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(decoded_length)
        .map_err(|_| IdentityClaimError::MalformedPayload)?;
    for chunk in content.chunks(4 * 1024) {
        if !checkpoint() {
            return Err(IdentityClaimError::Interrupted);
        }
        if hexadecimal {
            for pair in chunk.chunks_exact(2) {
                let high = decode_claim_hex_digit(pair[0])?;
                let low = decode_claim_hex_digit(pair[1])?;
                bytes.push((high << 4) | low);
            }
        } else {
            bytes.extend_from_slice(chunk);
        }
    }
    if hexadecimal && claim_discriminator_is_utf8(&bytes, checkpoint)? {
        return Err(IdentityClaimError::NoncanonicalPayload);
    }
    Ok(bytes)
}

fn decode_claim_hex_digit(byte: u8) -> Result<u8, IdentityClaimError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Err(IdentityClaimError::NoncanonicalPayload),
        _ => Err(IdentityClaimError::MalformedPayload),
    }
}

fn claim_discriminator_is_utf8(
    bytes: &[u8],
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<bool, IdentityClaimError> {
    let mut position = 0;
    while position < bytes.len() {
        if !checkpoint() {
            return Err(IdentityClaimError::Interrupted);
        }
        let end = position + (bytes.len() - position).min(4 * 1024);
        match std::str::from_utf8(&bytes[position..end]) {
            Ok(_) => position = end,
            Err(error) => {
                if error.error_len().is_some() || end == bytes.len() {
                    return Ok(false);
                }
                // An incomplete code point is retried with the next chunk;
                // otherwise a valid boundary-spanning UTF-8 sequence could
                // incorrectly permit the hexadecimal alternative.
                position += error.valid_up_to();
            }
        }
    }
    Ok(true)
}

/// Canonicalizes source signature text for provider-independent symbol identity.
///
/// Whitespace is presentation-only and therefore excluded from the durable
/// discriminator. Oversized or empty signatures deliberately collapse to no
/// discriminator instead of letting providers choose incompatible fallbacks.
#[must_use]
pub fn canonical_symbol_signature(text: &str, maximum_string_bytes: usize) -> Option<String> {
    if text.is_empty()
        || text.len() > MAX_LEXICAL_SIGNATURE_BYTES
        || text.len() > maximum_string_bytes
    {
        return None;
    }
    let mut canonical = String::with_capacity(text.len());
    canonical.extend(text.chars().filter(|character| !character.is_whitespace()));
    (!canonical.is_empty()).then_some(canonical)
}

/// Canonical identity of one Rust `impl` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustImplScopeIdentity {
    header: [u8; 32],
    display: String,
}

impl RustImplScopeIdentity {
    /// Returns the stable header digest.
    #[must_use]
    pub const fn header(&self) -> [u8; 32] {
        self.header
    }

    /// Returns the normalized display prefix.
    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }
}

/// Bounded Rust `impl` identity canonicalization failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RustScopeIdentityError {
    /// A length or allocation exceeded the declared identity budget.
    #[error("Rust scope identity exceeded its resource budget")]
    ResourceLimit,
    /// Checked length accounting overflowed.
    #[error("Rust scope identity accounting overflowed")]
    AccountingOverflow,
}

/// Canonicalizes a Rust `impl` self type and optional trait into one header identity.
///
/// Both structural and project providers use this recipe so comments and
/// presentation whitespace cannot change a method's durable container.
///
/// # Errors
///
/// Returns [`RustScopeIdentityError`] when bounded allocation or checked
/// accounting cannot represent the input.
pub fn canonical_rust_impl_scope(
    self_type: &str,
    trait_type: Option<&str>,
    maximum_string_bytes: usize,
) -> Result<Option<RustImplScopeIdentity>, RustScopeIdentityError> {
    let Some(self_type) = canonical_rust_scope_component(self_type, maximum_string_bytes)? else {
        return Ok(None);
    };
    let has_trait_type = trait_type.is_some();
    let trait_type = trait_type
        .map(|value| canonical_rust_scope_component(value, maximum_string_bytes))
        .transpose()?
        .flatten();
    if has_trait_type && trait_type.is_none() {
        return Ok(None);
    }
    let mut hasher = blake3::Hasher::new_derive_key(RUST_IMPL_HEADER_CONTEXT);
    hash_rust_scope_component(&mut hasher, b"self", &self_type)?;
    let display = match trait_type {
        Some(trait_type) => {
            hash_rust_scope_component(&mut hasher, b"trait", &trait_type)?;
            let length = self_type
                .display
                .len()
                .checked_add(trait_type.display.len())
                .and_then(|length| length.checked_add("< as >".len()))
                .ok_or(RustScopeIdentityError::AccountingOverflow)?;
            if length > maximum_string_bytes {
                return Ok(None);
            }
            format!("<{} as {}>", self_type.display, trait_type.display)
        }
        None => {
            hash_rust_scope_marker(&mut hasher, b"inherent")?;
            self_type.display
        }
    };
    Ok(Some(RustImplScopeIdentity {
        header: *hasher.finalize().as_bytes(),
        display,
    }))
}

/// Derives the container discriminator for a canonical Rust `impl` scope.
///
/// # Errors
///
/// Returns [`RustScopeIdentityError`] when the fixed syntax label length cannot
/// be represented.
pub fn derive_rust_impl_scope_identity(
    parent: Option<[u8; 32]>,
    header: [u8; 32],
) -> Result<[u8; 32], RustScopeIdentityError> {
    let mut hasher = blake3::Hasher::new_derive_key(RUST_SCOPE_IDENTITY_CONTEXT);
    match parent {
        Some(parent) => {
            hasher.update(&[1]);
            hasher.update(&parent);
        }
        None => {
            hasher.update(&[0]);
        }
    }
    let syntax_kind = b"rust.impl.scope";
    let syntax_length =
        u64::try_from(syntax_kind.len()).map_err(|_| RustScopeIdentityError::AccountingOverflow)?;
    hasher.update(&syntax_length.to_be_bytes());
    hasher.update(syntax_kind);
    hasher.update(&header);
    Ok(*hasher.finalize().as_bytes())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RustScopeComponent {
    tokens: Vec<String>,
    display: String,
}

fn canonical_rust_scope_component(
    text: &str,
    maximum_string_bytes: usize,
) -> Result<Option<RustScopeComponent>, RustScopeIdentityError> {
    if text.is_empty()
        || text.len() > MAX_LEXICAL_SIGNATURE_BYTES
        || text.len() > maximum_string_bytes
    {
        return Ok(None);
    }
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    tokens
        .try_reserve_exact(text.len().min(32))
        .map_err(|_| RustScopeIdentityError::ResourceLimit)?;
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") {
            index = bytes[index..]
                .iter()
                .position(|byte| matches!(byte, b'\r' | b'\n'))
                .map_or(bytes.len(), |offset| index + offset);
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            let mut depth = 1usize;
            index += 2;
            while index < bytes.len() && depth > 0 {
                if bytes[index..].starts_with(b"/*") {
                    depth = depth
                        .checked_add(1)
                        .ok_or(RustScopeIdentityError::AccountingOverflow)?;
                    index += 2;
                } else if bytes[index..].starts_with(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            if depth != 0 {
                return Ok(None);
            }
            continue;
        }
        if let Some(end) = rust_raw_string_end(bytes, index) {
            push_rust_scope_token(&mut tokens, text, index, end)?;
            index = end;
            continue;
        }
        let normal_string_start = match bytes.get(index..index.saturating_add(2)) {
            Some([b'b' | b'c', b'"']) => Some(index + 1),
            _ if bytes[index] == b'"' => Some(index),
            _ => None,
        };
        if let Some(quote) = normal_string_start {
            let Some(end) = rust_quoted_end(bytes, quote, b'"') else {
                return Ok(None);
            };
            push_rust_scope_token(&mut tokens, text, index, end)?;
            index = end;
            continue;
        }
        let char_start = if bytes.get(index..index.saturating_add(2)) == Some(b"b'") {
            Some(index + 1)
        } else if bytes[index] == b'\'' {
            Some(index)
        } else {
            None
        };
        if let Some(quote) = char_start
            && let Some(end) = rust_char_literal_end(text, quote)
        {
            push_rust_scope_token(&mut tokens, text, index, end)?;
            index = end;
            continue;
        }
        let character = text
            .get(index..)
            .and_then(|remaining| remaining.chars().next())
            .ok_or(RustScopeIdentityError::AccountingOverflow)?;
        if character.is_whitespace() {
            index = index
                .checked_add(character.len_utf8())
                .ok_or(RustScopeIdentityError::AccountingOverflow)?;
            continue;
        }
        if is_rust_scope_word_character(character)
            || character == '\''
                && text
                    .get(index + 1..)
                    .and_then(|remaining| remaining.chars().next())
                    .is_some_and(is_rust_scope_word_character)
        {
            let start = index;
            if character == '\'' {
                index += 1;
            }
            while index < bytes.len() {
                let next = text
                    .get(index..)
                    .and_then(|remaining| remaining.chars().next())
                    .ok_or(RustScopeIdentityError::AccountingOverflow)?;
                if !(is_rust_scope_word_character(next)
                    || next == '#' && index == start + 1 && bytes.get(start) == Some(&b'r'))
                {
                    break;
                }
                index = index
                    .checked_add(next.len_utf8())
                    .ok_or(RustScopeIdentityError::AccountingOverflow)?;
            }
            push_rust_scope_token(&mut tokens, text, start, index)?;
            continue;
        }
        let end = index
            .checked_add(character.len_utf8())
            .ok_or(RustScopeIdentityError::AccountingOverflow)?;
        push_rust_scope_token(&mut tokens, text, index, end)?;
        index = end;
    }
    if tokens.is_empty() {
        return Ok(None);
    }
    let mut display = String::new();
    display
        .try_reserve_exact(text.len())
        .map_err(|_| RustScopeIdentityError::ResourceLimit)?;
    let mut previous_word = false;
    for token in &tokens {
        let word = rust_scope_token_is_word(token);
        if previous_word && word {
            display.push(' ');
        }
        display.push_str(token);
        previous_word = word;
    }
    if display.len() > maximum_string_bytes {
        return Ok(None);
    }
    Ok(Some(RustScopeComponent { tokens, display }))
}

fn hash_rust_scope_component(
    hasher: &mut blake3::Hasher,
    label: &[u8],
    component: &RustScopeComponent,
) -> Result<(), RustScopeIdentityError> {
    hash_rust_scope_marker(hasher, label)?;
    let token_count = u64::try_from(component.tokens.len())
        .map_err(|_| RustScopeIdentityError::AccountingOverflow)?;
    hasher.update(&token_count.to_be_bytes());
    for token in &component.tokens {
        hash_rust_scope_marker(hasher, token.as_bytes())?;
    }
    Ok(())
}

fn hash_rust_scope_marker(
    hasher: &mut blake3::Hasher,
    value: &[u8],
) -> Result<(), RustScopeIdentityError> {
    let length =
        u64::try_from(value.len()).map_err(|_| RustScopeIdentityError::AccountingOverflow)?;
    hasher.update(&length.to_be_bytes());
    hasher.update(value);
    Ok(())
}

fn push_rust_scope_token(
    tokens: &mut Vec<String>,
    text: &str,
    start: usize,
    end: usize,
) -> Result<(), RustScopeIdentityError> {
    let token = text
        .get(start..end)
        .ok_or(RustScopeIdentityError::AccountingOverflow)?;
    tokens
        .try_reserve(1)
        .map_err(|_| RustScopeIdentityError::ResourceLimit)?;
    tokens.push(token.to_owned());
    Ok(())
}

fn is_rust_scope_word_character(character: char) -> bool {
    character == '_'
        || character.is_alphanumeric()
        || !character.is_ascii() && !character.is_whitespace()
}

fn rust_scope_token_is_word(token: &str) -> bool {
    token
        .chars()
        .next()
        .is_some_and(|character| is_rust_scope_word_character(character) || character == '\'')
}

fn rust_raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start;
    if matches!(bytes.get(cursor), Some(b'b' | b'c')) {
        cursor = cursor.checked_add(1)?;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor = cursor.checked_add(1)?;
    let hashes_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor = cursor.checked_add(1)?;
    }
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    let hash_count = cursor.checked_sub(hashes_start)?;
    cursor = cursor.checked_add(1)?;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"' {
            let hashes_end = cursor.checked_add(1)?.checked_add(hash_count)?;
            if hashes_end <= bytes.len()
                && bytes[cursor + 1..hashes_end]
                    .iter()
                    .all(|byte| *byte == b'#')
            {
                return Some(hashes_end);
            }
        }
        cursor = cursor.checked_add(1)?;
    }
    None
}

fn rust_quoted_end(bytes: &[u8], quote: usize, delimiter: u8) -> Option<usize> {
    if bytes.get(quote) != Some(&delimiter) {
        return None;
    }
    let mut cursor = quote.checked_add(1)?;
    let mut escaped = false;
    while let Some(byte) = bytes.get(cursor).copied() {
        cursor = cursor.checked_add(1)?;
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == delimiter {
            return Some(cursor);
        }
    }
    None
}

fn rust_char_literal_end(text: &str, quote: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.get(quote) != Some(&b'\'') {
        return None;
    }
    let content = quote.checked_add(1)?;
    let end = if bytes.get(content) == Some(&b'\\') {
        let escaped = content.checked_add(1)?;
        match bytes.get(escaped).copied()? {
            b'x' => escaped.checked_add(3)?,
            b'u' if bytes.get(escaped.checked_add(1)?) == Some(&b'{') => {
                let close = bytes
                    .get(escaped.checked_add(2)?..)?
                    .iter()
                    .position(|byte| *byte == b'}')?;
                escaped.checked_add(3)?.checked_add(close)?
            }
            _ => escaped.checked_add(1)?,
        }
    } else {
        let character = text.get(content..)?.chars().next()?;
        content.checked_add(character.len_utf8())?
    };
    (bytes.get(end) == Some(&b'\''))
        .then(|| end.checked_add(1))
        .flatten()
}

/// Identity-claim envelope construction or decoding failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdentityClaimError {
    /// The payload could not be encoded or decoded.
    #[error("identity claim payload is malformed")]
    MalformedPayload,
    /// The payload is not the unique canonical JSON encoding.
    #[error("identity claim payload is not canonical")]
    NoncanonicalPayload,
    /// The namespace, version, or criticality is not the claim contract.
    #[error("identity claim envelope contract is unsupported")]
    UnsupportedEnvelope,
    /// The envelope owner or evidence does not match the claim.
    #[error("identity claim envelope ownership or evidence does not match")]
    EnvelopeMismatch,
    /// Cooperative payload decoding or canonical re-encoding was interrupted.
    #[error("identity claim processing was interrupted")]
    Interrupted,
}

/// Typed fact recipe encoding failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("typed fact identity could not be encoded")]
pub struct FactIdentityRecipeError;

/// Builds a noncritical envelope carrying one unverified file claim.
///
/// # Errors
///
/// Returns [`IdentityClaimError`] when the claim and direct source disagree or
/// when canonical payload encoding fails.
pub fn new_file_identity_claim_envelope(
    claim: &FileIdentityClaim,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
) -> Result<ExtensionEnvelope, IdentityClaimError> {
    if claim.repository != source.repository()
        || claim.file != source.span().file()
        || claim.content_hash != source.content_hash()
        || claim.byte_length != source.span().end_byte()
        || source.span().start_byte() != 0
        || source.generation() != generation
        || claim.derived_file() != claim.file
    {
        return Err(IdentityClaimError::EnvelopeMismatch);
    }
    let payload = serde_json::to_string(claim).map_err(|_| IdentityClaimError::MalformedPayload)?;
    identity_claim_envelope(
        claim.repository,
        generation,
        FILE_IDENTITY_CLAIM_NAMESPACE,
        payload,
        provenance,
        source,
        FactRef::File(claim.file),
    )
}

/// Builds a noncritical envelope carrying one unverified symbol claim.
///
/// New envelopes use positional version 1.3 with lossless byte discriminators
/// selected by their smallest canonical escaped-JSON representation.
/// Repository and symbol IDs are bound by envelope ownership and exact entity
/// evidence. The structured claim and symbol identity recipe are unchanged.
///
/// # Errors
///
/// Returns [`IdentityClaimError`] when the claim and direct source disagree or
/// when canonical payload encoding fails.
pub fn new_symbol_identity_claim_envelope(
    claim: &SymbolIdentityClaim,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
) -> Result<ExtensionEnvelope, IdentityClaimError> {
    if claim.repository != source.repository()
        || source.generation() != generation
        || claim.derived_symbol() != claim.symbol
    {
        return Err(IdentityClaimError::EnvelopeMismatch);
    }
    let wire = PositionalSymbolIdentityClaim::from_dense_claim(claim)?;
    let payload = serde_json::to_string(&wire).map_err(|_| IdentityClaimError::MalformedPayload)?;
    identity_claim_envelope_with_encoding(
        claim.repository,
        generation,
        (
            SYMBOL_IDENTITY_CLAIM_NAMESPACE,
            DENSE_SYMBOL_IDENTITY_CLAIM_VERSION,
        ),
        payload,
        provenance,
        source,
        FactRef::Entity(claim.symbol),
    )
}

/// Decodes and validates one file identity-claim envelope.
///
/// # Errors
///
/// Returns [`IdentityClaimError`] for a malformed, noncanonical, mismatched, or
/// unsupported claim envelope.
pub fn decode_file_identity_claim_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<FileIdentityClaim, IdentityClaimError> {
    decode_file_identity_claim_envelope_with_checkpoint(envelope, || true)
}

/// Decodes a file claim with bounded cooperative payload checkpoints.
///
/// # Errors
///
/// Returns [`IdentityClaimError`] for an interrupted, malformed,
/// noncanonical, mismatched, or unsupported claim envelope.
pub fn decode_file_identity_claim_envelope_with_checkpoint(
    envelope: &ExtensionEnvelope,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<FileIdentityClaim, IdentityClaimError> {
    require_claim_envelope(envelope, FILE_IDENTITY_CLAIM_NAMESPACE)?;
    let claim: FileIdentityClaim = decode_canonical_payload(&envelope.payload, &mut checkpoint)?;
    let source = require_claim_evidence(envelope, FactRef::File(claim.file))?;
    if !checkpoint() {
        return Err(IdentityClaimError::Interrupted);
    }
    let derived_file = claim.derived_file();
    if !checkpoint() {
        return Err(IdentityClaimError::Interrupted);
    }
    if claim.repository != envelope.repository
        || claim.file != source.span().file()
        || claim.content_hash != source.content_hash()
        || claim.byte_length != source.span().end_byte()
        || source.span().start_byte() != 0
        || derived_file != claim.file
    {
        return Err(IdentityClaimError::EnvelopeMismatch);
    }
    require_envelope_id_with_checkpoint(envelope, &mut checkpoint)?;
    Ok(claim)
}

/// Decodes and validates one symbol identity-claim envelope.
///
/// Accepts canonical legacy 1.0, compact 1.1, positional 1.2 and dense 1.3 payloads.
///
/// # Errors
///
/// Returns [`IdentityClaimError`] for a malformed, noncanonical, mismatched, or
/// unsupported claim envelope.
pub fn decode_symbol_identity_claim_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<SymbolIdentityClaim, IdentityClaimError> {
    decode_symbol_identity_claim_envelope_with_checkpoint(envelope, || true)
}

/// Decodes a symbol claim with bounded cooperative payload checkpoints.
///
/// # Errors
///
/// Returns [`IdentityClaimError`] for an interrupted, malformed,
/// noncanonical, mismatched, or unsupported claim envelope.
pub fn decode_symbol_identity_claim_envelope_with_checkpoint(
    envelope: &ExtensionEnvelope,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<SymbolIdentityClaim, IdentityClaimError> {
    require_claim_envelope(envelope, SYMBOL_IDENTITY_CLAIM_NAMESPACE)?;
    let claim: SymbolIdentityClaim = match envelope.version.as_str() {
        IDENTITY_CLAIM_VERSION => decode_canonical_payload(&envelope.payload, &mut checkpoint)?,
        COMPACT_SYMBOL_IDENTITY_CLAIM_VERSION => {
            let wire: CompactSymbolIdentityClaim =
                decode_canonical_payload(&envelope.payload, &mut checkpoint)?;
            wire.into_claim(&mut checkpoint)?
        }
        POSITIONAL_SYMBOL_IDENTITY_CLAIM_VERSION | DENSE_SYMBOL_IDENTITY_CLAIM_VERSION => {
            let [FactRef::Entity(symbol)] = envelope.evidence.derivation.as_slice() else {
                return Err(IdentityClaimError::EnvelopeMismatch);
            };
            require_claim_evidence(envelope, FactRef::Entity(*symbol))?;
            let wire: PositionalSymbolIdentityClaim =
                decode_canonical_payload(&envelope.payload, &mut checkpoint)?;
            if envelope.version == DENSE_SYMBOL_IDENTITY_CLAIM_VERSION {
                wire.into_claim_with_encoding(envelope.repository, *symbol, &mut checkpoint, true)?
            } else {
                wire.into_claim(envelope.repository, *symbol, &mut checkpoint)?
            }
        }
        _ => return Err(IdentityClaimError::UnsupportedEnvelope),
    };
    let _source = require_claim_evidence(envelope, FactRef::Entity(claim.symbol))?;
    if !checkpoint() {
        return Err(IdentityClaimError::Interrupted);
    }
    let derived_symbol = claim.derived_symbol();
    if !checkpoint() {
        return Err(IdentityClaimError::Interrupted);
    }
    if claim.repository != envelope.repository || derived_symbol != claim.symbol {
        return Err(IdentityClaimError::EnvelopeMismatch);
    }
    require_envelope_id_with_checkpoint(envelope, &mut checkpoint)?;
    Ok(claim)
}

/// Derives a provenance ID from every typed semantic field except `id`.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if canonical JSON encoding fails.
pub fn derive_provenance_record_id(
    record: &ProvenanceRecord,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_provenance_record_id_with_checkpoint(record, || true)
}

/// Derives a provenance ID with checkpoints around recipe allocations.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if a checkpoint stops work or canonical
/// JSON encoding fails.
pub fn derive_provenance_record_id_with_checkpoint(
    record: &ProvenanceRecord,
    checkpoint: impl FnMut() -> bool,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_typed_fact_id_with_checkpoint(PROVENANCE_FACT_DOMAIN, record, checkpoint)
        .map_err(|_| FactIdentityRecipeError)
}

/// Derives an occurrence ID from every typed semantic field except `id`.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if canonical JSON encoding fails.
pub fn derive_occurrence_record_id(
    record: &OccurrenceRecord,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_occurrence_record_id_with_checkpoint(record, || true)
}

/// Derives an occurrence ID with checkpoints around recipe allocations.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if a checkpoint stops work or canonical
/// JSON encoding fails.
pub fn derive_occurrence_record_id_with_checkpoint(
    record: &OccurrenceRecord,
    checkpoint: impl FnMut() -> bool,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_typed_fact_id_with_checkpoint(OCCURRENCE_FACT_DOMAIN, record, checkpoint)
        .map_err(|_| FactIdentityRecipeError)
}

/// Derives a relation ID from every typed semantic field except `id`.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if canonical JSON encoding fails.
pub fn derive_relation_record_id(
    record: &RelationRecord,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_relation_record_id_with_checkpoint(record, || true)
}

/// Derives a relation ID with checkpoints around recipe allocations.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if a checkpoint stops work or canonical
/// JSON encoding fails.
pub fn derive_relation_record_id_with_checkpoint(
    record: &RelationRecord,
    checkpoint: impl FnMut() -> bool,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_typed_fact_id_with_checkpoint(RELATION_FACT_DOMAIN, record, checkpoint)
        .map_err(|_| FactIdentityRecipeError)
}

/// Derives a source-mapping ID from every typed semantic field except `id`.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if canonical JSON encoding fails.
pub fn derive_source_mapping_record_id(
    record: &SourceMappingRecord,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_source_mapping_record_id_with_checkpoint(record, || true)
}

/// Derives a source-mapping ID with checkpoints around recipe allocations.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if a checkpoint stops work or canonical
/// JSON encoding fails.
pub fn derive_source_mapping_record_id_with_checkpoint(
    record: &SourceMappingRecord,
    checkpoint: impl FnMut() -> bool,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_typed_fact_id_with_checkpoint(SOURCE_MAPPING_FACT_DOMAIN, record, checkpoint)
        .map_err(|_| FactIdentityRecipeError)
}

/// Derives a coverage ID from every typed semantic field except `id`.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if canonical JSON encoding fails.
pub fn derive_coverage_record_id(
    record: &CoverageRecord,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_coverage_record_id_with_checkpoint(record, || true)
}

/// Derives a coverage ID with checkpoints around recipe allocations.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if a checkpoint stops work or canonical
/// JSON encoding fails.
pub fn derive_coverage_record_id_with_checkpoint(
    record: &CoverageRecord,
    checkpoint: impl FnMut() -> bool,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_typed_fact_id_with_checkpoint(COVERAGE_FACT_DOMAIN, record, checkpoint)
        .map_err(|_| FactIdentityRecipeError)
}

/// Derives a skipped-region ID from every typed semantic field except `id`.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if canonical JSON encoding fails.
pub fn derive_skipped_region_id(record: &SkippedRegion) -> Result<FactId, FactIdentityRecipeError> {
    derive_skipped_region_id_with_checkpoint(record, || true)
}

/// Derives a skipped-region ID with checkpoints around recipe allocations.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if a checkpoint stops work or canonical
/// JSON encoding fails.
pub fn derive_skipped_region_id_with_checkpoint(
    record: &SkippedRegion,
    checkpoint: impl FnMut() -> bool,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_typed_fact_id_with_checkpoint(SKIPPED_REGION_FACT_DOMAIN, record, checkpoint)
        .map_err(|_| FactIdentityRecipeError)
}

/// Derives a diagnostic ID from every typed semantic field except `id`.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if canonical JSON encoding fails.
pub fn derive_diagnostic_record_id(
    record: &DiagnosticRecord,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_diagnostic_record_id_with_checkpoint(record, || true)
}

/// Derives a diagnostic ID with checkpoints around recipe allocations.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if a checkpoint stops work or canonical
/// JSON encoding fails.
pub fn derive_diagnostic_record_id_with_checkpoint(
    record: &DiagnosticRecord,
    checkpoint: impl FnMut() -> bool,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_typed_fact_id_with_checkpoint(DIAGNOSTIC_FACT_DOMAIN, record, checkpoint)
        .map_err(|_| FactIdentityRecipeError)
}

/// Stable identity label for one closed common entity kind.
#[must_use]
pub const fn entity_kind_identity_label(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Repository => "repository",
        EntityKind::Worktree => "worktree",
        EntityKind::Package => "package",
        EntityKind::BuildTarget => "build-target",
        EntityKind::Directory => "directory",
        EntityKind::File => "file",
        EntityKind::Module => "module",
        EntityKind::Namespace => "namespace",
        EntityKind::Class => "class",
        EntityKind::Struct => "struct",
        EntityKind::Enum => "enum",
        EntityKind::Union => "union",
        EntityKind::TypeAlias => "type-alias",
        EntityKind::Trait => "trait",
        EntityKind::Interface => "interface",
        EntityKind::Protocol => "protocol",
        EntityKind::Function => "function",
        EntityKind::Method => "method",
        EntityKind::Constructor => "constructor",
        EntityKind::Closure => "closure",
        EntityKind::Field => "field",
        EntityKind::Property => "property",
        EntityKind::Constant => "constant",
        EntityKind::Variable => "variable",
        EntityKind::Parameter => "parameter",
        EntityKind::TypeParameter => "type-parameter",
        EntityKind::Import => "import",
        EntityKind::Export => "export",
        EntityKind::Route => "route",
        EntityKind::Service => "service",
        EntityKind::MessageTopic => "message-topic",
        EntityKind::DatabaseObject => "database-object",
        EntityKind::Test => "test",
        EntityKind::ConfigurationKey => "configuration-key",
        EntityKind::StyleRule => "style-rule",
        EntityKind::Keyframes => "keyframes",
        EntityKind::MarkupElement => "markup-element",
        EntityKind::MarkupAttribute => "markup-attribute",
        EntityKind::Event => "event",
        EntityKind::ErrorDeclaration => "error-declaration",
        EntityKind::Modifier => "modifier",
        EntityKind::DocumentSection => "document-section",
        EntityKind::LinkDefinition => "link-definition",
        EntityKind::StatementLabel => "statement-label",
        EntityKind::Commit => "commit",
        EntityKind::Change => "change",
        EntityKind::CommunityView => "community-view",
        EntityKind::ExternalSymbol => "external-symbol",
    }
}

fn identity_claim_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    namespace: &str,
    payload: String,
    provenance: FactId,
    source: SourceRef,
    subject: FactRef,
) -> Result<ExtensionEnvelope, IdentityClaimError> {
    identity_claim_envelope_with_encoding(
        repository,
        generation,
        (namespace, IDENTITY_CLAIM_VERSION),
        payload,
        provenance,
        source,
        subject,
    )
}

fn identity_claim_envelope_with_encoding(
    repository: RepositoryId,
    generation: GenerationId,
    (namespace, version): (&str, &str),
    payload: String,
    provenance: FactId,
    source: SourceRef,
    subject: FactRef,
) -> Result<ExtensionEnvelope, IdentityClaimError> {
    let mut envelope = ExtensionEnvelope {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        namespace: namespace.to_owned(),
        version: version.to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation: vec![subject],
        },
    };
    envelope.id = derive_claim_envelope_id(&envelope)?;
    Ok(envelope)
}

fn require_claim_envelope(
    envelope: &ExtensionEnvelope,
    namespace: &str,
) -> Result<(), IdentityClaimError> {
    let supported_version = envelope.version == IDENTITY_CLAIM_VERSION
        || (namespace == SYMBOL_IDENTITY_CLAIM_NAMESPACE
            && (envelope.version == COMPACT_SYMBOL_IDENTITY_CLAIM_VERSION
                || envelope.version == POSITIONAL_SYMBOL_IDENTITY_CLAIM_VERSION
                || envelope.version == DENSE_SYMBOL_IDENTITY_CLAIM_VERSION));
    if envelope.namespace != namespace
        || !supported_version
        || envelope.criticality != ExtensionCriticality::Noncritical
    {
        return Err(IdentityClaimError::UnsupportedEnvelope);
    }
    Ok(())
}

fn require_claim_evidence(
    envelope: &ExtensionEnvelope,
    subject: FactRef,
) -> Result<&SourceRef, IdentityClaimError> {
    let source = envelope
        .evidence
        .source
        .as_ref()
        .ok_or(IdentityClaimError::EnvelopeMismatch)?;
    if source.repository() != envelope.repository
        || source.generation() != envelope.generation
        || envelope.evidence.derivation.as_slice() != [subject]
    {
        return Err(IdentityClaimError::EnvelopeMismatch);
    }
    Ok(source)
}

fn require_envelope_id_with_checkpoint(
    envelope: &ExtensionEnvelope,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<(), IdentityClaimError> {
    if derive_claim_envelope_id_with_checkpoint(envelope, checkpoint)? == envelope.id {
        Ok(())
    } else {
        Err(IdentityClaimError::EnvelopeMismatch)
    }
}

fn derive_claim_envelope_id(envelope: &ExtensionEnvelope) -> Result<FactId, IdentityClaimError> {
    derive_claim_envelope_id_with_checkpoint(envelope, &mut || true)
}

fn derive_claim_envelope_id_with_checkpoint(
    envelope: &ExtensionEnvelope,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<FactId, IdentityClaimError> {
    derive_typed_fact_id_with_checkpoint(
        "rootlight.identity-claim-envelope/v1",
        envelope,
        checkpoint,
    )
    .map_err(|error| match error {
        FactRecipeFailure::Encoding => IdentityClaimError::MalformedPayload,
        FactRecipeFailure::Interrupted => IdentityClaimError::Interrupted,
    })
}

fn decode_canonical_payload<T>(
    payload: &str,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<T, IdentityClaimError>
where
    T: Serialize + for<'de> Deserialize<'de>,
{
    let source = ClaimCheckpointReader::new(payload.as_bytes(), &mut checkpoint)?;
    let mut reader = BufReader::with_capacity(4 * 1024, source);
    let mut deserializer = serde_json::Deserializer::from_reader(&mut reader);
    let value = T::deserialize(&mut deserializer);
    let complete = value.is_ok() && deserializer.end().is_ok();
    drop(deserializer);
    let interrupted = reader.get_ref().interrupted;
    drop(reader);
    if interrupted {
        return Err(IdentityClaimError::Interrupted);
    }
    let value = value.map_err(|_| IdentityClaimError::MalformedPayload)?;
    if !complete {
        return Err(IdentityClaimError::MalformedPayload);
    }

    if !checkpoint() {
        return Err(IdentityClaimError::Interrupted);
    }
    let mut canonical = Vec::with_capacity(payload.len());
    {
        let mut writer = ClaimCheckpointWriter::new(&mut canonical, &mut checkpoint)?;
        if serde_json::to_writer(&mut writer, &value).is_err() {
            return if writer.interrupted {
                Err(IdentityClaimError::Interrupted)
            } else {
                Err(IdentityClaimError::MalformedPayload)
            };
        }
        writer.check()?;
    }
    if canonical != payload.as_bytes() {
        return Err(IdentityClaimError::NoncanonicalPayload);
    }
    Ok(value)
}

struct ClaimCheckpointReader<'input, 'checkpoint, F> {
    input: &'input [u8],
    position: usize,
    checkpoint: &'checkpoint mut F,
    interrupted: bool,
}

impl<'input, 'checkpoint, F> ClaimCheckpointReader<'input, 'checkpoint, F>
where
    F: FnMut() -> bool,
{
    fn new(
        input: &'input [u8],
        checkpoint: &'checkpoint mut F,
    ) -> Result<Self, IdentityClaimError> {
        if !checkpoint() {
            return Err(IdentityClaimError::Interrupted);
        }
        Ok(Self {
            input,
            position: 0,
            checkpoint,
            interrupted: false,
        })
    }
}

impl<F> Read for ClaimCheckpointReader<'_, '_, F>
where
    F: FnMut() -> bool,
{
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.interrupted {
            return Err(checkpoint_io_error());
        }
        if self.position >= self.input.len() {
            return Ok(0);
        }
        if !(self.checkpoint)() {
            self.interrupted = true;
            return Err(checkpoint_io_error());
        }
        let length = buffer
            .len()
            .min(4 * 1024)
            .min(self.input.len() - self.position);
        buffer[..length].copy_from_slice(&self.input[self.position..self.position + length]);
        self.position += length;
        Ok(length)
    }
}

struct ClaimCheckpointWriter<'output, 'checkpoint, F> {
    output: &'output mut Vec<u8>,
    checkpoint: &'checkpoint mut F,
    interrupted: bool,
}

impl<'output, 'checkpoint, F> ClaimCheckpointWriter<'output, 'checkpoint, F>
where
    F: FnMut() -> bool,
{
    fn new(
        output: &'output mut Vec<u8>,
        checkpoint: &'checkpoint mut F,
    ) -> Result<Self, IdentityClaimError> {
        let mut writer = Self {
            output,
            checkpoint,
            interrupted: false,
        };
        writer.check()?;
        Ok(writer)
    }

    fn check(&mut self) -> Result<(), IdentityClaimError> {
        if self.interrupted {
            return Err(IdentityClaimError::Interrupted);
        }
        if (self.checkpoint)() {
            Ok(())
        } else {
            self.interrupted = true;
            Err(IdentityClaimError::Interrupted)
        }
    }
}

impl<F> Write for ClaimCheckpointWriter<'_, '_, F>
where
    F: FnMut() -> bool,
{
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut written = 0;
        for chunk in buffer.chunks(4 * 1024) {
            self.check().map_err(|_| checkpoint_io_error())?;
            self.output.extend_from_slice(chunk);
            written += chunk.len();
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn checkpoint_io_error() -> io::Error {
    // Standard I/O adapters automatically retry `Interrupted`; cancellation
    // must escape serde so the public decoder can restore its typed error.
    io::Error::other("identity claim checkpoint")
}

enum FactRecipeFailure {
    Encoding,
    Interrupted,
}

fn derive_typed_fact_id_with_checkpoint<T>(
    domain: &str,
    record: &T,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<FactId, FactRecipeFailure>
where
    T: Serialize,
{
    require_recipe_checkpoint(&mut checkpoint)?;
    let mut value = serde_json::to_value(record).map_err(|_| FactRecipeFailure::Encoding)?;
    require_recipe_checkpoint(&mut checkpoint)?;
    let object = value.as_object_mut().ok_or(FactRecipeFailure::Encoding)?;
    if object.remove("id").is_none() {
        return Err(FactRecipeFailure::Encoding);
    }
    require_recipe_checkpoint(&mut checkpoint)?;
    let bytes = serde_json::to_vec(&value).map_err(|_| FactRecipeFailure::Encoding)?;
    require_recipe_checkpoint(&mut checkpoint)?;
    let id = derive_fact(domain, &bytes).id();
    require_recipe_checkpoint(&mut checkpoint)?;
    Ok(id)
}

fn require_recipe_checkpoint(
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<(), FactRecipeFailure> {
    if checkpoint() {
        Ok(())
    } else {
        Err(FactRecipeFailure::Interrupted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceSpan;

    #[test]
    fn fact_recipe_stops_between_canonical_allocations() {
        #[derive(Serialize)]
        struct RecipeFixture {
            id: FactId,
            payload: String,
        }

        let fixture = RecipeFixture {
            id: FactId::from_bytes([1; 20]),
            payload: "x".repeat(16 * 1024),
        };
        let mut checkpoints = 0;
        let result = derive_typed_fact_id_with_checkpoint("test", &fixture, || {
            checkpoints += 1;
            checkpoints < 2
        });

        assert!(matches!(result, Err(FactRecipeFailure::Interrupted)));
        assert_eq!(checkpoints, 2);
    }

    #[test]
    fn canonical_symbol_signatures_ignore_only_presentation_whitespace() {
        assert_eq!(
            canonical_symbol_signature("( x : i32 )", 128),
            Some("(x:i32)".to_owned())
        );
        assert_ne!(
            canonical_symbol_signature("(x:i32)", 128),
            canonical_symbol_signature("(x:u64)", 128)
        );
        assert_eq!(canonical_symbol_signature(" ", 128), None);
        assert_eq!(canonical_symbol_signature("(x)", 2), None);
    }

    #[test]
    fn rust_impl_scope_identity_ignores_presentation_and_preserves_trait_context() {
        let compact = canonical_rust_impl_scope("Demo<T>", None, 128)
            .expect("compact scope is bounded")
            .expect("compact scope is valid");
        let spaced = canonical_rust_impl_scope("Demo < T >", None, 128)
            .expect("spaced scope is bounded")
            .expect("spaced scope is valid");
        assert_eq!(compact.header(), spaced.header());
        assert_eq!(
            derive_rust_impl_scope_identity(None, compact.header())
                .expect("compact scope identity derives"),
            derive_rust_impl_scope_identity(None, spaced.header())
                .expect("spaced scope identity derives")
        );

        let trait_impl = canonical_rust_impl_scope("Demo<T>", Some("Display"), 128)
            .expect("trait scope is bounded")
            .expect("trait scope is valid");
        assert_ne!(compact.header(), trait_impl.header());
    }

    #[test]
    fn positional_symbol_claim_preserves_recipe_with_bounded_signature_overhead() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let file = FileId::from_bytes([3; 20]);
        let provenance = FactId::from_bytes([4; 20]);
        let container_identity = vec![0xff, 0, 0x80, 3];
        let signature_discriminator =
            format!("(value:\"λ\\quoted\",{})", "x".repeat(2048)).into_bytes();
        let build_context_discriminator = vec![0xff; 32];
        let expected_symbol = derive_symbol(SymbolIdentity {
            repository,
            language: "rust",
            semantic_kind: "function",
            container_identity: &container_identity,
            declared_identity: "fixture",
            signature_discriminator: &signature_discriminator,
            build_context_discriminator: &build_context_discriminator,
        })
        .id();
        let claim = SymbolIdentityClaim {
            symbol: expected_symbol,
            repository,
            language: "rust".to_owned(),
            kind: EntityKind::Function,
            container: Some(ContainerRef::File(file)),
            container_identity,
            declared_identity: "fixture".to_owned(),
            signature_discriminator,
            build_context_discriminator,
        };
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 0, 7).expect("fixture source span is valid"),
            rootlight_ids::content_hash(b"fixture"),
            None,
        );
        let envelope =
            new_symbol_identity_claim_envelope(&claim, generation, provenance, source.clone())
                .expect("symbol claim envelope is valid");

        assert_eq!(
            decode_symbol_identity_claim_envelope(&envelope)
                .expect("symbol claim round trip is lossless"),
            claim
        );
        assert_eq!(claim.derived_symbol(), expected_symbol);
        assert_eq!(envelope.namespace, SYMBOL_IDENTITY_CLAIM_NAMESPACE);
        assert_eq!(envelope.criticality, ExtensionCriticality::Noncritical);
        assert_eq!(envelope.repository, repository);
        assert_eq!(envelope.generation, generation);
        assert_eq!(envelope.provenance, provenance);
        assert_eq!(envelope.evidence.source, Some(source));
        assert_eq!(
            envelope.evidence.derivation,
            vec![FactRef::Entity(expected_symbol)]
        );
        assert!(
            envelope.payload.len() <= claim.signature_discriminator.len() + 512,
            "signature-like payload must have bounded overhead: {} bytes for {} signature bytes",
            envelope.payload.len(),
            claim.signature_discriminator.len()
        );
        assert_eq!(envelope.version, "1.3");
        let fields: serde_json::Value =
            serde_json::from_str(&envelope.payload).expect("positional payload is JSON");
        assert_eq!(fields.as_array().expect("payload is positional").len(), 7);
        let compact = compact_symbol_claim_envelope(&claim, &envelope);
        assert!(envelope.payload.len() < compact.payload.len());
        assert_ne!(envelope.id, compact.id);
    }

    #[test]
    fn compact_symbol_claim_round_trips_all_discriminator_bytes() {
        for bytes in [
            Vec::new(),
            b"signature".to_vec(),
            "λ\0\n\r\t\"\\\u{feff}".as_bytes().to_vec(),
            (0..=u8::MAX).collect(),
            vec![0xf0, 0x9f, 0x92],
        ] {
            let (claim, envelope) = symbol_claim_fixture(bytes.clone());
            let wire: PositionalSymbolIdentityClaim =
                serde_json::from_str(&envelope.payload).expect("positional wire payload decodes");
            let expected = shortest_discriminator_fixture(&bytes);
            assert_eq!(wire.3, expected);
            assert_eq!(wire.5, expected);
            assert_eq!(wire.6, expected);
            for carrier in symbol_claim_carriers(&claim, &envelope) {
                let decoded = decode_symbol_identity_claim_envelope(&carrier)
                    .expect("every discriminator byte survives");
                assert_eq!(decoded, claim);
                assert_eq!(decoded.derived_symbol(), claim.symbol);
            }
        }
    }

    #[test]
    fn symbol_claim_decoder_accepts_all_canonical_carrier_versions() {
        let (claim, positional) = symbol_claim_fixture("λ\0\"\\signature".as_bytes().to_vec());
        let carriers = symbol_claim_carriers(&claim, &positional);
        for (carrier, version) in carriers.iter().zip(["1.0", "1.1", "1.2", "1.3"]) {
            assert_eq!(carrier.version, version);
            let decoded = decode_symbol_identity_claim_envelope(carrier)
                .expect("canonical carrier preserves the public claim");
            assert_eq!(decoded, claim);
            assert_eq!(decoded.derived_symbol(), claim.symbol);
        }
        assert_ne!(carriers[0].id, carriers[1].id);
        assert_ne!(carriers[0].id, carriers[2].id);
        assert_ne!(carriers[1].id, carriers[2].id);
    }

    #[test]
    fn symbol_claim_decoder_rejects_noncanonical_and_malformed_discriminators() {
        let (claim, envelope) = symbol_claim_fixture(vec![0xff]);
        for (encoded, expected) in [
            ("h:", IdentityClaimError::NoncanonicalPayload),
            ("h:00", IdentityClaimError::NoncanonicalPayload),
            ("h:cebb", IdentityClaimError::NoncanonicalPayload),
            ("h:FF", IdentityClaimError::NoncanonicalPayload),
            ("h:f", IdentityClaimError::MalformedPayload),
            ("h:fg", IdentityClaimError::MalformedPayload),
            ("text", IdentityClaimError::MalformedPayload),
            ("b:ff", IdentityClaimError::MalformedPayload),
        ] {
            let mut compact =
                CompactSymbolIdentityClaim::from_claim(&claim).expect("valid claim encodes");
            compact.signature_discriminator = encoded.to_owned();
            let mut positional =
                PositionalSymbolIdentityClaim::from_claim(&claim).expect("valid claim encodes");
            positional.5 = encoded.to_owned();
            for (version, payload) in [
                ("1.1", serde_json::to_string(&compact).unwrap()),
                ("1.2", serde_json::to_string(&positional).unwrap()),
            ] {
                let mut malformed = envelope.clone();
                malformed.version = version.to_owned();
                malformed.payload = payload;
                refresh_claim_envelope_id(&mut malformed);
                assert_eq!(
                    decode_symbol_identity_claim_envelope(&malformed),
                    Err(expected),
                    "version {version}, encoded discriminator {encoded:?}"
                );
            }
        }
    }

    #[test]
    fn symbol_claim_decoder_enforces_canonical_json_in_all_versions() {
        let (claim, positional) = symbol_claim_fixture("λ".as_bytes().to_vec());
        for envelope in symbol_claim_carriers(&claim, &positional) {
            let mut spaced = envelope.clone();
            spaced.payload.push(' ');
            spaced.id = derive_claim_envelope_id(&spaced).expect("fixture identity derives");
            assert_eq!(
                decode_symbol_identity_claim_envelope(&spaced),
                Err(IdentityClaimError::NoncanonicalPayload)
            );

            if matches!(envelope.version.as_str(), "1.2" | "1.3") {
                continue;
            }
            let mut unknown_field = envelope.clone();
            unknown_field.payload.pop();
            unknown_field.payload.push_str(",\"extra\":0}");
            unknown_field.id =
                derive_claim_envelope_id(&unknown_field).expect("fixture identity derives");
            assert_eq!(
                decode_symbol_identity_claim_envelope(&unknown_field),
                Err(IdentityClaimError::MalformedPayload)
            );

            let mut duplicate_field = envelope.clone();
            duplicate_field.payload.pop();
            duplicate_field.payload.push_str(",\"language\":\"rust\"}");
            duplicate_field.id =
                derive_claim_envelope_id(&duplicate_field).expect("fixture identity derives");
            assert_eq!(
                decode_symbol_identity_claim_envelope(&duplicate_field),
                Err(IdentityClaimError::MalformedPayload)
            );
        }
        let (_, mut escaped) = symbol_claim_fixture("λ".as_bytes().to_vec());
        escaped.payload = escaped.payload.replace('λ', "\\u03bb");
        escaped.id = derive_claim_envelope_id(&escaped).expect("fixture identity derives");
        assert_eq!(
            decode_symbol_identity_claim_envelope(&escaped),
            Err(IdentityClaimError::NoncanonicalPayload)
        );
    }

    #[test]
    fn symbol_claim_decoder_rejects_cross_version_and_mismatched_envelopes() {
        let (claim, positional) = symbol_claim_fixture(vec![0xff; 8]);
        for carrier in symbol_claim_carriers(&claim, &positional) {
            for version in ["1.0", "1.1", "1.2", "1.3"] {
                if version == carrier.version {
                    continue;
                }
                let mut envelope = carrier.clone();
                envelope.version = version.to_owned();
                refresh_claim_envelope_id(&mut envelope);
                assert_eq!(
                    decode_symbol_identity_claim_envelope(&envelope),
                    Err(if carrier.version == "1.2" && version == "1.3" {
                        IdentityClaimError::NoncanonicalPayload
                    } else {
                        IdentityClaimError::MalformedPayload
                    })
                );
            }
        }

        for version in ["", "1.4", "2.0"] {
            let mut envelope = positional.clone();
            envelope.version = version.to_owned();
            assert_eq!(
                decode_symbol_identity_claim_envelope(&envelope),
                Err(IdentityClaimError::UnsupportedEnvelope)
            );
        }

        let mut unsupported = positional.clone();
        unsupported.namespace = FILE_IDENTITY_CLAIM_NAMESPACE.to_owned();
        assert_eq!(
            decode_symbol_identity_claim_envelope(&unsupported),
            Err(IdentityClaimError::UnsupportedEnvelope)
        );
        unsupported = positional.clone();
        unsupported.criticality = ExtensionCriticality::Critical;
        assert_eq!(
            decode_symbol_identity_claim_envelope(&unsupported),
            Err(IdentityClaimError::UnsupportedEnvelope)
        );

        let mut mismatched = positional.clone();
        mismatched.id = FactId::from_bytes([9; 20]);
        assert_eq!(
            decode_symbol_identity_claim_envelope(&mismatched),
            Err(IdentityClaimError::EnvelopeMismatch)
        );
        mismatched = positional.clone();
        mismatched.evidence.derivation.clear();
        mismatched.id = derive_claim_envelope_id(&mismatched).expect("fixture identity derives");
        assert_eq!(
            decode_symbol_identity_claim_envelope(&mismatched),
            Err(IdentityClaimError::EnvelopeMismatch)
        );
        mismatched = positional;
        mismatched.evidence.derivation = vec![FactRef::Entity(SymbolId::from_bytes([9; 20]))];
        mismatched.id = derive_claim_envelope_id(&mismatched).expect("fixture identity derives");
        assert_eq!(
            decode_symbol_identity_claim_envelope(&mismatched),
            Err(IdentityClaimError::EnvelopeMismatch)
        );
    }

    #[test]
    fn positional_symbol_claim_rejects_wrong_tuple_shapes_and_types() {
        let (_, envelope) = symbol_claim_fixture(b"signature".to_vec());
        let fields: serde_json::Value = serde_json::from_str(&envelope.payload).unwrap();
        let fields = fields.as_array().unwrap();
        let mut missing = fields.clone();
        missing.pop();
        let mut extra = fields.clone();
        extra.push(serde_json::Value::Null);
        let mut wrong_kind = fields.clone();
        wrong_kind[1] = serde_json::Value::Null;
        let mut wrong_container = fields.clone();
        wrong_container[2] = serde_json::Value::String("file".to_owned());
        let mut numeric_discriminator = fields.clone();
        numeric_discriminator[3] = serde_json::json!([1, 2]);
        let mut reordered = fields.clone();
        reordered.swap(0, 1);
        let mut wrong_identity = fields.clone();
        wrong_identity[4] = serde_json::Value::Bool(false);
        for fields in [
            missing,
            extra,
            wrong_kind,
            wrong_container,
            numeric_discriminator,
            reordered,
            wrong_identity,
        ] {
            let mut malformed = envelope.clone();
            malformed.payload = serde_json::to_string(&fields).unwrap();
            refresh_claim_envelope_id(&mut malformed);
            assert_eq!(
                decode_symbol_identity_claim_envelope(&malformed),
                Err(IdentityClaimError::MalformedPayload)
            );
        }
        for payload in ["{}".to_owned(), format!("{}null", envelope.payload)] {
            let mut malformed = envelope.clone();
            malformed.payload = payload;
            refresh_claim_envelope_id(&mut malformed);
            assert_eq!(
                decode_symbol_identity_claim_envelope(&malformed),
                Err(IdentityClaimError::MalformedPayload)
            );
        }
    }

    #[test]
    fn symbol_claim_decoder_requires_exact_owned_entity_evidence() {
        let (claim, positional) = symbol_claim_fixture(b"signature".to_vec());
        let other_symbol = SymbolId::from_bytes([9; 20]);
        let file = FileId::from_bytes([3; 20]);
        for carrier in symbol_claim_carriers(&claim, &positional) {
            for derivation in [
                Vec::new(),
                vec![FactRef::File(file)],
                vec![FactRef::Fact(carrier.provenance)],
                vec![FactRef::Entity(other_symbol)],
                vec![FactRef::Entity(claim.symbol); 2],
                vec![FactRef::Entity(claim.symbol), FactRef::Entity(other_symbol)],
                vec![FactRef::Entity(claim.symbol), FactRef::File(file)],
            ] {
                let mut mismatched = carrier.clone();
                mismatched.evidence.derivation = derivation;
                refresh_claim_envelope_id(&mut mismatched);
                assert_eq!(
                    decode_symbol_identity_claim_envelope(&mismatched),
                    Err(IdentityClaimError::EnvelopeMismatch),
                    "version {} must reject ambiguous or wrong entity evidence",
                    carrier.version
                );
            }
            let source = carrier.evidence.source.as_ref().unwrap();
            for source in [
                None,
                Some(SourceRef::new(
                    RepositoryId::from_bytes([9; 16]),
                    source.generation(),
                    source.span(),
                    source.content_hash(),
                    source.line_hint(),
                )),
                Some(SourceRef::new(
                    source.repository(),
                    GenerationId::from_bytes([9; 20]),
                    source.span(),
                    source.content_hash(),
                    source.line_hint(),
                )),
            ] {
                let mut mismatched = carrier.clone();
                mismatched.evidence.source = source;
                refresh_claim_envelope_id(&mut mismatched);
                assert_eq!(
                    decode_symbol_identity_claim_envelope(&mismatched),
                    Err(IdentityClaimError::EnvelopeMismatch)
                );
            }
        }
    }

    #[test]
    fn positional_symbol_claim_requires_ownership_before_payload_reconstruction() {
        let (_, mut envelope) = symbol_claim_fixture(b"signature".to_vec());
        envelope.payload = "[".to_owned();
        refresh_claim_envelope_id(&mut envelope);
        assert_eq!(
            decode_symbol_identity_claim_envelope(&envelope),
            Err(IdentityClaimError::MalformedPayload)
        );
        let mut missing_entity = envelope.clone();
        missing_entity.evidence.derivation.clear();
        refresh_claim_envelope_id(&mut missing_entity);
        assert_eq!(
            decode_symbol_identity_claim_envelope(&missing_entity),
            Err(IdentityClaimError::EnvelopeMismatch)
        );
        envelope.evidence.source = None;
        refresh_claim_envelope_id(&mut envelope);
        assert_eq!(
            decode_symbol_identity_claim_envelope(&envelope),
            Err(IdentityClaimError::EnvelopeMismatch)
        );
    }

    #[test]
    fn symbol_claim_decoder_rechecks_recipe_after_carrier_id_is_refreshed() {
        let (claim, positional) = symbol_claim_fixture(b"signature".to_vec());
        for field in 0..6 {
            let mut tampered = claim.clone();
            match field {
                0 => tampered.language = "other".to_owned(),
                1 => tampered.kind = EntityKind::ExternalSymbol,
                2 => tampered.container_identity.push(b'/'),
                3 => tampered.declared_identity.push('_'),
                4 => tampered.signature_discriminator.push(b'/'),
                5 => tampered.build_context_discriminator.push(b'/'),
                _ => unreachable!("fixture changes exactly the six recipe fields"),
            }
            let mut current = positional.clone();
            current.payload = serde_json::to_string(
                &PositionalSymbolIdentityClaim::from_claim(&tampered).unwrap(),
            )
            .unwrap();
            refresh_claim_envelope_id(&mut current);
            for carrier in symbol_claim_carriers(&tampered, &current) {
                assert_eq!(
                    decode_symbol_identity_claim_envelope(&carrier),
                    Err(IdentityClaimError::EnvelopeMismatch),
                    "version {}, recipe field {field}",
                    carrier.version
                );
            }
        }
        for mut carrier in symbol_claim_carriers(&claim, &positional) {
            let repository = RepositoryId::from_bytes([9; 16]);
            let source = carrier.evidence.source.as_ref().unwrap();
            carrier.evidence.source = Some(SourceRef::new(
                repository,
                source.generation(),
                source.span(),
                source.content_hash(),
                source.line_hint(),
            ));
            carrier.repository = repository;
            refresh_claim_envelope_id(&mut carrier);
            assert_eq!(
                decode_symbol_identity_claim_envelope(&carrier),
                Err(IdentityClaimError::EnvelopeMismatch)
            );
        }
    }

    #[test]
    fn compact_discriminator_conversion_checks_each_bounded_chunk() {
        for encoded in [
            format!("u:{}", "λ".repeat(16 * 1024)),
            format!("h:{}ff", "61".repeat(16 * 1024)),
        ] {
            let mut checkpoints = 0;
            let expected = decode_claim_discriminator(&encoded, &mut || {
                checkpoints += 1;
                true
            })
            .expect("large discriminator decodes");
            assert!(expected.len() >= 16 * 1024);
            assert!(checkpoints > 4);

            for cutoff in 1..=checkpoints {
                let mut observed = 0;
                assert_eq!(
                    decode_claim_discriminator(&encoded, &mut || {
                        observed += 1;
                        observed < cutoff
                    }),
                    Err(IdentityClaimError::Interrupted)
                );
                assert_eq!(observed, cutoff);
            }
        }
    }

    #[test]
    fn padded_claim_base64_matches_independent_rfc4648_vectors() {
        for (bytes, expected) in [
            (b"".as_slice(), ""),
            (b"f".as_slice(), "Zg=="),
            (b"fo".as_slice(), "Zm8="),
            (b"foo".as_slice(), "Zm9v"),
            (b"foob".as_slice(), "Zm9vYg=="),
            (b"fooba".as_slice(), "Zm9vYmE="),
            (b"foobar".as_slice(), "Zm9vYmFy"),
            ([0xff].as_slice(), "/w=="),
            ([0xff, 0xff].as_slice(), "//8="),
            ([0xff, 0xff, 0xff].as_slice(), "////"),
        ] {
            assert_eq!(
                encode_claim_base64(bytes, &mut || true).unwrap(),
                format!("b:{expected}")
            );
            assert_eq!(
                decode_claim_base64(expected.as_bytes(), &mut || true).unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn dense_discriminators_choose_actual_json_cost_and_legacy_tie_priority() {
        for (bytes, expected) in [
            (b"".as_slice(), "u:"),
            (b"abc".as_slice(), "u:abc"),
            (b"\"".as_slice(), "u:\""),
            (b"\\".as_slice(), "u:\\"),
            (b"\n".as_slice(), "u:\n"),
            (b"\0".as_slice(), "h:00"),
            (b"\0\0".as_slice(), "h:0000"),
            (b"\0\0\0".as_slice(), "b:AAAA"),
            ([0xff].as_slice(), "h:ff"),
            ([0xff, 0xff].as_slice(), "h:ffff"),
            ([0xff, 0xff, 0xff].as_slice(), "b:////"),
        ] {
            assert_eq!(
                encode_dense_claim_discriminator(bytes, &mut || true).unwrap(),
                expected
            );
            assert_eq!(
                decode_versioned_claim_discriminator(expected, &mut || true, true).unwrap(),
                bytes
            );
        }
        for text in [
            String::from_utf8((0..=127).collect()).unwrap(),
            "λ\0\n\r\t\"\\\u{feff}\u{2028}\u{2029}".to_owned(),
            format!("{}λ{}", "x".repeat(4095), "x".repeat(4096)),
        ] {
            assert_eq!(
                claim_json_text_length(&text, &mut || true).unwrap(),
                serde_json::to_string(&format!("u:{text}")).unwrap().len()
            );
            assert_eq!(
                encode_dense_claim_discriminator(text.as_bytes(), &mut || true).unwrap(),
                shortest_discriminator_fixture(text.as_bytes())
            );
        }
    }

    #[test]
    fn dense_claim_discriminators_preserve_each_byte_and_mixed_unicode_controls() {
        for byte in 0..=u8::MAX {
            let encoded = encode_dense_claim_discriminator(&[byte], &mut || true).unwrap();
            assert_eq!(
                decode_versioned_claim_discriminator(&encoded, &mut || true, true).unwrap(),
                [byte]
            );
        }
        for bytes in [
            (0..=u8::MAX).collect::<Vec<_>>(),
            "λ\0\n\r\t\"\\\u{feff}".as_bytes().to_vec(),
        ] {
            let (claim, envelope) = symbol_claim_fixture(bytes);
            assert_eq!(
                decode_symbol_identity_claim_envelope(&envelope).unwrap(),
                claim
            );
            assert_eq!(envelope.version, "1.3");
            assert_eq!(
                serde_json::from_str::<Vec<serde_json::Value>>(&envelope.payload)
                    .unwrap()
                    .len(),
                7
            );
        }
    }

    #[test]
    fn positional_legacy_binary_fixture_remains_exact_and_readable() {
        let (claim, dense) = symbol_claim_fixture(vec![0xff; 3]);
        let legacy = positional_symbol_claim_envelope(&claim, &dense);
        let expected = serde_json::to_string(&(
            claim.language.as_str(),
            claim.kind,
            claim.container,
            "h:ffffff",
            claim.declared_identity.as_str(),
            "h:ffffff",
            "h:ffffff",
        ))
        .unwrap();
        assert_eq!(legacy.version, "1.2");
        assert_eq!(legacy.payload, expected);
        assert_eq!(
            decode_symbol_identity_claim_envelope(&legacy).unwrap(),
            claim
        );
        let wire: PositionalSymbolIdentityClaim = serde_json::from_str(&dense.payload).unwrap();
        assert_eq!(
            (wire.3.as_str(), wire.5.as_str(), wire.6.as_str()),
            ("b:////", "b:////", "b:////")
        );
        assert_eq!(dense.evidence, legacy.evidence);
        assert_eq!(dense.provenance, legacy.provenance);
        assert_eq!(dense.repository, legacy.repository);
        assert_eq!(dense.generation, legacy.generation);
        assert_ne!(dense.id, legacy.id);

        let (text_claim, mut text_dense) = symbol_claim_fixture(b"plain".to_vec());
        let text_legacy = positional_symbol_claim_envelope(&text_claim, &text_dense);
        assert_eq!(text_legacy.payload, text_dense.payload);
        text_dense.version = "1.2".to_owned();
        refresh_claim_envelope_id(&mut text_dense);
        assert_eq!(
            decode_symbol_identity_claim_envelope(&text_dense).unwrap(),
            text_claim
        );
    }

    #[test]
    fn dense_base64_rejects_bad_padding_tail_bits_and_alphabets() {
        for content in [
            "A", "AA", "AAA", "A===", "====", "AA=A", "AA==AA==", "AB==", "AAB=", "AA-_", "AA\n=",
            "éé",
        ] {
            assert_eq!(
                decode_claim_base64(content.as_bytes(), &mut || true),
                Err(IdentityClaimError::MalformedPayload),
                "{content:?}"
            );
        }
        assert_eq!(
            claim_base64_length(usize::MAX),
            Err(IdentityClaimError::MalformedPayload)
        );
        let mut checkpoint = || true;
        let mut writer = ClaimJsonLengthWriter {
            length: usize::MAX,
            checkpoint: &mut checkpoint,
            interrupted: false,
        };
        assert!(writer.write(b"x").is_err());
        assert!(!writer.interrupted);
    }

    #[test]
    fn dense_claim_decoder_rejects_nonwinning_discriminator_carriers() {
        for encoded in [
            "u:\0", "b:AA==", "h:", "h:616263", "b:YWJj", "h:ffffff", "b:/w==", "h:FF",
        ] {
            assert_eq!(
                decode_versioned_claim_discriminator(encoded, &mut || true, true),
                Err(IdentityClaimError::NoncanonicalPayload),
                "{encoded:?}"
            );
        }
        assert_eq!(
            decode_versioned_claim_discriminator("h:00", &mut || true, true),
            Ok(vec![0])
        );
        assert_eq!(
            decode_claim_discriminator("h:00", &mut || true),
            Err(IdentityClaimError::NoncanonicalPayload)
        );
        let (claim, mut envelope) = symbol_claim_fixture(vec![0xff; 3]);
        let mut wire = PositionalSymbolIdentityClaim::from_dense_claim(&claim).unwrap();
        wire.5 = "h:ffffff".to_owned();
        envelope.payload = serde_json::to_string(&wire).unwrap();
        refresh_claim_envelope_id(&mut envelope);
        assert_eq!(
            decode_symbol_identity_claim_envelope(&envelope),
            Err(IdentityClaimError::NoncanonicalPayload)
        );
    }

    #[test]
    fn dense_discriminator_canonicalization_checks_every_bounded_stage() {
        for bytes in [
            "λ".repeat(8 * 1024).into_bytes(),
            vec![0xff; 16 * 1024],
            vec![0],
            Vec::new(),
        ] {
            let encoded = encode_dense_claim_discriminator(&bytes, &mut || true).unwrap();
            let mut checkpoints = 0;
            assert_eq!(
                decode_versioned_claim_discriminator(
                    &encoded,
                    &mut || {
                        checkpoints += 1;
                        true
                    },
                    true
                )
                .unwrap(),
                bytes
            );
            assert!(checkpoints > 2);
            for cutoff in 1..=checkpoints {
                let mut observed = 0;
                assert_eq!(
                    decode_versioned_claim_discriminator(
                        &encoded,
                        &mut || {
                            observed += 1;
                            observed < cutoff
                        },
                        true
                    ),
                    Err(IdentityClaimError::Interrupted)
                );
                assert_eq!(observed, cutoff);
            }
        }
        assert_eq!(
            encode_claim_base64(b"", &mut || false),
            Err(IdentityClaimError::Interrupted)
        );
        assert_eq!(
            decode_claim_base64(b"", &mut || false),
            Err(IdentityClaimError::Interrupted)
        );
    }

    #[test]
    fn hexadecimal_utf8_rejection_handles_boundary_spanning_code_points() {
        let text = format!("{}λ{}", "x".repeat(4095), "x".repeat(4096));
        let hex = text
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();

        assert_eq!(
            decode_claim_discriminator(&format!("h:{hex}"), &mut || true),
            Err(IdentityClaimError::NoncanonicalPayload)
        );
    }

    #[test]
    fn symbol_claim_decoder_stops_inside_all_carrier_payloads() {
        let (claim, positional) = symbol_claim_fixture(vec![b'x'; 16 * 1024]);
        for envelope in symbol_claim_carriers(&claim, &positional) {
            let mut checkpoints = 0;
            assert_eq!(
                decode_symbol_identity_claim_envelope_with_checkpoint(&envelope, || {
                    checkpoints += 1;
                    checkpoints < 3
                }),
                Err(IdentityClaimError::Interrupted)
            );
            assert_eq!(checkpoints, 3);
        }
    }

    #[test]
    fn symbol_claim_decoder_checks_every_stage_for_all_carriers() {
        let (claim, positional) = symbol_claim_fixture(vec![0xff; 8 * 1024]);
        for envelope in symbol_claim_carriers(&claim, &positional) {
            let mut checkpoints = 0;
            assert_eq!(
                decode_symbol_identity_claim_envelope_with_checkpoint(&envelope, || {
                    checkpoints += 1;
                    true
                })
                .unwrap(),
                claim
            );
            assert!(checkpoints > 4);
            for cutoff in 1..=checkpoints {
                let mut observed = 0;
                assert_eq!(
                    decode_symbol_identity_claim_envelope_with_checkpoint(&envelope, || {
                        observed += 1;
                        observed < cutoff
                    }),
                    Err(IdentityClaimError::Interrupted),
                    "version {}, checkpoint {cutoff}",
                    envelope.version
                );
                assert_eq!(observed, cutoff);
            }
        }
    }

    #[test]
    fn file_claim_writer_retains_the_canonical_version_one_payload() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let path_identity = b"fixture.rs".to_vec();
        let content_hash = rootlight_ids::content_hash(b"fixture");
        let claim = FileIdentityClaim {
            file: derive_file(FileIdentity {
                repository,
                path_identity: &path_identity,
            })
            .id(),
            repository,
            path: "fixture.rs".to_owned(),
            path_identity,
            content_hash,
            byte_length: 7,
        };
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(claim.file, 0, 7).expect("fixture source span is valid"),
            content_hash,
            None,
        );
        let mut envelope = new_file_identity_claim_envelope(
            &claim,
            generation,
            FactId::from_bytes([4; 20]),
            source,
        )
        .expect("file claim envelope is valid");

        assert_eq!(envelope.version, "1.0");
        assert_eq!(envelope.payload, serde_json::to_string(&claim).unwrap());
        assert_eq!(
            decode_file_identity_claim_envelope(&envelope).expect("file claim still decodes"),
            claim
        );
        for version in ["1.1", "1.2", "1.3"] {
            envelope.version = version.to_owned();
            assert_eq!(
                decode_file_identity_claim_envelope(&envelope),
                Err(IdentityClaimError::UnsupportedEnvelope)
            );
        }
    }

    fn symbol_claim_fixture(bytes: Vec<u8>) -> (SymbolIdentityClaim, ExtensionEnvelope) {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let file = FileId::from_bytes([3; 20]);
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository,
            language: "rust".to_owned(),
            kind: EntityKind::Function,
            container: Some(ContainerRef::File(file)),
            container_identity: bytes.clone(),
            declared_identity: "fixture".to_owned(),
            signature_discriminator: bytes.clone(),
            build_context_discriminator: bytes,
        };
        claim.symbol = claim.derived_symbol();
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 0, 7).expect("fixture source span is valid"),
            rootlight_ids::content_hash(b"fixture"),
            None,
        );
        let envelope = new_symbol_identity_claim_envelope(
            &claim,
            generation,
            FactId::from_bytes([4; 20]),
            source,
        )
        .expect("symbol claim envelope is valid");
        (claim, envelope)
    }

    fn legacy_symbol_claim_envelope(
        claim: &SymbolIdentityClaim,
        compact: &ExtensionEnvelope,
    ) -> ExtensionEnvelope {
        identity_claim_envelope(
            claim.repository,
            compact.generation,
            SYMBOL_IDENTITY_CLAIM_NAMESPACE,
            serde_json::to_string(claim).expect("legacy payload encodes"),
            compact.provenance,
            compact
                .evidence
                .source
                .clone()
                .expect("direct source exists"),
            FactRef::Entity(claim.symbol),
        )
        .expect("legacy envelope recipe remains valid")
    }

    fn compact_symbol_claim_envelope(
        claim: &SymbolIdentityClaim,
        positional: &ExtensionEnvelope,
    ) -> ExtensionEnvelope {
        // Serialize the retained 1.1 wire directly; a new writer must not define
        // the legacy fixture whose continued readability this test checks.
        identity_claim_envelope_with_encoding(
            claim.repository,
            positional.generation,
            (SYMBOL_IDENTITY_CLAIM_NAMESPACE, "1.1"),
            serde_json::to_string(&CompactSymbolIdentityClaim::from_claim(claim).unwrap()).unwrap(),
            positional.provenance,
            positional.evidence.source.clone().unwrap(),
            FactRef::Entity(claim.symbol),
        )
        .expect("compact envelope recipe remains valid")
    }

    fn symbol_claim_carriers(
        claim: &SymbolIdentityClaim,
        positional: &ExtensionEnvelope,
    ) -> [ExtensionEnvelope; 4] {
        [
            legacy_symbol_claim_envelope(claim, positional),
            compact_symbol_claim_envelope(claim, positional),
            positional_symbol_claim_envelope(claim, positional),
            positional.clone(),
        ]
    }

    fn positional_symbol_claim_envelope(
        claim: &SymbolIdentityClaim,
        dense: &ExtensionEnvelope,
    ) -> ExtensionEnvelope {
        // Version 1.2 keeps its own encoder; dense writers cannot redefine the
        // legacy fixture or silently admit shorter noncanonical alternatives.
        identity_claim_envelope_with_encoding(
            claim.repository,
            dense.generation,
            (SYMBOL_IDENTITY_CLAIM_NAMESPACE, "1.2"),
            serde_json::to_string(&PositionalSymbolIdentityClaim::from_claim(claim).unwrap())
                .unwrap(),
            dense.provenance,
            dense.evidence.source.clone().unwrap(),
            FactRef::Entity(claim.symbol),
        )
        .expect("positional legacy envelope recipe remains valid")
    }

    fn shortest_discriminator_fixture(bytes: &[u8]) -> String {
        let mut candidates = Vec::new();
        if let Ok(text) = std::str::from_utf8(bytes) {
            candidates.push(format!("u:{text}"));
        }
        candidates.push(format!(
            "h:{}",
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        candidates.push(encode_claim_base64(bytes, &mut || true).unwrap());
        candidates
            .into_iter()
            .enumerate()
            .min_by_key(|(priority, candidate)| {
                (serde_json::to_string(candidate).unwrap().len(), *priority)
            })
            .unwrap()
            .1
    }

    fn refresh_claim_envelope_id(envelope: &mut ExtensionEnvelope) {
        envelope.id = derive_claim_envelope_id(envelope).expect("fixture identity derives");
    }

    #[test]
    fn claim_decoder_stops_inside_a_large_payload() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let path_identity = vec![3; 16 * 1024];
        let file = derive_file(FileIdentity {
            repository,
            path_identity: &path_identity,
        })
        .id();
        let content_hash = rootlight_ids::content_hash(b"fixture");
        let claim = FileIdentityClaim {
            file,
            repository,
            path: "fixture.rs".to_owned(),
            path_identity,
            content_hash,
            byte_length: 7,
        };
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 0, 7).expect("fixture source span is valid"),
            content_hash,
            None,
        );
        let envelope = new_file_identity_claim_envelope(
            &claim,
            generation,
            FactId::from_bytes([4; 20]),
            source,
        )
        .expect("large claim envelope is valid");
        let mut checkpoints = 0;
        let result = decode_file_identity_claim_envelope_with_checkpoint(&envelope, || {
            checkpoints += 1;
            checkpoints < 3
        });

        assert_eq!(result, Err(IdentityClaimError::Interrupted));
        assert_eq!(checkpoints, 3);
    }
}
