//! Stable, versioned identity primitives for Rootlight.
//!
//! IDs are binary internally and use strict lowercase, checksummed base32 at
//! public boundaries. Semantic identity never depends on storage rows, parser
//! nodes, memory addresses, wall time, or worker completion order.

#![forbid(unsafe_code)]

use std::{fmt, str::FromStr, sync::LazyLock};

use data_encoding::BASE32_NOPAD;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

const CHECKSUM_BYTES: usize = 4;
const REPOSITORY_CONTEXT: &str = "rootlight/repository/v1";
const GENERATION_CONTEXT: &str = "rootlight/generation/v1";
const SYMBOL_CONTEXT: &str = "rootlight/symbol/v1";
const FILE_CONTEXT: &str = "rootlight/file/v1";
const FACT_CONTEXT: &str = "rootlight/fact/v1";
const TEXT_CHECKSUM_CONTEXT: &str = "rootlight/id-text-checksum/v1";
static TEXT_CHECKSUM_HASHER: LazyLock<blake3::Hasher> =
    LazyLock::new(|| blake3::Hasher::new_derive_key(TEXT_CHECKSUM_CONTEXT));

/// The complete digest retained to detect collisions in compact public IDs.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdentityDigest([u8; 32]);

impl IdentityDigest {
    /// Creates a digest from its canonical bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the canonical digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for IdentityDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("IdentityDigest")
            .field(&Hex(&self.0))
            .finish()
    }
}

/// A deterministic compact identifier paired with its full collision digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedId<T> {
    id: T,
    digest: IdentityDigest,
}

impl<T: Copy> DerivedId<T> {
    /// Returns the compact public identifier.
    #[must_use]
    pub const fn id(&self) -> T {
        self.id
    }

    /// Returns the full digest used for collision detection.
    #[must_use]
    pub const fn digest(&self) -> IdentityDigest {
        self.digest
    }
}

/// Reports whether a compact-ID insertion is new, identical, or a collision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionDisposition {
    /// No record exists for the compact ID.
    Vacant,
    /// The compact ID and full digest identify the same semantic input.
    SameIdentity,
    /// The compact ID matches but the full digest differs.
    Collision,
}

/// Compares an incoming digest with the digest already stored for a compact ID.
#[must_use]
pub fn collision_disposition(
    stored: Option<IdentityDigest>,
    incoming: IdentityDigest,
) -> CollisionDisposition {
    match stored {
        None => CollisionDisposition::Vacant,
        Some(stored) if stored == incoming => CollisionDisposition::SameIdentity,
        Some(_) => CollisionDisposition::Collision,
    }
}

macro_rules! define_stable_id {
    ($name:ident, $size:expr, $prefix:literal, $pattern:literal, $summary:literal) => {
        #[doc = $summary]
        #[repr(transparent)]
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; $size]);

        #[cfg(feature = "schema")]
        impl schemars::JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(
                _generator: &mut schemars::SchemaGenerator,
            ) -> schemars::Schema {
                schemars::json_schema!({
                    "type": "string",
                    "pattern": $pattern,
                })
            }
        }

        impl $name {
            /// Creates the identifier from canonical binary bytes.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; $size]) -> Self {
                Self(bytes)
            }

            /// Returns the canonical binary bytes.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; $size] {
                &self.0
            }

            fn with_text<T>(&self, consume: impl FnOnce(&str) -> T) -> T {
                // Fixed ID families bound both buffers. Recovery serializes the
                // same binary references repeatedly, so text conversion must not
                // allocate an intermediate string for each reference.
                let mut payload = [0_u8; $size + CHECKSUM_BYTES];
                payload[..$size].copy_from_slice(&self.0);
                payload[$size..].copy_from_slice(&text_checksum($prefix, &self.0));
                let mut encoded =
                    [0_u8; $prefix.len() + (($size + CHECKSUM_BYTES) * 8).div_ceil(5)];
                encoded[..$prefix.len()].copy_from_slice($prefix.as_bytes());
                let suffix = &mut encoded[$prefix.len()..];
                BASE32_NOPAD.encode_mut(&payload, suffix);
                suffix.make_ascii_lowercase();
                consume(std::str::from_utf8(&encoded).expect("ID alphabet and prefix are ASCII"))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.with_text(|text| formatter.pad(text))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.with_text(|text| {
                    formatter
                        .debug_tuple(stringify!($name))
                        .field(&text)
                        .finish()
                })
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(input: &str) -> Result<Self, Self::Err> {
                decode_text::<$size>(
                    $prefix,
                    input,
                    &mut [0_u8; (($size + CHECKSUM_BYTES) * 8).div_ceil(5)],
                    &mut [0_u8; $size + CHECKSUM_BYTES],
                )
                .map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                self.with_text(|text| serializer.serialize_str(text))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let input = String::deserialize(deserializer)?;
                input.parse().map_err(de::Error::custom)
            }
        }
    };
}

