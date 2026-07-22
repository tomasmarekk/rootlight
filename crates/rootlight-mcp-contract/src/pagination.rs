//! Opaque authenticated pagination cursors bound to a pinned generation.
//!
//! Cursors are integrity-protected envelopes that prevent scope changes,
//! generation mixing, and forgery across query boundaries. A cursor cannot
//! change scope, filters, confidence, budget semantics, or generation.

use rootlight_ids::{GenerationId, RepositoryId};

use crate::{McpTool, vertical::ResponseProfile};

/// Maximum serialized cursor bytes accepted on the wire.
pub const MAX_CURSOR_BYTES: usize = 4_096;

/// Cursor validity window in milliseconds.
const CURSOR_TTL_MS: u64 = 300_000;

/// Maximum accepted clock skew between a cursor's issue time and server time.
const CLOCK_SKEW_MS: u64 = 30_000;

/// Errors returned during cursor creation or validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CursorError {
    /// The cursor payload exceeds the wire byte ceiling.
    #[error("cursor exceeds the maximum byte length")]
    TooLong,
    /// The cursor is empty or structurally malformed.
    #[error("cursor is malformed")]
    Malformed,
    /// The cursor was issued for a different repository.
    #[error("cursor repository mismatch")]
    RepositoryMismatch,
    /// The cursor was issued for a different generation.
    #[error("cursor generation mismatch")]
    GenerationMismatch,
    /// The cursor was issued for a different tool or query shape.
    #[error("cursor query mismatch")]
    QueryMismatch,
    /// The cursor has expired.
    #[error("cursor expired")]
    Expired,
    /// The cursor integrity check failed.
    #[error("cursor integrity check failed")]
    IntegrityFailed,
    /// The cursor claims an issue time unreasonably far in the future.
    #[error("cursor issued in the future")]
    IssuedInTheFuture,
    /// The cursor was signed by an unknown or retired server key.
    #[error("cursor key unknown or retired")]
    KeyMismatch,
    /// The cursor uses a wire version this build does not serve.
    #[error("cursor wire version unsupported")]
    UnsupportedVersion,
    /// The cursor validity window cannot be represented by the timestamp type.
    #[error("cursor timestamp exceeds the supported range")]
    TimestampOverflow,
}

/// Bound context that a cursor is pinned to.
///
/// All fields participate in the integrity fingerprint. Changing any field
/// invalidates the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorContext {
    /// Repository the cursor is bound to.
    pub repository: RepositoryId,
    /// Immutable generation the cursor is bound to.
    pub generation: GenerationId,
    /// Tool that issued the cursor.
    pub tool: McpTool,
    /// Tool contract major version the cursor was issued under.
    pub tool_major_version: u16,
    /// Opaque query-shape fingerprint derived from normalized request parameters.
    pub query_fingerprint: [u8; 32],
    /// Physical-plan fingerprint the cursor is bound to.
    pub plan_fingerprint: [u8; 32],
    /// Response profile the cursor was issued under.
    pub response_profile: ResponseProfile,
    /// Repository or catalog snapshot identity the cursor is bound to.
    pub snapshot_id: [u8; 32],
    /// Requested page size at cursor creation time.
    pub page_size: u16,
    /// Non-secret server key identifier that signed the cursor.
    pub key_id: u64,
}

/// An opaque continuation cursor with embedded integrity metadata.
///
/// The wire format is a versioned, base64url-encoded envelope containing
/// the cursor context, a last-sort-key offset, issue timestamp, and a
/// BLAKE3 keyed hash for tamper detection. The server instance key is
/// process-local and rotates on restart, which invalidates outstanding
/// cursors gracefully (they return INVALID_CURSOR with a safe restart
/// request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedCursor {
    /// Bound context.
    context: CursorContext,
    /// Opaque last-sort-key for deterministic page continuation.
    last_sort_key: Vec<u8>,
    /// Issue time as Unix milliseconds.
    issued_at_ms: u64,
    /// Explicit expiry time as Unix milliseconds.
    expires_at_ms: u64,
    /// Integrity tag.
    tag: [u8; 32],
}

