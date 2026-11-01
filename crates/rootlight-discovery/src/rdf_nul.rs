//! Bind raw NUL admission to complete source bytes and the selected native grammar.
//! N-Triples/N-Quads use bounded physical lines; Turtle/TriG use a bounded document.

use std::{cell::Cell, ops::ControlFlow};

use rootlight_cancel::Cancellation;
use rootlight_ids::ContentHash;
use rootlight_vfs::{StreamingSourceTextValidator, TextEncoding, VfsError};

use crate::{DiscoveryError, NTriplesNulProbe, NTriplesNulProof};

const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;

/// Native grammar selected by source routing before NUL evidence is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeNulGrammar {
    /// Physical-line N-Triples syntax.
    NTriples,
    /// Physical-line standard N-Quads syntax.
    NQuads,
    /// Complete Turtle document syntax.
    Turtle,
    /// Complete TriG document syntax, including graph blocks.
    Trig,
}

/// Complete source identity and lexical evidence bound to its requested grammar.
#[derive(Debug, Clone)]
pub struct NativeNulProof {
    grammar: NativeNulGrammar,
    evidence: Evidence,
}

#[derive(Debug, Clone)]
enum Evidence {
    Line(NTriplesNulProof),
    Document {
        hash: ContentHash,
        bytes: u64,
        permits_nul: bool,
        refused: bool,
    },
}

impl NativeNulProof {
    pub(crate) fn from_ntriples(proof: NTriplesNulProof) -> Self {
        Self {
            grammar: NativeNulGrammar::NTriples,
            evidence: Evidence::Line(proof),
        }
    }

    /// Returns the selected grammar authenticated with this evidence.
    #[must_use]
    pub const fn grammar(&self) -> NativeNulGrammar {
        self.grammar
    }

    /// Checks the complete physical hash and byte length.
    #[must_use]
    pub fn matches_source(&self, hash: ContentHash, bytes: u64) -> bool {
        match &self.evidence {
            Evidence::Line(proof) => proof.matches_source(hash, bytes),
            Evidence::Document {
                hash: observed_hash,
                bytes: observed_bytes,
                ..
            } => *observed_hash == hash && *observed_bytes == bytes,
        }
    }

    /// Checks routing context together with the complete captured source identity.
    #[must_use]
    pub fn matches_request(
        &self,
        grammar: NativeNulGrammar,
        hash: ContentHash,
        bytes: u64,
    ) -> bool {
        self.grammar == grammar && self.matches_source(hash, bytes)
    }

    /// Whether every NUL has clean native string/comment membership.
    #[must_use]
    pub const fn permits_nul(&self) -> bool {
        match &self.evidence {
            Evidence::Line(proof) => proof.permits_nul(),
            Evidence::Document { permits_nul, .. } => *permits_nul,
        }
    }

    /// Whether a source, native parser, input, or traversal bound refused evidence.
    #[must_use]
    pub const fn was_refused(&self) -> bool {
        match &self.evidence {
            Evidence::Line(proof) => proof.was_refused(),
            Evidence::Document { refused, .. } => *refused,
        }
    }
}

impl From<NTriplesNulProof> for NativeNulProof {
    fn from(proof: NTriplesNulProof) -> Self {
        Self::from_ntriples(proof)
    }
}

/// Incremental identity accounting with grammar-specific bounded native evidence.
pub struct NativeNulProbe(Probe);

enum Probe {
    Line {
        grammar: NativeNulGrammar,
        probe: NTriplesNulProbe,
    },
    Document(DocumentProbe),
}

impl NativeNulProbe {
    /// Starts a proof, retaining at most 8 MiB for a contextual document.
    #[must_use]
    pub fn new(grammar: NativeNulGrammar) -> Self {
        Self(match grammar {
            NativeNulGrammar::NTriples => Probe::Line {
                grammar,
                probe: NTriplesNulProbe::new(),
            },
            NativeNulGrammar::NQuads => Probe::Line {
                grammar,
                probe: NTriplesNulProbe::for_nquads(),
            },
            NativeNulGrammar::Turtle | NativeNulGrammar::Trig => {
                Probe::Document(DocumentProbe::new(grammar))
            }
        })
    }