define_stable_id!(
    RepositoryId,
    16,
    "repo1_",
    "^repo1_[a-z2-7]{32}$",
    "A stable repository identity independent of mutable remote URLs."
);
define_stable_id!(
    GenerationId,
    20,
    "gen1_",
    "^gen1_[a-z2-7]{39}$",
    "An immutable generation identity derived from all semantic build inputs."
);
define_stable_id!(
    SymbolId,
    20,
    "sym1_",
    "^sym1_[a-z2-7]{39}$",
    "A semantic symbol identity independent of storage and parser-local IDs."
);
define_stable_id!(
    FileId,
    20,
    "file1_",
    "^file1_[a-z2-7]{39}$",
    "A repository-scoped identity for a canonical path identity policy."
);
define_stable_id!(
    FactId,
    20,
    "fact1_",
    "^fact1_[a-z2-7]{39}$",
    "A deterministic identity for a canonical semantic fact payload."
);
define_stable_id!(
    OperationId,
    16,
    "op1_",
    "^op1_[a-z2-7]{32}$",
    "An opaque operation handle encoded with the stable public ID codec."
);
define_stable_id!(
    ContentHash,
    32,
    "b3_",
    "^b3_[a-z2-7]{58}$",
    "A full BLAKE3 digest for immutable content."
);

/// Canonical fields used to derive a [`GenerationId`].
#[derive(Debug, Clone, Copy)]
pub struct GenerationIdentity {
    /// Repository owning the generation.
    pub repository: RepositoryId,
    /// Optional parent generation.
    pub parent: Option<GenerationId>,
    /// Canonical discovery or input-manifest hash.
    pub manifest_hash: ContentHash,
    /// Canonical configuration-snapshot hash.
    pub config_hash: ContentHash,
    /// Canonical adapter or provider-set hash.
    pub provider_set_hash: ContentHash,
    /// Versioned storage or logical-format identifier.
    pub format_version: u32,
}

/// Canonical fields used to derive a [`SymbolId`].
#[derive(Debug, Clone, Copy)]
pub struct SymbolIdentity<'a> {
    /// Repository owning the symbol.
    pub repository: RepositoryId,
    /// Canonical language identifier.
    pub language: &'a str,
    /// Canonical semantic kind.
    pub semantic_kind: &'a str,
    /// Canonical container identity, empty only for roots.
    pub container_identity: &'a [u8],
    /// Canonical declared identity.
    pub declared_identity: &'a str,
    /// Canonical overload or signature discriminator.
    pub signature_discriminator: &'a [u8],
    /// Canonical build-context discriminator.
    pub build_context_discriminator: &'a [u8],
}

/// Canonical fields used to derive a [`FileId`].
#[derive(Debug, Clone, Copy)]
pub struct FileIdentity<'a> {
    /// Repository owning the file.
    pub repository: RepositoryId,
    /// Canonical path identity bytes supplied by the future VFS boundary.
    pub path_identity: &'a [u8],
}

/// Derives a repository identity from a persisted local UUID or canonical origin.
#[must_use]
pub fn derive_repository(input: &[u8]) -> DerivedId<RepositoryId> {
    derive(
        REPOSITORY_CONTEXT,
        |encoder| {
            encoder.bytes(input);
        },
        RepositoryId::from_bytes,
    )
}

