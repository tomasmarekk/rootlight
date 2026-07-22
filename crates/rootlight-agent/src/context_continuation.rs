//! Bounded continuation state for deterministic context-pack resumption.
//!
//! The state contains only source-free digests and counters. Authentication,
//! expiry, authorization profile, and repository-generation binding remain the
//! responsibility of the composing MCP adapter.

use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_mcp_contract::vertical::{ContinuationCursor, ResponseProfile};

/// Wire version of the private context continuation payload.
pub const CONTEXT_CONTINUATION_STATE_VERSION: u8 = 2;

const FIXED_STATE_BYTES: usize = 1 + 2 + 4 + 32 + 32 + 32 + 2 + 2 + 4 + 2;
const MAX_FRONTIER_PAGES: usize = 2_048;

/// Canonical request dimensions an authenticated continuation must bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextContinuationBinding {
    /// Exact repository identity.
    pub repository: RepositoryId,
    /// Exact immutable generation identity.
    pub generation: GenerationId,
    /// Canonical source-free request digest.
    pub request_digest: [u8; 32],
    /// Selected representation profile.
    pub response_profile: ResponseProfile,
    /// Original output-token ceiling.
    pub token_budget: u16,
    /// Context planner version.
    pub planner_version: u32,
    /// Objective-role policy version.
    pub role_policy_version: u32,
}

/// Source-free optimizer frontier carried inside an authenticated cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextContinuationState {
    next_page: u16,
    output_budget: u32,
    corpus_digest: [u8; 32],
    page_start_digest: [u8; 32],
    page_start_count: u16,
    emitted_digest: [u8; 32],
    emitted_count: u16,
    remaining_candidates: u32,
    page_item_counts: Vec<u8>,
}

/// Checked inputs used to create one continuation frontier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextContinuationStateParts {
    /// Zero-based page to produce on resume.
    pub next_page: u16,
    /// Exact optimizer token capacity retained from the first page.
    pub output_budget: u32,
    /// Digest of the deterministic candidate corpus.
    pub corpus_digest: [u8; 32],
    /// Hash-chain digest before the current page was emitted.
    pub page_start_digest: [u8; 32],
    /// Number of identities emitted before the current page.
    pub page_start_count: u16,
    /// Hash-chain digest after the current page was emitted.
    pub emitted_digest: [u8; 32],
    /// Total identities emitted through the current page.
    pub emitted_count: u16,
    /// Candidates still available after the current page.
    pub remaining_candidates: u32,
    /// Authenticated item count for every emitted page.
    pub page_item_counts: Vec<u8>,
}

impl ContextContinuationState {
    /// Creates the next deterministic context-pack frontier.
    ///
    /// # Errors
    ///
    /// Returns [`ContextContinuationError::Invalid`] when the frontier is
    /// empty, unbounded, or internally inconsistent.
    pub fn new(parts: ContextContinuationStateParts) -> Result<Self, ContextContinuationError> {
        let ContextContinuationStateParts {
            next_page,
            output_budget,
            corpus_digest,
            page_start_digest,
            page_start_count,
            emitted_digest,
            emitted_count,
            remaining_candidates,
            page_item_counts,
        } = parts;
        if !valid_frontier_counts(
            next_page,
            page_start_count,
            emitted_count,
            &page_item_counts,
        ) || output_budget == 0
            || remaining_candidates == 0
        {
            return Err(ContextContinuationError::Invalid);
        }
        Ok(Self {
            next_page,
            output_budget,
            corpus_digest,
            page_start_digest,
            page_start_count,
            emitted_digest,
            emitted_count,
            remaining_candidates,
            page_item_counts,
        })
    }

    /// Returns the zero-based page to produce on resume.
    #[must_use]
    pub const fn next_page(&self) -> u16 {
        self.next_page
    }

    /// Returns the exact optimizer token capacity observed on the first page.
    #[must_use]
    pub const fn output_budget(&self) -> u32 {
        self.output_budget
    }

    /// Returns the digest of the deterministic candidate corpus.
    #[must_use]
    pub const fn corpus_digest(&self) -> [u8; 32] {
        self.corpus_digest
    }

    /// Returns the hash-chain digest of previously emitted identities.
    #[must_use]
    pub const fn emitted_digest(&self) -> [u8; 32] {
        self.emitted_digest
    }

