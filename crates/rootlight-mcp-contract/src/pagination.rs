//! Opaque authenticated pagination cursors bound to a pinned generation.
//!
//! Cursors are integrity-protected envelopes that prevent scope changes,
//! generation mixing, and forgery across query boundaries. A cursor cannot
//! change scope, filters, confidence, budget semantics, or generation.

use rootlight_ids::{GenerationId, RepositoryId};

/// Maximum serialized cursor bytes accepted on the wire.
pub const MAX_CURSOR_BYTES: usize = 4_096;

/// Cursor validity window in milliseconds.
const CURSOR_TTL_MS: u64 = 300_000;

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
    /// Tool name that issued the cursor.
    pub tool_name: &'static str,
    /// Opaque query-shape fingerprint derived from normalized request parameters.
    pub query_fingerprint: [u8; 32],
    /// Requested page size at cursor creation time.
    pub page_size: u16,
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
    /// Integrity tag.
    tag: [u8; 32],
}

impl AuthenticatedCursor {
    /// Creates a new authenticated cursor bound to the given context.
    ///
    /// The server key is a process-local secret used for BLAKE3 keyed hashing.
    /// It rotates on daemon restart, gracefully invalidating old cursors.
    #[must_use]
    pub fn create(
        context: CursorContext,
        last_sort_key: Vec<u8>,
        issued_at_ms: u64,
        server_key: &[u8; 32],
    ) -> Self {
        let tag = compute_tag(&context, &last_sort_key, issued_at_ms, server_key);
        Self {
            context,
            last_sort_key,
            issued_at_ms,
            tag,
        }
    }

    /// Validates a cursor against the expected context and current time.
    ///
    /// # Errors
    ///
    /// Returns [CursorError] when the cursor is expired, bound to a
    /// different repository or generation, issued for a different query
    /// shape, or fails the integrity check.
    pub fn validate(
        &self,
        expected: &CursorContext,
        now_ms: u64,
        server_key: &[u8; 32],
    ) -> Result<(), CursorError> {
        if self.context.repository != expected.repository {
            return Err(CursorError::RepositoryMismatch);
        }
        if self.context.generation != expected.generation {
            return Err(CursorError::GenerationMismatch);
        }
        if self.context.tool_name != expected.tool_name
            || self.context.query_fingerprint != expected.query_fingerprint
            || self.context.page_size != expected.page_size
        {
            return Err(CursorError::QueryMismatch);
        }
        let elapsed = now_ms.saturating_sub(self.issued_at_ms);
        if elapsed > CURSOR_TTL_MS {
            return Err(CursorError::Expired);
        }
        let expected_tag = compute_tag(
            &self.context,
            &self.last_sort_key,
            self.issued_at_ms,
            server_key,
        );
        if expected_tag != self.tag {
            return Err(CursorError::IntegrityFailed);
        }
        Ok(())
    }

    /// Returns the opaque last-sort-key for page continuation.
    #[must_use]
    pub fn last_sort_key(&self) -> &[u8] {
        &self.last_sort_key
    }

    /// Returns the issue timestamp in Unix milliseconds.
    #[must_use]
    pub const fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    /// Serializes the cursor to an opaque wire string.
    ///
    /// The format is version-prefixed base64url without padding.
    #[must_use]
    pub fn to_wire(&self) -> String {
        let payload = self.serialize_payload();
        format!("c1.{}", base64url_encode(&payload))
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
        let payload = wire
            .strip_prefix("c1.")
            .ok_or(CursorError::Malformed)
            .and_then(|encoded| base64url_decode(encoded).ok_or(CursorError::Malformed))?;
        Self::deserialize_payload(&payload)
    }

    fn serialize_payload(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(self.context.repository.as_bytes());
        payload.extend_from_slice(self.context.generation.as_bytes());
        payload.extend_from_slice(self.context.tool_name.as_bytes());
        payload.push(0);
        payload.extend_from_slice(&self.context.query_fingerprint);
        payload.extend_from_slice(&self.context.page_size.to_le_bytes());
        payload.extend_from_slice(&self.issued_at_ms.to_le_bytes());
        payload.extend_from_slice(&(self.last_sort_key.len() as u16).to_le_bytes());
        payload.extend_from_slice(&self.last_sort_key);
        payload.extend_from_slice(&self.tag);
        payload
    }