/// Derives an immutable generation identity from its complete semantic inputs.
#[must_use]
pub fn derive_generation(input: GenerationIdentity) -> DerivedId<GenerationId> {
    derive(
        GENERATION_CONTEXT,
        |encoder| {
            encoder.bytes(input.repository.as_bytes());
            match input.parent {
                Some(parent) => {
                    encoder.optional_bytes(Some(parent.as_bytes()));
                }
                None => {
                    encoder.optional_bytes(None);
                }
            }
            encoder
                .bytes(input.manifest_hash.as_bytes())
                .bytes(input.config_hash.as_bytes())
                .bytes(input.provider_set_hash.as_bytes())
                .u32(input.format_version);
        },
        GenerationId::from_bytes,
    )
}

/// Derives a symbol identity from canonical language and build-context semantics.
#[must_use]
pub fn derive_symbol(input: SymbolIdentity<'_>) -> DerivedId<SymbolId> {
    derive(
        SYMBOL_CONTEXT,
        |encoder| {
            encoder
                .bytes(input.repository.as_bytes())
                .text(input.language)
                .text(input.semantic_kind)
                .bytes(input.container_identity)
                .text(input.declared_identity)
                .bytes(input.signature_discriminator)
                .bytes(input.build_context_discriminator);
        },
        SymbolId::from_bytes,
    )
}

/// Derives a repository-scoped file identity from canonical path bytes.
#[must_use]
pub fn derive_file(input: FileIdentity<'_>) -> DerivedId<FileId> {
    derive(
        FILE_CONTEXT,
        |encoder| {
            encoder
                .bytes(input.repository.as_bytes())
                .bytes(input.path_identity);
        },
        FileId::from_bytes,
    )
}

/// Derives a fact identity from its domain and canonical semantic payload.
#[must_use]
pub fn derive_fact(domain: &str, semantic_payload: &[u8]) -> DerivedId<FactId> {
    derive(
        FACT_CONTEXT,
        |encoder| {
            encoder.text(domain).bytes(semantic_payload);
        },
        FactId::from_bytes,
    )
}

/// Computes the immutable content hash for a byte slice.
#[must_use]
pub fn content_hash(content: &[u8]) -> ContentHash {
    ContentHash::from_bytes(*blake3::hash(content).as_bytes())
}

fn derive<const N: usize, T>(
    context: &str,
    encode: impl FnOnce(&mut CanonicalEncoder),
    compact: impl FnOnce([u8; N]) -> T,
) -> DerivedId<T> {
    let mut encoder = CanonicalEncoder::default();
    encode(&mut encoder);
    let digest = blake3::derive_key(context, encoder.as_bytes());
    let mut compact_bytes = [0_u8; N];
    compact_bytes.copy_from_slice(&digest[..N]);
    DerivedId {
        id: compact(compact_bytes),
        digest: IdentityDigest::from_bytes(digest),
    }
}

#[derive(Debug, Default)]
struct CanonicalEncoder {
    bytes: Vec<u8>,
}

impl CanonicalEncoder {
    fn bytes(&mut self, value: &[u8]) -> &mut Self {
        self.length(value.len());
        self.bytes.extend_from_slice(value);
        self
    }

    fn length(&mut self, value: usize) -> &mut Self {
        match u32::try_from(value) {
            Ok(value) if value < u32::MAX => {
                self.bytes.extend_from_slice(&value.to_be_bytes());
            }
            _ => {
                // UINT32_MAX is an escape marker so oversized adjacent fields cannot
                // collapse onto the same byte stream through a saturated prefix.
                self.bytes.extend_from_slice(&u32::MAX.to_be_bytes());
                let value =
                    u64::try_from(value).expect("supported Rust targets have at most 64-bit usize");
                self.bytes.extend_from_slice(&value.to_be_bytes());
            }
        }
        self
    }

    fn text(&mut self, value: &str) -> &mut Self {
        self.bytes(value.as_bytes())
    }

    fn optional_bytes(&mut self, value: Option<&[u8]>) -> &mut Self {
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.bytes(value);
            }
            None => self.bytes.push(0),
        }
        self
    }

    fn u32(&mut self, value: u32) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_be_bytes());
        self
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
fn encode_text(prefix: &str, raw: &[u8]) -> String {
    let checksum = text_checksum(prefix, raw);
    let mut payload = Vec::with_capacity(raw.len() + CHECKSUM_BYTES);
    payload.extend_from_slice(raw);
    payload.extend_from_slice(&checksum);
    let encoded = BASE32_NOPAD.encode(&payload).to_ascii_lowercase();
    format!("{prefix}{encoded}")
}