    /// Returns the number of previously emitted identities.
    #[must_use]
    pub const fn emitted_count(&self) -> u16 {
        self.emitted_count
    }

    /// Returns the number of candidates remaining at cursor issue time.
    #[must_use]
    pub const fn remaining_candidates(&self) -> u32 {
        self.remaining_candidates
    }

    /// Returns the authenticated number of items emitted by each prior page.
    #[must_use]
    pub fn page_item_counts(&self) -> &[u8] {
        &self.page_item_counts
    }

    /// Defers trailing optional items after exact final-representation sizing.
    ///
    /// # Errors
    ///
    /// Returns [`ContextContinuationError::Invalid`] if the supplied retained
    /// identities do not describe the current authenticated page.
    pub fn retain_current_page(
        &mut self,
        retained_identities: &[String],
    ) -> Result<(), ContextContinuationError> {
        let current_count = self
            .page_item_counts
            .last_mut()
            .ok_or(ContextContinuationError::Invalid)?;
        if retained_identities.is_empty() || retained_identities.len() > usize::from(*current_count)
        {
            return Err(ContextContinuationError::Invalid);
        }
        let retained_count = u8::try_from(retained_identities.len())
            .map_err(|_| ContextContinuationError::Invalid)?;
        let removed = current_count.saturating_sub(retained_count);
        self.emitted_count = self
            .page_start_count
            .checked_add(u16::from(retained_count))
            .ok_or(ContextContinuationError::Invalid)?;
        self.remaining_candidates = self
            .remaining_candidates
            .checked_add(u32::from(removed))
            .ok_or(ContextContinuationError::Invalid)?;
        self.emitted_digest = extend_identity_digest(self.page_start_digest, retained_identities);
        *current_count = retained_count;
        Ok(())
    }

    /// Encodes the bounded private state for an authenticated cursor payload.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut output =
            Vec::with_capacity(FIXED_STATE_BYTES.saturating_add(self.page_item_counts.len()));
        output.push(CONTEXT_CONTINUATION_STATE_VERSION);
        output.extend_from_slice(&self.next_page.to_le_bytes());
        output.extend_from_slice(&self.output_budget.to_le_bytes());
        output.extend_from_slice(&self.corpus_digest);
        output.extend_from_slice(&self.page_start_digest);
        output.extend_from_slice(&self.page_start_count.to_le_bytes());
        output.extend_from_slice(&self.emitted_digest);
        output.extend_from_slice(&self.emitted_count.to_le_bytes());
        output.extend_from_slice(&self.remaining_candidates.to_le_bytes());
        output.extend_from_slice(
            &u16::try_from(self.page_item_counts.len())
                .unwrap_or(u16::MAX)
                .to_le_bytes(),
        );
        for count in &self.page_item_counts {
            output.push(*count);
        }
        output
    }

    /// Decodes one exact bounded private state.
    ///
    /// # Errors
    ///
    /// Returns [`ContextContinuationError::Invalid`] for malformed, trailing,
    /// unsupported-version, empty-frontier, or impossible-page payloads.
    pub fn decode(input: &[u8]) -> Result<Self, ContextContinuationError> {
        if input.len() < FIXED_STATE_BYTES
            || input.first().copied() != Some(CONTEXT_CONTINUATION_STATE_VERSION)
        {
            return Err(ContextContinuationError::Invalid);
        }
        let mut offset = 1;
        let next_page = read_u16(input, &mut offset)?;
        let output_budget = read_u32(input, &mut offset)?;
        let corpus_digest = read_array(input, &mut offset)?;
        let page_start_digest = read_array(input, &mut offset)?;
        let page_start_count = read_u16(input, &mut offset)?;
        let emitted_digest = read_array(input, &mut offset)?;
        let emitted_count = read_u16(input, &mut offset)?;
        let remaining_candidates = read_u32(input, &mut offset)?;
        let page_count = usize::from(read_u16(input, &mut offset)?);
        let mut page_item_counts = Vec::with_capacity(page_count);
        for _ in 0..page_count {
            page_item_counts.push(read_u8(input, &mut offset)?);
        }
        if output_budget == 0
            || remaining_candidates == 0
            || !valid_frontier_counts(
                next_page,
                page_start_count,
                emitted_count,
                &page_item_counts,
            )
            || offset != input.len()
        {
            return Err(ContextContinuationError::Invalid);
        }
        Ok(Self {
            next_page,
            output_budget,
            corpus_digest,
            page_start_digest,
            page_start_count,
            emitted_digest,
            emitted_count,
            remaining_candidates,
            page_item_counts,
        })
    }
}