    /// Accounts for ordered physical bytes without treating chunk boundaries as EOF.
    /// Hashing and UTF-8 validation continue after native source-budget refusal.
    ///
    /// # Errors
    /// Returns cancellation, allocation failure, or byte-accounting overflow.
    pub fn push(
        &mut self,
        bytes: &[u8],
        cancellation: &Cancellation,
    ) -> Result<(), DiscoveryError> {
        match &mut self.0 {
            Probe::Line { probe, .. } => probe.push(bytes, cancellation),
            Probe::Document(probe) => {
                let result = probe.push(bytes, cancellation);
                if result.is_err() {
                    probe.valid = false;
                }
                result
            }
        }
    }

    /// Finalizes only at physical EOF, preserving identity for negative evidence.
    ///
    /// # Errors
    /// Returns cancellation or native parser setup failure.
    pub fn finish(self, cancellation: &Cancellation) -> Result<NativeNulProof, DiscoveryError> {
        match self.0 {
            Probe::Line { grammar, probe } => Ok(NativeNulProof {
                grammar,
                evidence: Evidence::Line(probe.finish(cancellation)?),
            }),
            Probe::Document(probe) => probe.finish(cancellation),
        }
    }
}

#[derive(Clone, Copy)]
struct NativeLimits {
    source_bytes: usize,
    input_bytes: usize,
    input_calls: usize,
    progress_polls: usize,
    nodes: usize,
}

impl Default for NativeLimits {
    fn default() -> Self {
        Self {
            source_bytes: MAX_SOURCE_BYTES,
            input_bytes: MAX_SOURCE_BYTES * 8,
            input_calls: 65_536,
            progress_polls: 4096,
            nodes: 262_144,
        }
    }
}

struct DocumentProbe {
    grammar: NativeNulGrammar,
    hash: blake3::Hasher,
    text: StreamingSourceTextValidator,
    bytes: u64,
    source: Vec<u8>,
    saw_nul: bool,
    valid: bool,
    refused: bool,
    limits: NativeLimits,
}

impl DocumentProbe {
    fn new(grammar: NativeNulGrammar) -> Self {
        Self {
            grammar,
            hash: blake3::Hasher::new(),
            text: StreamingSourceTextValidator::new(),
            bytes: 0,
            source: Vec::new(),
            saw_nul: false,
            valid: true,
            refused: false,
            limits: NativeLimits::default(),
        }
    }

    fn push(&mut self, bytes: &[u8], cancellation: &Cancellation) -> Result<(), DiscoveryError> {
        for block in bytes.chunks(4096) {
            cancellation.check()?;
            self.bytes = self
                .bytes
                .checked_add(
                    u64::try_from(block.len()).map_err(|_| DiscoveryError::IncrementalDrift)?,
                )
                .ok_or(DiscoveryError::IncrementalDrift)?;
            self.hash.update(block);
            self.text.push(block, cancellation)?;
            self.saw_nul |= block.contains(&0);
            if !self.valid || self.refused {
                continue;
            }
            if self
                .source
                .len()
                .checked_add(block.len())
                .is_none_or(|length| length > self.limits.source_bytes)
            {
                self.refused = true;
                self.source.clear();
                continue;
            }
            let required = self.source.len() + block.len();
            if required > self.source.capacity() {
                let capacity = self
                    .source
                    .capacity()
                    .saturating_mul(2)
                    .max(4096)
                    .max(required)
                    .min(self.limits.source_bytes);
                self.source
                    .try_reserve_exact(capacity - self.source.len())
                    .map_err(|_| DiscoveryError::Vfs(VfsError::MemoryUnavailable))?;
            }
            self.source.extend_from_slice(block);
        }
        cancellation.check()?;
        Ok(())
    }