    fn deserialize_payload(payload: &[u8]) -> Result<Self, CursorError> {
        const MIN_LEN: usize = 16 + 20 + 1 + 32 + 2 + 8 + 2 + 32;
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

        let query_fingerprint: [u8; 32] = payload
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

        let issued_at_ms = u64::from_le_bytes(
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

        let repository = RepositoryId::from_bytes(repo_bytes);
        let generation = GenerationId::from_bytes(gen_bytes);

        Ok(Self {
            context: CursorContext {
                repository,
                generation,
                tool_name: leak_tool_name(tool_name)?,
                query_fingerprint,
                page_size,
            },
            last_sort_key,
            issued_at_ms,
            tag,
        })
    }
}

/// Maps a deserialized tool name back to its static str reference.
fn leak_tool_name(name: &str) -> Result<&'static str, CursorError> {
    use crate::McpTool;
    for tool in McpTool::ALL {
        if tool.name() == name {
            return Ok(tool.name());
        }
    }
    Err(CursorError::Malformed)
}

/// Computes the BLAKE3 keyed integrity tag for a cursor.
fn compute_tag(
    context: &CursorContext,
    last_sort_key: &[u8],
    issued_at_ms: u64,
    server_key: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_keyed(server_key);
    hasher.update(context.repository.as_bytes());
    hasher.update(context.generation.as_bytes());
    hasher.update(context.tool_name.as_bytes());
    hasher.update(&[0]);
    hasher.update(&context.query_fingerprint);
    hasher.update(&context.page_size.to_le_bytes());
    hasher.update(&issued_at_ms.to_le_bytes());
    hasher.update(&(last_sort_key.len() as u16).to_le_bytes());
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
    let mut output = Vec::new();
    let bytes = input.as_bytes();
    for chunk in bytes.chunks(4) {
        let values: Vec<u8> = chunk
            .iter()
            .map(|&b| base64url_value(b))
            .collect::<Option<Vec<u8>>>()?;
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
    use rootlight_ids::{GenerationId, RepositoryId};

    fn test_context() -> CursorContext {
        CursorContext {
            repository: RepositoryId::from_bytes([1; 16]),
            generation: GenerationId::from_bytes([2; 20]),
            tool_name: "code.locate",
            query_fingerprint: [3; 32],
            page_size: 20,
        }
    }

    #[test]
    fn cursor_round_trips_through_wire_format() {
        let key = [42; 32];
        let context = test_context();
        let cursor = AuthenticatedCursor::create(context.clone(), vec![1, 2, 3], 1_000_000, &key);
        let wire = cursor.to_wire();
        assert!(wire.starts_with("c1."));
        assert!(wire.len() <= super::MAX_CURSOR_BYTES);

        let decoded = AuthenticatedCursor::from_wire(&wire).expect("wire decodes");
        assert_eq!(decoded, cursor);
        assert_eq!(decoded.last_sort_key(), &[1, 2, 3]);
        assert_eq!(decoded.issued_at_ms(), 1_000_000);
    }

    #[test]
    fn cursor_validates_against_matching_context() {
        let key = [42; 32];
        let context = test_context();
        let cursor = AuthenticatedCursor::create(context.clone(), vec![], 1_000_000, &key);
        assert!(cursor.validate(&context, 1_100_000, &key).is_ok());
    }

    #[test]
    fn cursor_rejects_generation_mismatch() {
        let key = [42; 32];
        let context = test_context();
        let cursor = AuthenticatedCursor::create(context, vec![], 1_000_000, &key);
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
        let cursor = AuthenticatedCursor::create(context, vec![], 1_000_000, &key);
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
        let cursor = AuthenticatedCursor::create(context.clone(), vec![], 1_000_000, &key);
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
        let cursor = AuthenticatedCursor::create(context.clone(), vec![], 1_000_000, &key);
        assert_eq!(
            cursor.validate(&context, 1_100_000, &wrong_key),
            Err(CursorError::IntegrityFailed)
        );
    }

    #[test]
    fn cursor_rejects_tampered_sort_key() {
        let key = [42; 32];
        let context = test_context();
        let mut cursor =
            AuthenticatedCursor::create(context.clone(), vec![1, 2, 3], 1_000_000, &key);
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
            AuthenticatedCursor::from_wire("c1."),
            Err(CursorError::Malformed)
        );
        assert_eq!(
            AuthenticatedCursor::from_wire("c2.AAAA"),
            Err(CursorError::Malformed)
        );
    }

    #[test]
    fn deterministic_page_equality_for_pinned_generation() {
        let key = [42; 32];
        let context = test_context();
        let cursor_a = AuthenticatedCursor::create(context.clone(), vec![10], 5_000, &key);
        let cursor_b = AuthenticatedCursor::create(context, vec![10], 5_000, &key);
        assert_eq!(cursor_a.to_wire(), cursor_b.to_wire());
    }
}