impl AuthenticatedCursor {
    /// Creates a new authenticated cursor bound to the given context.
    ///
    /// The server key is a process-local secret used for BLAKE3 keyed hashing.
    /// It rotates on daemon restart, gracefully invalidating old cursors.
    /// # Errors
    ///
    /// Returns [`CursorError::TooLong`] when the sort key cannot be represented
    /// by the bounded wire envelope, or [`CursorError::TimestampOverflow`] when
    /// the validity window exceeds the timestamp range.
    pub fn create(
        context: CursorContext,
        last_sort_key: Vec<u8>,
        issued_at_ms: u64,
        server_key: &[u8; 32],
    ) -> Result<Self, CursorError> {
        let _sort_key_len = u16::try_from(last_sort_key.len()).map_err(|_| CursorError::TooLong)?;
        let expires_at_ms = issued_at_ms
            .checked_add(CURSOR_TTL_MS)
            .ok_or(CursorError::TimestampOverflow)?;
        let tag = compute_tag(
            &context,
            &last_sort_key,
            issued_at_ms,
            expires_at_ms,
            server_key,
        );
        let cursor = Self {
            context,
            last_sort_key,
            issued_at_ms,
            expires_at_ms,
            tag,
        };
        if cursor.to_wire().len() > MAX_CURSOR_BYTES {
            return Err(CursorError::TooLong);
        }
        Ok(cursor)
    }

    /// Validates a cursor against the expected context and current time.
    ///
    /// # Errors
    ///
    /// Returns [`CursorError`] when the cursor is expired, bound to a
    /// different repository or generation, issued for a different query
    /// shape, or fails the integrity check.
    pub fn validate(
        &self,
        expected: &CursorContext,
        now_ms: u64,
        server_key: &[u8; 32],
    ) -> Result<(), CursorError> {
        if self.context.key_id != expected.key_id {
            return Err(CursorError::KeyMismatch);
        }
        if self.context.repository != expected.repository {
            return Err(CursorError::RepositoryMismatch);
        }
        if self.context.generation != expected.generation {
            return Err(CursorError::GenerationMismatch);
        }
        if self.context.tool != expected.tool
            || self.context.tool_major_version != expected.tool_major_version
            || self.context.query_fingerprint != expected.query_fingerprint
            || self.context.plan_fingerprint != expected.plan_fingerprint
            || self.context.response_profile != expected.response_profile
            || self.context.snapshot_id != expected.snapshot_id
            || self.context.page_size != expected.page_size
        {
            return Err(CursorError::QueryMismatch);
        }
        // Reject cursors issued unreasonably in the future instead of relying
        // on saturating arithmetic, which would silently accept them.
        if self.issued_at_ms > now_ms.saturating_add(CLOCK_SKEW_MS) {
            return Err(CursorError::IssuedInTheFuture);
        }
        if now_ms > self.expires_at_ms {
            return Err(CursorError::Expired);
        }
        let expected_tag = compute_tag(
            &self.context,
            &self.last_sort_key,
            self.issued_at_ms,
            self.expires_at_ms,
            server_key,
        );
        if !constant_time_tag_eq(&expected_tag, &self.tag) {
            return Err(CursorError::IntegrityFailed);
        }
        Ok(())
    }

    /// Returns the opaque last-sort-key for page continuation.
    #[must_use]
    pub fn last_sort_key(&self) -> &[u8] {
        &self.last_sort_key
    }

    /// Returns the immutable repository or catalog snapshot identity.
    #[must_use]
    pub const fn snapshot_id(&self) -> [u8; 32] {
        self.context.snapshot_id
    }