fn decode_text<const N: usize>(
    prefix: &str,
    input: &str,
    encoded_buffer: &mut [u8],
    payload: &mut [u8],
) -> Result<[u8; N], IdParseError> {
    if input.trim() != input {
        return Err(IdParseError::Whitespace);
    }
    if input.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(IdParseError::Uppercase);
    }
    if input.contains('=') {
        return Err(IdParseError::Padding);
    }
    if !input.is_ascii() {
        return Err(IdParseError::InvalidAlphabet);
    }

    let encoded = input
        .strip_prefix(prefix)
        .ok_or_else(|| classify_prefix(prefix, input))?;
    let expected_length = N + CHECKSUM_BYTES;
    let expected_encoded_length = (expected_length * 8).div_ceil(5);
    if encoded.len() != expected_encoded_length {
        return Err(IdParseError::InvalidEncodedLength {
            expected: expected_encoded_length,
            actual: encoded.len(),
        });
    }
    // Macro-owned buffers have fixed family widths, independent of input.
    // The exact-length gate above must precede copying or invoking the codec.
    debug_assert_eq!(encoded_buffer.len(), expected_encoded_length);
    debug_assert_eq!(payload.len(), expected_length);
    encoded_buffer.copy_from_slice(encoded.as_bytes());
    encoded_buffer.make_ascii_uppercase();
    let decoded_length = BASE32_NOPAD
        .decode_mut(encoded_buffer, payload)
        .map_err(|_| IdParseError::InvalidAlphabet)?;
    if decoded_length != expected_length {
        return Err(IdParseError::InvalidLength {
            expected: expected_length,
            actual: decoded_length,
        });
    }

    let (raw, checksum) = payload.split_at(N);
    if checksum != text_checksum(prefix, raw) {
        return Err(IdParseError::ChecksumMismatch);
    }

    let mut bytes = [0_u8; N];
    bytes.copy_from_slice(raw);
    // Reuse the verified payload, including its checksum, for the canonical
    // round-trip check instead of deriving and allocating the entire ID again.
    BASE32_NOPAD.encode_mut(payload, encoded_buffer);
    encoded_buffer.make_ascii_lowercase();
    if encoded_buffer != encoded.as_bytes() {
        return Err(IdParseError::NonCanonical);
    }
    Ok(bytes)
}

fn classify_prefix(expected: &str, input: &str) -> IdParseError {
    let Some(separator) = input.find('_') else {
        return IdParseError::InvalidPrefix;
    };
    let found = &input[..=separator];
    let expected_family = expected.trim_end_matches("1_");
    if found.starts_with(expected_family) && found != expected {
        IdParseError::UnsupportedVersion
    } else {
        IdParseError::InvalidPrefix
    }
}

fn text_checksum(prefix: &str, raw: &[u8]) -> [u8; CHECKSUM_BYTES] {
    // Recovery formats many references. Reuse only the immutable derivation
    // context and stream the existing length framing without a per-ID buffer.
    let mut hasher = TEXT_CHECKSUM_HASHER.clone();
    for value in [prefix.as_bytes(), raw] {
        match u32::try_from(value.len()) {
            Ok(length) if length < u32::MAX => {
                hasher.update(&length.to_be_bytes());
            }
            _ => {
                hasher.update(&u32::MAX.to_be_bytes());
                let length = u64::try_from(value.len())
                    .expect("supported Rust targets have at most 64-bit usize");
                hasher.update(&length.to_be_bytes());
            }
        }
        hasher.update(value);
    }
    let digest = hasher.finalize();
    let mut checksum = [0_u8; CHECKSUM_BYTES];
    checksum.copy_from_slice(&digest.as_bytes()[..CHECKSUM_BYTES]);
    checksum
}