    fn finish(mut self, cancellation: &Cancellation) -> Result<NativeNulProof, DiscoveryError> {
        cancellation.check()?;
        let utf8 = self.text.finish() == Some(TextEncoding::Utf8);
        let permits_nul = if utf8 && self.saw_nul && self.valid && !self.refused {
            let source = self
                .source
                .strip_prefix(b"\xef\xbb\xbf")
                .unwrap_or(&self.source);
            let outcome = native_document(source, self.limits, cancellation)?;
            self.refused |= outcome.refused;
            outcome.permits_nul
        } else {
            false
        };
        Ok(NativeNulProof {
            grammar: self.grammar,
            evidence: Evidence::Document {
                hash: ContentHash::from_bytes(*self.hash.finalize().as_bytes()),
                bytes: self.bytes,
                permits_nul,
                refused: self.refused,
            },
        })
    }
}

#[derive(Default)]
struct NativeOutcome {
    permits_nul: bool,
    refused: bool,
}

fn next_nul(
    bytes: &[u8],
    start: usize,
    cancellation: &Cancellation,
) -> Result<Option<usize>, DiscoveryError> {
    let Some(tail) = bytes.get(start..) else {
        return Ok(None);
    };
    for (index, chunk) in tail.chunks(4096).enumerate() {
        cancellation.check()?;
        if let Some(offset) = chunk.iter().position(|byte| *byte == 0) {
            return Ok(Some(start + index * 4096 + offset));
        }
    }
    Ok(None)
}