    /// Returns the issue timestamp in Unix milliseconds.
    #[must_use]
    pub const fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    /// Returns the explicit expiry timestamp in Unix milliseconds.
    #[must_use]
    pub const fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    /// Serializes the cursor to an opaque wire string.
    ///
    /// The format is version-prefixed base64url without padding.
    #[must_use]
    pub fn to_wire(&self) -> String {
        let payload = self.serialize_payload();
        format!("c2.{}", base64url_encode(&payload))
    }

    /// Parses a cursor from its opaque wire string.
    ///
    /// # Errors
    ///
    /// Returns [CursorError::Malformed] or [CursorError::TooLong] when
    /// the wire string cannot be decoded.
    pub fn from_wire(wire: &str) -> Result<Self, CursorError> {
        if wire.len() > MAX_CURSOR_BYTES {
            return Err(CursorError::TooLong);
        }
        // The legacy c1 envelope predates the bound plan, snapshot, key, and
        // expiry fields and is never reinterpreted under c2 semantics.
        if wire.starts_with("c1.") {
            return Err(CursorError::UnsupportedVersion);
        }
        let encoded = wire.strip_prefix("c2.").ok_or(CursorError::Malformed)?;
        let payload = base64url_decode(encoded).ok_or(CursorError::Malformed)?;
        Self::deserialize_payload(&payload)
    }

    fn serialize_payload(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(self.context.repository.as_bytes());
        payload.extend_from_slice(self.context.generation.as_bytes());
        payload.extend_from_slice(self.context.tool.name().as_bytes());
        payload.push(0);
        payload.extend_from_slice(&self.context.tool_major_version.to_le_bytes());
        payload.extend_from_slice(&self.context.query_fingerprint);
        payload.extend_from_slice(&self.context.plan_fingerprint);
        payload.push(response_profile_tag(self.context.response_profile));
        payload.extend_from_slice(&self.context.snapshot_id);
        payload.extend_from_slice(&self.context.page_size.to_le_bytes());
        payload.extend_from_slice(&self.context.key_id.to_le_bytes());
        payload.extend_from_slice(&self.issued_at_ms.to_le_bytes());
        payload.extend_from_slice(&self.expires_at_ms.to_le_bytes());
        let sort_key_len =
            u16::try_from(self.last_sort_key.len()).expect("cursor creation bounds sort keys");
        payload.extend_from_slice(&sort_key_len.to_le_bytes());
        payload.extend_from_slice(&self.last_sort_key);
        payload.extend_from_slice(&self.tag);
        payload
    }