fn valid_frontier_counts(
    next_page: u16,
    page_start_count: u16,
    emitted_count: u16,
    page_item_counts: &[u8],
) -> bool {
    if next_page == 0
        || page_item_counts.len() != usize::from(next_page)
        || page_item_counts.len() > MAX_FRONTIER_PAGES
        || page_item_counts
            .iter()
            .any(|count| *count == 0 || *count > 200)
    {
        return false;
    }
    let total = page_item_counts.iter().copied().map(u32::from).sum::<u32>();
    let prior = page_item_counts
        .iter()
        .take(page_item_counts.len().saturating_sub(1))
        .copied()
        .map(u32::from)
        .sum::<u32>();
    total == u32::from(emitted_count) && prior == u32::from(page_start_count)
}

fn read_u8(input: &[u8], offset: &mut usize) -> Result<u8, ContextContinuationError> {
    let value = input
        .get(*offset)
        .copied()
        .ok_or(ContextContinuationError::Invalid)?;
    *offset = offset
        .checked_add(1)
        .ok_or(ContextContinuationError::Invalid)?;
    Ok(value)
}

/// Extends the deterministic emitted-identity hash chain.
#[must_use]
pub fn extend_identity_digest(mut digest: [u8; 32], identities: &[String]) -> [u8; 32] {
    for identity in identities {
        let mut hasher =
            blake3::Hasher::new_derive_key("rootlight.context-pack.emitted-identity-chain.v1");
        hasher.update(&digest);
        hasher.update(
            &u64::try_from(identity.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        hasher.update(identity.as_bytes());
        digest = *hasher.finalize().as_bytes();
    }
    digest
}

/// Failure to open, validate, or seal a context continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ContextContinuationError {
    /// The cursor or its private state is malformed, expired, or mismatched.
    #[error("context continuation is invalid")]
    Invalid,
    /// The adapter could not safely issue a bounded cursor.
    #[error("context continuation could not be issued")]
    Unavailable,
}

/// Adapter boundary for authenticated context continuation cursors.
pub trait ContextContinuationCodec: Send + Sync + 'static {
    /// Opens and validates a cursor against the exact canonical request.
    ///
    /// # Errors
    ///
    /// Returns [`ContextContinuationError`] if authentication, expiry, request
    /// binding, or private-state decoding fails.
    fn open_context_continuation(
        &self,
        cursor: &ContinuationCursor,
        binding: ContextContinuationBinding,
    ) -> Result<ContextContinuationState, ContextContinuationError>;

    /// Authenticates one bounded next-page state for public transport.
    ///
    /// # Errors
    ///
    /// Returns [`ContextContinuationError`] when the adapter cannot issue a
    /// bounded authenticated cursor.
    fn seal_context_continuation(
        &self,
        state: ContextContinuationState,
        binding: ContextContinuationBinding,
    ) -> Result<ContinuationCursor, ContextContinuationError>;
}

