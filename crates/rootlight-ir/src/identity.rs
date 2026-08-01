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
/// Version of the producer-neutral identity-claim payload.
pub const IDENTITY_CLAIM_VERSION: &str = "1.0";

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
    let payload = serde_json::to_string(claim).map_err(|_| IdentityClaimError::MalformedPayload)?;
    identity_claim_envelope(
        claim.repository,
        generation,
        SYMBOL_IDENTITY_CLAIM_NAMESPACE,
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
    let claim: SymbolIdentityClaim = decode_canonical_payload(&envelope.payload, &mut checkpoint)?;
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
    let mut envelope = ExtensionEnvelope {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        namespace: namespace.to_owned(),
        version: IDENTITY_CLAIM_VERSION.to_owned(),
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
    if envelope.namespace != namespace
        || envelope.version != IDENTITY_CLAIM_VERSION
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