    fn deserialize_payload(payload: &[u8]) -> Result<Self, CursorError> {
        // repository(16) + generation(20) + tool-name null(1) + tool major(2)
        // + query fingerprint(32) + plan fingerprint(32) + profile(1)
        // + snapshot(32) + page size(2) + key id(8) + issued(8) + expiry(8)
        // + sort-key length(2) + tag(32).
        const MIN_LEN: usize = 16 + 20 + 1 + 2 + 32 + 32 + 1 + 32 + 2 + 8 + 8 + 8 + 2 + 32;
        if payload.len() < MIN_LEN {
            return Err(CursorError::Malformed);
        }
        let mut offset = 0;
        let repo_bytes: [u8; 16] = payload
            .get(offset..offset + 16)
            .ok_or(CursorError::Malformed)?
            .try_into()
            .map_err(|_| CursorError::Malformed)?;
        offset += 16;
        let gen_bytes: [u8; 20] = payload
            .get(offset..offset + 20)
            .ok_or(CursorError::Malformed)?
            .try_into()
            .map_err(|_| CursorError::Malformed)?;
        offset += 20;

        let nul_pos = payload
            .get(offset..)
            .and_then(|slice| slice.iter().position(|&b| b == 0))
            .ok_or(CursorError::Malformed)?;
        let tool_name_bytes = payload
            .get(offset..offset + nul_pos)
            .ok_or(CursorError::Malformed)?;
        let tool_name = std::str::from_utf8(tool_name_bytes).map_err(|_| CursorError::Malformed)?;
        offset += nul_pos + 1;

        let tool_major_version = u16::from_le_bytes(
            payload
                .get(offset..offset + 2)
                .ok_or(CursorError::Malformed)?
                .try_into()
                .map_err(|_| CursorError::Malformed)?,
        );
        offset += 2;

        let query_fingerprint: [u8; 32] = payload
            .get(offset..offset + 32)
            .ok_or(CursorError::Malformed)?
            .try_into()
            .map_err(|_| CursorError::Malformed)?;
        offset += 32;

        let plan_fingerprint: [u8; 32] = payload
            .get(offset..offset + 32)
            .ok_or(CursorError::Malformed)?
            .try_into()
            .map_err(|_| CursorError::Malformed)?;
        offset += 32;

        let response_profile =
            response_profile_from_tag(*payload.get(offset).ok_or(CursorError::Malformed)?)?;
        offset += 1;

        let snapshot_id: [u8; 32] = payload
            .get(offset..offset + 32)
            .ok_or(CursorError::Malformed)?
            .try_into()
            .map_err(|_| CursorError::Malformed)?;
        offset += 32;

        let page_size = u16::from_le_bytes(
            payload
                .get(offset..offset + 2)
                .ok_or(CursorError::Malformed)?
                .try_into()
                .map_err(|_| CursorError::Malformed)?,
        );
        offset += 2;

        let key_id = u64::from_le_bytes(
            payload
                .get(offset..offset + 8)
                .ok_or(CursorError::Malformed)?
                .try_into()
                .map_err(|_| CursorError::Malformed)?,
        );
        offset += 8;

        let issued_at_ms = u64::from_le_bytes(
            payload
                .get(offset..offset + 8)
                .ok_or(CursorError::Malformed)?
                .try_into()
                .map_err(|_| CursorError::Malformed)?,
        );
        offset += 8;

        let expires_at_ms = u64::from_le_bytes(
            payload
                .get(offset..offset + 8)
                .ok_or(CursorError::Malformed)?
                .try_into()
                .map_err(|_| CursorError::Malformed)?,
        );
        offset += 8;

        let sort_key_len = usize::from(u16::from_le_bytes(
            payload
                .get(offset..offset + 2)
                .ok_or(CursorError::Malformed)?
                .try_into()
                .map_err(|_| CursorError::Malformed)?,
        ));
        offset += 2;

        let last_sort_key = payload
            .get(offset..offset + sort_key_len)
            .ok_or(CursorError::Malformed)?
            .to_vec();
        offset += sort_key_len;

        let tag: [u8; 32] = payload
            .get(offset..offset + 32)
            .ok_or(CursorError::Malformed)?
            .try_into()
            .map_err(|_| CursorError::Malformed)?;
        offset += 32;

        // The authenticated envelope must end exactly at the tag. Trailing
        // bytes after a valid tag would otherwise be accepted silently,
        // defeating the canonical-envelope tamper detection.
        if offset != payload.len() {
            return Err(CursorError::Malformed);
        }

        if expires_at_ms < issued_at_ms
            || expires_at_ms.saturating_sub(issued_at_ms) != CURSOR_TTL_MS
        {
            return Err(CursorError::Malformed);
        }

        let repository = RepositoryId::from_bytes(repo_bytes);
        let generation = GenerationId::from_bytes(gen_bytes);

        Ok(Self {
            context: CursorContext {
                repository,
                generation,
                tool: parse_tool_name(tool_name)?,
                tool_major_version,
                query_fingerprint,
                plan_fingerprint,
                response_profile,
                snapshot_id,
                page_size,
                key_id,
            },
            last_sort_key,
            issued_at_ms,
            expires_at_ms,
            tag,
        })
    }
}

/// Maps a deserialized tool name back to its typed catalog entry.
fn parse_tool_name(name: &str) -> Result<McpTool, CursorError> {
    for tool in McpTool::ALL {
        if tool.name() == name {
            return Ok(tool);
        }
    }
    Err(CursorError::Malformed)
}