fn native_document(
    bytes: &[u8],
    limits: NativeLimits,
    cancellation: &Cancellation,
) -> Result<NativeOutcome, DiscoveryError> {
    cancellation.check()?;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_turtle::LANGUAGE.into())
        .map_err(|_| DiscoveryError::StreamedCaptureUnavailable)?;
    let refused = Cell::new(false);
    let mut polls = 0usize;
    let mut progress = |_: &tree_sitter::ParseState| {
        polls = polls.saturating_add(1);
        if polls > limits.progress_polls {
            refused.set(true);
        }
        if refused.get() || cancellation.check().is_err() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut calls = 0usize;
    let mut read_bytes = 0usize;
    let mut input = |offset: usize, _: tree_sitter::Point| {
        calls = calls.saturating_add(1);
        let block = bytes
            .get(offset..offset.saturating_add(4096).min(bytes.len()))
            .unwrap_or_default();
        read_bytes = read_bytes.saturating_add(block.len());
        if calls > limits.input_calls
            || read_bytes > limits.input_bytes
            || cancellation.check().is_err()
        {
            refused.set(true);
            &[]
        } else {
            block
        }
    };
    let tree = parser.parse_with_options(
        &mut input,
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
    );
    cancellation.check()?;
    if refused.get() {
        return Ok(NativeOutcome {
            refused: true,
            ..NativeOutcome::default()
        });
    }
    let Some(tree) = tree else {
        return Ok(NativeOutcome::default());
    };
    let root = tree.root_node();
    if root.kind() != "document" || root.has_error() || root.byte_range() != (0..bytes.len()) {
        return Ok(NativeOutcome::default());
    }
    let mut cursor = tree.walk();
    let mut nodes = 0usize;
    let mut nul = next_nul(bytes, 0, cancellation)?;
    loop {
        cancellation.check()?;
        nodes = nodes.saturating_add(1);
        if nodes > limits.nodes {
            return Ok(NativeOutcome {
                refused: true,
                ..NativeOutcome::default()
            });
        }
        let node = cursor.node();
        if node.is_error()
            || node.is_missing()
            || nul.is_some_and(|offset| offset < node.start_byte())
        {
            return Ok(NativeOutcome::default());
        }
        if matches!(node.kind(), "string" | "comment") && !node.has_error() {
            while let Some(offset) =
                nul.filter(|offset| *offset >= node.start_byte() && *offset < node.end_byte())
            {
                nul = next_nul(bytes, offset + 1, cancellation)?;
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(NativeOutcome {
                    permits_nul: nul.is_none(),
                    refused: false,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::content_hash;

    #[test]
    fn nquads_line_nul_proof_is_complete_chunk_independent_and_grammar_bound() {
        for source in [
            b"<s:><p:>\"a\0b\"<g:>.\r\n# c\0\n".as_slice(),
            b"_:s <p:> \"a\0b\" _:g .\n",
            b"<s:><p:>\"a\0b\".\n",
            "\u{feff}<s:><p:>\"λ\0😀\"<g:>.\r\n".as_bytes(),
        ] {
            for split in 0..=source.len() {
                let cancellation = Cancellation::new();
                let mut probe = NativeNulProbe::new(NativeNulGrammar::NQuads);
                probe.push(&source[..split], &cancellation).unwrap();
                probe.push(&source[split..], &cancellation).unwrap();
                let proof = probe.finish(&cancellation).unwrap();
                let bytes = u64::try_from(source.len()).unwrap();
                assert!(proof.permits_nul(), "{source:?}");
                assert!(!proof.was_refused());
                assert!(proof.matches_request(
                    NativeNulGrammar::NQuads,
                    content_hash(source),
                    bytes
                ));
                assert!(!proof.matches_request(
                    NativeNulGrammar::NTriples,
                    content_hash(source),
                    bytes
                ));
                assert!(!proof.matches_request(
                    NativeNulGrammar::Turtle,
                    content_hash(source),
                    bytes
                ));
                assert!(!proof.matches_source(content_hash(b"stale"), bytes));
            }
        }
    }

    #[test]
    fn nquads_line_nul_proof_rejects_relative_ids_invalid_graphs_and_full_bad_tails() {
        for source in [
            b"<s><p:>\"a\0b\"<g:>.".as_slice(),
            b"<s:><p:>\"a\0b\"<g>.".as_slice(),
            b"<s:><p:>\"a\0b\"\"graph\".",
            b"<s:><p:>\"a\0b\"<g:>\0.",
            b"<s:><p:>\"a\0b\"<g:>.\n\xff",
            b"<s:><p:>\"a\0b\"<g:> <extra:>.",
        ] {
            let cancellation = Cancellation::new();
            let mut probe = NativeNulProbe::new(NativeNulGrammar::NQuads);
            for chunk in source.chunks(3) {
                probe.push(chunk, &cancellation).unwrap();
            }
            let proof = probe.finish(&cancellation).unwrap();
            assert!(!proof.permits_nul(), "{source:?}");
            assert!(
                proof.matches_source(content_hash(source), u64::try_from(source.len()).unwrap())
            );
        }
    }

    #[test]
    fn contextual_nul_proof_is_complete_chunk_independent_and_grammar_bound() {
        for (grammar, source) in [
            (
                NativeNulGrammar::Turtle,
                b"<s> <p> '''first\n#\0last''' .\n<tail> <p> <o> .".as_slice(),
            ),
            (
                NativeNulGrammar::Trig,
                b"GRAPH <g> { <s> <p> \"\"\"first\n#\0last\"\"\" . }\n# c\0\n<tail> <p> <o> ."
                    .as_slice(),
            ),
        ] {
            for split in 0..=source.len() {
                let cancellation = Cancellation::new();
                let mut probe = NativeNulProbe::new(grammar);
                probe.push(&source[..split], &cancellation).unwrap();
                probe.push(&source[split..], &cancellation).unwrap();
                let proof = probe.finish(&cancellation).unwrap();
                let bytes = u64::try_from(source.len()).unwrap();
                assert!(proof.permits_nul());
                assert!(!proof.was_refused());
                assert!(proof.matches_request(grammar, content_hash(source), bytes));
                assert!(!proof.matches_request(
                    NativeNulGrammar::NTriples,
                    content_hash(source),
                    bytes
                ));
                let other = if grammar == NativeNulGrammar::Turtle {
                    NativeNulGrammar::Trig
                } else {
                    NativeNulGrammar::Turtle
                };
                assert!(!proof.matches_request(other, content_hash(source), bytes));
                assert!(!proof.matches_source(content_hash(b"stale"), bytes));
            }
        }
    }

    #[test]
    fn contextual_nul_proof_rejects_bad_contexts_and_complete_invalid_tails() {
        for source in [
            b"<s>\0<p><o>.".as_slice(),
            b"<s> <p> <o\0> .",
            b"<s> <p> '\0\\q' .",
            b"<s> <p> '''first\n\0last",
            b"<s> <p> '\0\n' .",
            b"<s> <p> '\0' .\n\xff",
            b"GRAPH <g> { <s> <p> '\0' .",
        ] {
            let cancellation = Cancellation::new();
            let mut probe = NativeNulProbe::new(NativeNulGrammar::Trig);
            for chunk in source.chunks(3) {
                probe.push(chunk, &cancellation).unwrap();
            }
            let proof = probe.finish(&cancellation).unwrap();
            assert!(!proof.permits_nul(), "{source:?}");
            assert!(
                proof.matches_source(content_hash(source), u64::try_from(source.len()).unwrap())
            );
        }
    }

    #[test]
    fn contextual_nul_budgets_keep_complete_identity_and_never_mint_positive_evidence() {
        let source = b"<s> <p> '\0' .\n# tail\xff";
        let cancellation = Cancellation::new();
        let mut probe = DocumentProbe::new(NativeNulGrammar::Turtle);
        probe.limits.source_bytes = 8;
        for chunk in source.chunks(3) {
            probe.push(chunk, &cancellation).unwrap();
        }
        let proof = probe.finish(&cancellation).unwrap();
        assert!(proof.was_refused());
        assert!(!proof.permits_nul());
        assert!(proof.matches_source(content_hash(source), u64::try_from(source.len()).unwrap()));
        for limits in [
            NativeLimits {
                input_calls: 0,
                ..NativeLimits::default()
            },
            NativeLimits {
                input_bytes: 0,
                ..NativeLimits::default()
            },
            NativeLimits {
                nodes: 0,
                ..NativeLimits::default()
            },
        ] {
            let mut probe = DocumentProbe::new(NativeNulGrammar::Turtle);
            probe.limits = limits;
            probe.push(b"<s> <p> '\0' .", &cancellation).unwrap();
            let proof = probe.finish(&cancellation).unwrap();
            assert!(proof.was_refused());
            assert!(!proof.permits_nul());
        }
        let mut probe = NativeNulProbe::new(NativeNulGrammar::Turtle);
        probe.push(b"<s> <p> '\0' .", &cancellation).unwrap();
        assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(probe.finish(&cancellation).is_err());
    }

    #[test]
    fn contextual_nul_resident_capacity_and_native_progress_remain_bounded() {
        let cancellation = Cancellation::new();
        let source = vec![b'a'; MAX_SOURCE_BYTES];
        let mut probe = DocumentProbe::new(NativeNulGrammar::Turtle);
        for chunk in source.chunks(3000) {
            probe.push(chunk, &cancellation).unwrap();
            assert!(probe.source.capacity() <= MAX_SOURCE_BYTES);
        }
        assert_eq!(probe.source.len(), MAX_SOURCE_BYTES);
        let mut source = b"<s> <p> '\0' .\n".to_vec();
        source.extend_from_slice(&b"<s> <p> <o> .\n".repeat(4096));
        let mut probe = DocumentProbe::new(NativeNulGrammar::Turtle);
        probe.limits.progress_polls = 0;
        probe.push(&source, &cancellation).unwrap();
        let proof = probe.finish(&cancellation).unwrap();
        assert!(proof.was_refused());
        assert!(!proof.permits_nul());
        assert!(proof.matches_source(content_hash(&source), u64::try_from(source.len()).unwrap()));
    }
}