/// Errors returned by strict public ID decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdParseError {
    /// The expected ID family prefix was absent.
    #[error("invalid identifier prefix")]
    InvalidPrefix,
    /// The ID uses a recognized family with an unsupported encoding version.
    #[error("unsupported identifier version")]
    UnsupportedVersion,
    /// The input contains uppercase characters.
    #[error("uppercase identifier encoding is not canonical")]
    Uppercase,
    /// The input contains base32 padding.
    #[error("padded identifier encoding is not canonical")]
    Padding,
    /// The input contains leading or trailing whitespace.
    #[error("identifier encoding contains whitespace")]
    Whitespace,
    /// The encoded payload contains characters outside the base32 alphabet.
    #[error("identifier encoding uses an invalid alphabet")]
    InvalidAlphabet,
    /// The encoded payload has the wrong fixed width.
    #[error("invalid identifier encoded length: expected {expected}, got {actual}")]
    InvalidEncodedLength {
        /// Required unpadded base32 character count.
        expected: usize,
        /// Observed encoded character count.
        actual: usize,
    },
    /// The decoded payload has the wrong length.
    #[error("invalid identifier payload length: expected {expected}, got {actual}")]
    InvalidLength {
        /// Required decoded payload length in bytes.
        expected: usize,
        /// Observed decoded payload length in bytes.
        actual: usize,
    },
    /// The checksum does not match the family and raw bytes.
    #[error("identifier checksum mismatch")]
    ChecksumMismatch,
    /// The input decodes but is not the unique canonical spelling.
    #[error("identifier encoding is not canonical")]
    NonCanonical,
}

struct Hex<'a>(&'a [u8]);