const fn response_profile_tag(profile: ResponseProfile) -> u8 {
    match profile {
        ResponseProfile::Compact => 0,
        ResponseProfile::Standard => 1,
        ResponseProfile::Evidence => 2,
    }
}

fn response_profile_from_tag(tag: u8) -> Result<ResponseProfile, CursorError> {
    match tag {
        0 => Ok(ResponseProfile::Compact),
        1 => Ok(ResponseProfile::Standard),
        2 => Ok(ResponseProfile::Evidence),
        _ => Err(CursorError::Malformed),
    }
}

/// Compares two integrity tags in constant time so a mismatch does not leak the
/// first differing byte through a timing side channel.
fn constant_time_tag_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

/// Computes the BLAKE3 keyed integrity tag for a cursor.
fn compute_tag(
    context: &CursorContext,
    last_sort_key: &[u8],
    issued_at_ms: u64,
    expires_at_ms: u64,
    server_key: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_keyed(server_key);
    hasher.update(context.repository.as_bytes());
    hasher.update(context.generation.as_bytes());
    hasher.update(context.tool.name().as_bytes());
    hasher.update(&[0]);
    hasher.update(&context.tool_major_version.to_le_bytes());
    hasher.update(&context.query_fingerprint);
    hasher.update(&context.plan_fingerprint);
    hasher.update(&[response_profile_tag(context.response_profile)]);
    hasher.update(&context.snapshot_id);
    hasher.update(&context.page_size.to_le_bytes());
    hasher.update(&context.key_id.to_le_bytes());
    hasher.update(&issued_at_ms.to_le_bytes());
    hasher.update(&expires_at_ms.to_le_bytes());
    let sort_key_len = u16::try_from(last_sort_key.len())
        .expect("cursor creation bounds sort keys before authentication");
    hasher.update(&sort_key_len.to_le_bytes());
    hasher.update(last_sort_key);
    *hasher.finalize().as_bytes()
}

const BASE64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url_encode(input: &[u8]) -> String {
    let mut output = String::new();
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        output.push(BASE64URL_ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        output.push(BASE64URL_ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            output.push(BASE64URL_ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        }
        if chunk.len() > 2 {
            output.push(BASE64URL_ALPHABET[(triple & 0x3F) as usize] as char);
        }
    }
    output
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    if input.len() % 4 == 1 {
        return None;
    }
    let mut output = Vec::new();
    let bytes = input.as_bytes();
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let values: Vec<u8> = chunk
            .iter()
            .map(|&b| base64url_value(b))
            .collect::<Option<Vec<u8>>>()?;
        let is_last = index == bytes.len().saturating_sub(1) / 4;
        if is_last
            && ((values.len() == 2 && values[1] & 0x0F != 0)
                || (values.len() == 3 && values[2] & 0x03 != 0))
        {
            return None;
        }
        if values.len() >= 2 {
            output.push((values[0] << 2) | (values[1] >> 4));
        }
        if values.len() >= 3 {
            output.push((values[1] << 4) | (values[2] >> 2));
        }
        if values.len() >= 4 {
            output.push((values[2] << 6) | values[3]);
        }
    }
    Some(output)
}