fn read_u16(input: &[u8], offset: &mut usize) -> Result<u16, ContextContinuationError> {
    let end = offset
        .checked_add(2)
        .ok_or(ContextContinuationError::Invalid)?;
    let bytes = input
        .get(*offset..end)
        .ok_or(ContextContinuationError::Invalid)?
        .try_into()
        .map_err(|_| ContextContinuationError::Invalid)?;
    *offset = end;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(input: &[u8], offset: &mut usize) -> Result<u32, ContextContinuationError> {
    let end = offset
        .checked_add(4)
        .ok_or(ContextContinuationError::Invalid)?;
    let bytes = input
        .get(*offset..end)
        .ok_or(ContextContinuationError::Invalid)?
        .try_into()
        .map_err(|_| ContextContinuationError::Invalid)?;
    *offset = end;
    Ok(u32::from_le_bytes(bytes))
}

fn read_array(input: &[u8], offset: &mut usize) -> Result<[u8; 32], ContextContinuationError> {
    let end = offset
        .checked_add(32)
        .ok_or(ContextContinuationError::Invalid)?;
    let bytes = input
        .get(*offset..end)
        .ok_or(ContextContinuationError::Invalid)?
        .try_into()
        .map_err(|_| ContextContinuationError::Invalid)?;
    *offset = end;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{
        ContextContinuationError, ContextContinuationState, ContextContinuationStateParts,
    };
    use rootlight_ids::{GenerationId, RepositoryId};
    use rootlight_mcp_contract::{
        ExposureProfile, McpTool,
        pagination::{AuthenticatedCursor, CursorContext, MAX_CURSOR_BYTES},
        vertical::ResponseProfile,
    };

    fn state() -> ContextContinuationState {
        ContextContinuationState::new(ContextContinuationStateParts {
            next_page: 2,
            output_budget: 800,
            corpus_digest: [3; 32],
            page_start_digest: [8; 32],
            page_start_count: 3,
            emitted_digest: [4; 32],
            emitted_count: 7,
            remaining_candidates: 9,
            page_item_counts: vec![3, 4],
        })
        .expect("fixture state is valid")
    }

    #[test]
    fn bounded_state_round_trips_exactly() {
        let encoded = state().encode();
        assert_eq!(ContextContinuationState::decode(&encoded), Ok(state()));
    }

    #[test]
    fn state_rejects_version_trailing_and_empty_frontier_mutations() {
        let mut wrong_version = state().encode();
        wrong_version[0] = 3;
        assert_eq!(
            ContextContinuationState::decode(&wrong_version),
            Err(ContextContinuationError::Invalid)
        );

        let mut trailing = state().encode();
        trailing.push(0);
        assert_eq!(
            ContextContinuationState::decode(&trailing),
            Err(ContextContinuationError::Invalid)
        );

        let mut no_remaining = state().encode();
        const REMAINING_OFFSET: usize = 1 + 2 + 4 + 32 + 32 + 2 + 32 + 2;
        no_remaining[REMAINING_OFFSET..REMAINING_OFFSET + 4].copy_from_slice(&0_u32.to_le_bytes());
        assert_eq!(
            ContextContinuationState::decode(&no_remaining),
            Err(ContextContinuationError::Invalid)
        );

        let mut inconsistent_page_total = state().encode();
        let last = inconsistent_page_total
            .last_mut()
            .expect("fixture contains page counts");
        *last = last.saturating_add(1);
        assert_eq!(
            ContextContinuationState::decode(&inconsistent_page_total),
            Err(ContextContinuationError::Invalid)
        );

        let mut inconsistent_page_start = state().encode();
        const PAGE_START_COUNT_OFFSET: usize = 1 + 2 + 4 + 32 + 32;
        inconsistent_page_start[PAGE_START_COUNT_OFFSET..PAGE_START_COUNT_OFFSET + 2]
            .copy_from_slice(&2_u16.to_le_bytes());
        assert_eq!(
            ContextContinuationState::decode(&inconsistent_page_start),
            Err(ContextContinuationError::Invalid)
        );
    }

    #[test]
    fn maximum_admitted_frontier_fits_the_authenticated_cursor_wire_bound() {
        let state = ContextContinuationState::new(ContextContinuationStateParts {
            next_page: 2_048,
            output_budget: 500,
            corpus_digest: [3; 32],
            page_start_digest: [4; 32],
            page_start_count: 2_047,
            emitted_digest: [5; 32],
            emitted_count: 2_048,
            remaining_candidates: 1,
            page_item_counts: vec![1; 2_048],
        })
        .expect("maximum admitted frontier is valid");
        let cursor = AuthenticatedCursor::create(
            CursorContext {
                repository: RepositoryId::from_bytes([1; 16]),
                generation: GenerationId::from_bytes([2; 20]),
                tool: McpTool::ContextPack,
                tool_major_version: 1,
                query_fingerprint: [6; 32],
                plan_fingerprint: [7; 32],
                response_profile: ResponseProfile::Compact,
                exposure_profile: ExposureProfile::Developer,
                snapshot_id: [8; 32],
                page_size: 500,
                key_id: 9,
            },
            state.encode(),
            1_000,
            &[10; 32],
        )
        .expect("maximum context frontier remains sealable");

        assert!(cursor.to_wire().len() <= MAX_CURSOR_BYTES);
    }
}