impl fmt::Debug for Hex<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Keep the allocating decoder as an independent compatibility oracle for
    // the exact error precedence as well as canonical successful inputs.
    fn decode_text_legacy<const N: usize>(
        prefix: &str,
        input: &str,
    ) -> Result<[u8; N], IdParseError> {
        if input.trim() != input {
            return Err(IdParseError::Whitespace);
        }
        if input.bytes().any(|byte| byte.is_ascii_uppercase()) {
            return Err(IdParseError::Uppercase);
        }
        if input.contains('=') {
            return Err(IdParseError::Padding);
        }
        if !input.is_ascii() {
            return Err(IdParseError::InvalidAlphabet);
        }

        let encoded = input
            .strip_prefix(prefix)
            .ok_or_else(|| classify_prefix(prefix, input))?;
        let expected_length = N + CHECKSUM_BYTES;
        let expected_encoded_length = (expected_length * 8).div_ceil(5);
        if encoded.len() != expected_encoded_length {
            return Err(IdParseError::InvalidEncodedLength {
                expected: expected_encoded_length,
                actual: encoded.len(),
            });
        }
        let payload = BASE32_NOPAD
            .decode(encoded.to_ascii_uppercase().as_bytes())
            .map_err(|_| IdParseError::InvalidAlphabet)?;
        if payload.len() != expected_length {
            return Err(IdParseError::InvalidLength {
                expected: expected_length,
                actual: payload.len(),
            });
        }

        let (raw, checksum) = payload.split_at(N);
        if checksum != text_checksum(prefix, raw) {
            return Err(IdParseError::ChecksumMismatch);
        }

        let mut bytes = [0_u8; N];
        bytes.copy_from_slice(raw);
        if encode_text(prefix, &bytes) != input {
            return Err(IdParseError::NonCanonical);
        }
        Ok(bytes)
    }

    use super::*;
    use proptest::prelude::*;

    fn uncached_text_checksum(prefix: &str, raw: &[u8]) -> [u8; CHECKSUM_BYTES] {
        let mut encoder = CanonicalEncoder::default();
        encoder.text(prefix).bytes(raw);
        let digest = blake3::derive_key(TEXT_CHECKSUM_CONTEXT, encoder.as_bytes());
        let mut checksum = [0; CHECKSUM_BYTES];
        checksum.copy_from_slice(&digest[..CHECKSUM_BYTES]);
        checksum
    }

    #[test]
    fn checksum_calls_do_not_share_message_state() {
        std::thread::scope(|scope| {
            for worker in 0_u8..8 {
                scope.spawn(move || {
                    for value in 0_u8..=255 {
                        let mut raw = [value; 20];
                        raw[0] = worker;
                        for prefix in ["sym1_", "fact1_", "file1_"] {
                            assert_eq!(
                                text_checksum(prefix, &raw),
                                uncached_text_checksum(prefix, &raw)
                            );
                        }
                    }
                });
            }
        });
    }

    fn repository() -> DerivedId<RepositoryId> {
        derive_repository(b"01914f58-0bd1-7f65-b52c-73aebd98c4a1")
    }

    #[test]
    fn identifier_families_are_domain_separated() {
        let repository = repository();
        let fact = derive_fact("repository", b"01914f58-0bd1-7f65-b52c-73aebd98c4a1");
        assert_ne!(repository.digest(), fact.digest());
    }

    #[test]
    fn field_boundaries_change_symbol_identity() {
        let repository = repository().id();
        let first = derive_symbol(SymbolIdentity {
            repository,
            language: "rust",
            semantic_kind: "function",
            container_identity: b"ab",
            declared_identity: "c",
            signature_discriminator: b"",
            build_context_discriminator: b"",
        });
        let second = derive_symbol(SymbolIdentity {
            repository,
            language: "rust",
            semantic_kind: "function",
            container_identity: b"a",
            declared_identity: "bc",
            signature_discriminator: b"",
            build_context_discriminator: b"",
        });
        assert_ne!(first.digest(), second.digest());
    }

    #[test]
    fn extended_length_marker_is_unambiguous() {
        let mut maximum_inline = CanonicalEncoder::default();
        maximum_inline.length(u32::MAX as usize - 1);
        let mut first_extended = CanonicalEncoder::default();
        first_extended.length(u32::MAX as usize);

        assert_ne!(maximum_inline.as_bytes(), first_extended.as_bytes());
        assert_eq!(maximum_inline.as_bytes().len(), 4);
        assert_eq!(first_extended.as_bytes().len(), 12);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn extended_length_prefixes_do_not_saturate() {
        let mut first = CanonicalEncoder::default();
        first.length(u32::MAX as usize);
        let mut second = CanonicalEncoder::default();
        second.length(u32::MAX as usize + 1);

        assert_ne!(first.as_bytes(), second.as_bytes());
    }

    #[test]
    fn strict_codec_rejects_noncanonical_forms() {
        let text = repository().id().to_string();
        assert_eq!(
            text.to_ascii_uppercase().parse::<RepositoryId>(),
            Err(IdParseError::Uppercase)
        );
        assert_eq!(
            format!("{text}=").parse::<RepositoryId>(),
            Err(IdParseError::Padding)
        );
        assert_eq!(
            format!(" {text}").parse::<RepositoryId>(),
            Err(IdParseError::Whitespace)
        );
        assert_eq!(
            text.replacen("repo1_", "repo2_", 1).parse::<RepositoryId>(),
            Err(IdParseError::UnsupportedVersion)
        );
    }

    #[test]
    fn fixed_width_is_checked_before_base32_decode() {
        let short = "repo1_a";
        assert_eq!(
            short.parse::<RepositoryId>(),
            Err(IdParseError::InvalidEncodedLength {
                expected: 32,
                actual: 1,
            })
        );

        let long = format!("repo1_{}", "a".repeat(1_000_000));
        assert_eq!(
            long.parse::<RepositoryId>(),
            Err(IdParseError::InvalidEncodedLength {
                expected: 32,
                actual: 1_000_000,
            })
        );
    }

    #[test]
    fn corruption_changes_checksum() {
        let mut text = repository().id().to_string().into_bytes();
        let last = text.last_mut().expect("derived IDs are non-empty");
        *last = if *last == b'a' { b'b' } else { b'a' };
        let corrupted = String::from_utf8(text).expect("mutated base32 remains UTF-8");
        assert_eq!(
            corrupted.parse::<RepositoryId>(),
            Err(IdParseError::ChecksumMismatch)
        );
    }

    #[test]
    fn serde_uses_the_canonical_string() {
        let id = repository().id();
        let json = serde_json::to_string(&id).expect("repository ID serializes");
        let decoded = serde_json::from_str::<RepositoryId>(&json)
            .expect("canonical repository ID deserializes");
        assert_eq!(decoded, id);
    }

    #[test]
    fn collision_disposition_never_silently_merges() {
        let digest = IdentityDigest::from_bytes([1; 32]);
        assert_eq!(
            collision_disposition(None, digest),
            CollisionDisposition::Vacant
        );
        assert_eq!(
            collision_disposition(Some(digest), digest),
            CollisionDisposition::SameIdentity
        );
        assert_eq!(
            collision_disposition(Some(digest), IdentityDigest::from_bytes([2; 32])),
            CollisionDisposition::Collision
        );
    }

    proptest! {
        #[test]
        fn text_checksum_matches_uncached_canonical_recipe(
            short in any::<[u8; 16]>(),
            medium in any::<[u8; 20]>(),
            long in any::<[u8; 32]>(),
        ) {
            for (prefix, raw) in [
                ("repo1_", short.as_slice()),
                ("op1_", short.as_slice()),
                ("gen1_", medium.as_slice()),
                ("sym1_", medium.as_slice()),
                ("file1_", medium.as_slice()),
                ("fact1_", medium.as_slice()),
                ("b3_", long.as_slice()),
            ] {
                prop_assert_eq!(text_checksum(prefix, raw), uncached_text_checksum(prefix, raw));
            }
        }

        #[test]
        fn fixed_buffer_decoder_matches_reference_errors(
            short in any::<[u8; 16]>(),
            medium in any::<[u8; 20]>(),
            long in any::<[u8; 32]>(),
            position in 0_usize..64,
            replacement in 0_u8..=127,
            arbitrary in ".{0,96}",
        ) {
            macro_rules! check_family {
                ($family:ident, $prefix:literal, $bytes:expr, $size:expr) => {{
                    let canonical = encode_text($prefix, &$bytes);
                    let mut mutated = canonical.as_bytes().to_vec();
                    let index = position % mutated.len();
                    mutated[index] = replacement;
                    let mutated = String::from_utf8(mutated).expect("ASCII mutation is UTF-8");
                    for input in [&canonical, &mutated, &arbitrary] {
                        prop_assert_eq!(
                            input.parse::<$family>().map(|id| *id.as_bytes()),
                            decode_text_legacy::<$size>($prefix, input)
                        );
                    }
                }};
            }
            check_family!(RepositoryId, "repo1_", short, 16);
            check_family!(OperationId, "op1_", short, 16);
            check_family!(GenerationId, "gen1_", medium, 20);
            check_family!(SymbolId, "sym1_", medium, 20);
            check_family!(FileId, "file1_", medium, 20);
            check_family!(FactId, "fact1_", medium, 20);
            check_family!(ContentHash, "b3_", long, 32);
        }

        #[test]
        fn stack_text_codec_matches_canonical_encoding(
            short in any::<[u8; 16]>(),
            medium in any::<[u8; 20]>(),
            long in any::<[u8; 32]>(),
        ) {
            macro_rules! check_family {
                ($family:ident, $prefix:literal, $bytes:expr) => {{
                    let id = $family::from_bytes($bytes);
                    let expected = encode_text($prefix, &$bytes);
                    prop_assert_eq!(id.to_string(), expected.as_str());
                    prop_assert_eq!(format!("{id:>70}"), format!("{expected:>70}"));
                    prop_assert_eq!(format!("{id:.12}"), format!("{expected:.12}"));
                    prop_assert_eq!(
                        format!("{id:?}"),
                        format!("{}({expected:?})", stringify!($family))
                    );
                    prop_assert_eq!(
                        serde_json::to_string(&id).expect("ID serializes"),
                        serde_json::to_string(&expected).expect("string serializes")
                    );
                    prop_assert_eq!(expected.parse::<$family>(), Ok(id));
                }};
            }
            check_family!(RepositoryId, "repo1_", short);
            check_family!(OperationId, "op1_", short);
            check_family!(GenerationId, "gen1_", medium);
            check_family!(SymbolId, "sym1_", medium);
            check_family!(FileId, "file1_", medium);
            check_family!(FactId, "fact1_", medium);
            check_family!(ContentHash, "b3_", long);
        }

        #[test]
        fn operation_id_round_trips(bytes in any::<[u8; 16]>()) {
            let id = OperationId::from_bytes(bytes);
            prop_assert_eq!(id.to_string().parse::<OperationId>(), Ok(id));
        }

        #[test]
        fn content_hash_round_trips(bytes in any::<[u8; 32]>()) {
            let hash = ContentHash::from_bytes(bytes);
            prop_assert_eq!(hash.to_string().parse::<ContentHash>(), Ok(hash));
        }
    }
}