fn base64url_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthenticatedCursor, CursorContext, CursorError};
    use crate::{McpTool, vertical::ResponseProfile};
    use proptest::prelude::*;
    use rootlight_ids::{GenerationId, RepositoryId};

    fn test_context() -> CursorContext {
        CursorContext {
            repository: RepositoryId::from_bytes([1; 16]),
            generation: GenerationId::from_bytes([2; 20]),
            tool: McpTool::CodeLocate,
            tool_major_version: 1,
            query_fingerprint: [3; 32],
            plan_fingerprint: [4; 32],
            response_profile: ResponseProfile::Compact,
            snapshot_id: [5; 32],
            page_size: 20,
            key_id: 7,
        }
    }

    fn create_cursor(
        context: CursorContext,
        last_sort_key: Vec<u8>,
        issued_at_ms: u64,
        server_key: &[u8; 32],
    ) -> AuthenticatedCursor {
        AuthenticatedCursor::create(context, last_sort_key, issued_at_ms, server_key)
            .expect("test cursor fits the wire envelope")
    }

    #[test]
    fn cursor_round_trips_through_wire_format() {
        let key = [42; 32];
        let context = test_context();
        let cursor = create_cursor(context.clone(), vec![1, 2, 3], 1_000_000, &key);
        let wire = cursor.to_wire();
        assert!(wire.starts_with("c2."));
        assert!(wire.len() <= super::MAX_CURSOR_BYTES);

        let decoded = AuthenticatedCursor::from_wire(&wire).expect("wire decodes");
        assert_eq!(decoded, cursor);
        assert_eq!(decoded.last_sort_key(), &[1, 2, 3]);
        assert_eq!(decoded.snapshot_id(), [5; 32]);
        assert_eq!(decoded.issued_at_ms(), 1_000_000);
    }

    #[test]
    fn snapshot_accessor_returns_the_authenticated_bound_identity() {
        let key = [42; 32];
        let mut context = test_context();
        context.snapshot_id = [91; 32];
        let cursor = create_cursor(context, vec![1], 1_000_000, &key);

        assert_eq!(cursor.snapshot_id(), [91; 32]);
        let decoded =
            AuthenticatedCursor::from_wire(&cursor.to_wire()).expect("wire cursor decodes");
        assert_eq!(decoded.snapshot_id(), [91; 32]);
    }

    #[test]
    fn cursor_validates_against_matching_context() {
        let key = [42; 32];
        let context = test_context();
        let cursor = create_cursor(context.clone(), vec![], 1_000_000, &key);
        assert!(cursor.validate(&context, 1_100_000, &key).is_ok());
    }

    #[test]
    fn cursor_rejects_generation_mismatch() {
        let key = [42; 32];
        let context = test_context();
        let cursor = create_cursor(context, vec![], 1_000_000, &key);
        let mut wrong = test_context();
        wrong.generation = GenerationId::from_bytes([9; 20]);
        assert_eq!(
            cursor.validate(&wrong, 1_100_000, &key),
            Err(CursorError::GenerationMismatch)
        );
    }

    #[test]
    fn cursor_rejects_repository_mismatch() {
        let key = [42; 32];
        let context = test_context();
        let cursor = create_cursor(context, vec![], 1_000_000, &key);
        let mut wrong = test_context();
        wrong.repository = RepositoryId::from_bytes([9; 16]);
        assert_eq!(
            cursor.validate(&wrong, 1_100_000, &key),
            Err(CursorError::RepositoryMismatch)
        );
    }

    #[test]
    fn cursor_rejects_expired_ttl() {
        let key = [42; 32];
        let context = test_context();
        let cursor = create_cursor(context.clone(), vec![], 1_000_000, &key);
        assert_eq!(
            cursor.validate(&context, 1_000_000 + 300_001, &key),
            Err(CursorError::Expired)
        );
    }

    #[test]
    fn cursor_rejects_wrong_server_key() {
        let key = [42; 32];
        let wrong_key = [99; 32];
        let context = test_context();
        let cursor = create_cursor(context.clone(), vec![], 1_000_000, &key);
        assert_eq!(
            cursor.validate(&context, 1_100_000, &wrong_key),
            Err(CursorError::IntegrityFailed)
        );
    }

    #[test]
    fn cursor_rejects_tampered_sort_key() {
        let key = [42; 32];
        let context = test_context();
        let mut cursor = create_cursor(context.clone(), vec![1, 2, 3], 1_000_000, &key);
        cursor.last_sort_key = vec![4, 5, 6];
        assert_eq!(
            cursor.validate(&context, 1_100_000, &key),
            Err(CursorError::IntegrityFailed)
        );
    }

    #[test]
    fn oversized_wire_is_rejected() {
        let oversized = format!("c1.{}", "A".repeat(5000));
        assert_eq!(
            AuthenticatedCursor::from_wire(&oversized),
            Err(CursorError::TooLong)
        );
    }

    #[test]
    fn malformed_wire_is_rejected() {
        assert_eq!(
            AuthenticatedCursor::from_wire("invalid"),
            Err(CursorError::Malformed)
        );
        assert_eq!(
            AuthenticatedCursor::from_wire("c2."),
            Err(CursorError::Malformed)
        );
        assert_eq!(
            AuthenticatedCursor::from_wire("c2.A"),
            Err(CursorError::Malformed)
        );
    }

    #[test]
    fn base64url_decoder_rejects_noncanonical_padding_bits() {
        assert_eq!(super::base64url_decode("AA"), Some(vec![0]));
        assert_eq!(super::base64url_decode("AB"), None);
        assert_eq!(super::base64url_decode("AAA"), Some(vec![0, 0]));
        assert_eq!(super::base64url_decode("AAB"), None);
    }

    #[test]
    fn cursor_creation_rejects_oversized_sort_key() {
        let result = AuthenticatedCursor::create(
            test_context(),
            vec![0; super::MAX_CURSOR_BYTES],
            1_000_000,
            &[42; 32],
        );
        assert_eq!(result, Err(CursorError::TooLong));
    }

    #[test]
    fn cursor_creation_rejects_timestamp_overflow() {
        let result = AuthenticatedCursor::create(test_context(), Vec::new(), u64::MAX, &[42; 32]);
        assert_eq!(result, Err(CursorError::TimestampOverflow));
    }

    #[test]
    fn wire_cursor_rejects_noncanonical_expiry_window() {
        let key = [42; 32];
        let mut cursor = create_cursor(test_context(), vec![], 1_000_000, &key);
        cursor.expires_at_ms = cursor.expires_at_ms.saturating_add(1);
        assert_eq!(
            AuthenticatedCursor::from_wire(&cursor.to_wire()),
            Err(CursorError::Malformed)
        );
    }

    #[test]
    fn legacy_c1_wire_is_rejected_as_unsupported_version() {
        assert_eq!(
            AuthenticatedCursor::from_wire("c1.AAAA"),
            Err(CursorError::UnsupportedVersion)
        );
    }

    #[test]
    fn wire_cursor_with_trailing_bytes_after_tag_is_rejected() {
        let key = [42; 32];
        let context = test_context();
        let cursor = create_cursor(context, vec![1, 2, 3], 1_000_000, &key);
        let wire = cursor.to_wire();
        let body = wire.strip_prefix("c2.").expect("version prefix present");

        // Append one raw byte after the authenticated tag and re-encode. The
        // canonical envelope must be rejected even though the tag itself is
        // intact, because trailing bytes are not covered by the parse.
        let mut payload = super::base64url_decode(body).expect("valid payload decodes");
        payload.push(0xFF);
        let tampered = format!("c2.{}", super::base64url_encode(&payload));

        assert_eq!(
            AuthenticatedCursor::from_wire(&tampered),
            Err(CursorError::Malformed)
        );
    }

    #[test]
    fn deterministic_page_equality_for_pinned_generation() {
        let key = [42; 32];
        let context = test_context();
        let cursor_a = create_cursor(context.clone(), vec![10], 5_000, &key);
        let cursor_b = create_cursor(context, vec![10], 5_000, &key);
        assert_eq!(cursor_a.to_wire(), cursor_b.to_wire());
    }

    #[test]
    fn cursor_rejects_future_issue_time_beyond_skew() {
        let key = [42; 32];
        let context = test_context();
        let cursor = create_cursor(context.clone(), vec![], 1_000_000, &key);
        assert_eq!(
            cursor.validate(&context, 1_000_000 - 60_000, &key),
            Err(CursorError::IssuedInTheFuture)
        );
    }

    #[test]
    fn cursor_accepts_issue_time_within_clock_skew() {
        let key = [42; 32];
        let context = test_context();
        let cursor = create_cursor(context.clone(), vec![], 1_000_000, &key);
        assert!(cursor.validate(&context, 1_000_000 - 10_000, &key).is_ok());
    }

    #[test]
    fn constant_time_tag_comparison_agrees_with_equality() {
        let a = [7u8; 32];
        let mut b = [7u8; 32];
        assert!(super::constant_time_tag_eq(&a, &b));
        b[31] = 8;
        assert!(!super::constant_time_tag_eq(&a, &b));
        b[0] = 9;
        assert!(!super::constant_time_tag_eq(&a, &b));
    }

    #[test]
    fn cursor_rejects_key_id_mismatch() {
        let key = [42; 32];
        let context = test_context();
        let cursor = create_cursor(context, vec![], 1_000_000, &key);
        let mut wrong = test_context();
        wrong.key_id = 99;
        assert_eq!(
            cursor.validate(&wrong, 1_100_000, &key),
            Err(CursorError::KeyMismatch)
        );
    }

    #[test]
    fn cursor_rejects_expired_by_explicit_expiry() {
        let key = [42; 32];
        let context = test_context();
        let cursor = create_cursor(context.clone(), vec![], 1_000_000, &key);
        // expires_at is issued_at + TTL; one millisecond past it is expired.
        assert_eq!(
            cursor.validate(&context, cursor.expires_at_ms() + 1, &key),
            Err(CursorError::Expired)
        );
    }

    #[test]
    fn every_context_mutation_invalidates_validation() {
        let key = [42; 32];
        let context = test_context();
        let cursor = create_cursor(context.clone(), vec![], 1_000_000, &key);

        let mut mutations: Vec<CursorContext> = Vec::new();
        let mut m = test_context();
        m.repository = RepositoryId::from_bytes([9; 16]);
        mutations.push(m);
        let mut m = test_context();
        m.generation = GenerationId::from_bytes([9; 20]);
        mutations.push(m);
        let mut m = test_context();
        m.tool_major_version = 2;
        mutations.push(m);
        let mut m = test_context();
        m.query_fingerprint = [9; 32];
        mutations.push(m);
        let mut m = test_context();
        m.plan_fingerprint = [9; 32];
        mutations.push(m);
        let mut m = test_context();
        m.response_profile = ResponseProfile::Standard;
        mutations.push(m);
        let mut m = test_context();
        m.snapshot_id = [9; 32];
        mutations.push(m);
        let mut m = test_context();
        m.page_size = 21;
        mutations.push(m);
        let mut m = test_context();
        m.key_id = 99;
        mutations.push(m);

        for mutated in mutations {
            assert!(
                cursor.validate(&mutated, 1_100_000, &key).is_err(),
                "mutating any bound context field must invalidate the cursor"
            );
        }
    }

    proptest! {
        #[test]
        fn any_generated_semantic_context_mutation_is_rejected(
            field in 0_u8..9,
            marker in 10_u8..=u8::MAX,
        ) {
            let key = [42; 32];
            let context = test_context();
            let cursor = create_cursor(context.clone(), vec![], 1_000_000, &key);
            let mut mutated = context;
            match field {
                0 => mutated.repository = RepositoryId::from_bytes([marker; 16]),
                1 => mutated.generation = GenerationId::from_bytes([marker; 20]),
                2 => mutated.tool = McpTool::RepoList,
                3 => mutated.tool_major_version = u16::from(marker),
                4 => mutated.query_fingerprint = [marker; 32],
                5 => mutated.plan_fingerprint = [marker; 32],
                6 => mutated.response_profile = ResponseProfile::Evidence,
                7 => mutated.snapshot_id = [marker; 32],
                _ => mutated.page_size = u16::from(marker) + 1_000,
            }

            prop_assert!(cursor.validate(&mutated, 1_100_000, &key).is_err());
        }
    }
}
