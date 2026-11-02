//! Authenticate UTF-8 NUL contexts using the selected native RDF line grammar.
//! A proof covers complete captured bytes; only NUL-bearing physical lines are parsed.

use std::ops::ControlFlow;

use rootlight_cancel::Cancellation;
use rootlight_ids::ContentHash;
use rootlight_vfs::{StreamingSourceTextValidator, TextEncoding, VfsError};

use crate::DiscoveryError;

const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_NATIVE_BYTES: usize = 8 * 1024 * 1024;
const MAX_NATIVE_LINES: usize = 4096;

/// Finalized source identity and native lexical evidence for raw UTF-8 NULs.
/// The proof alone does not select a language or override binary signatures.
#[derive(Debug, Clone)]
pub struct NTriplesNulProof {
    hash: ContentHash,
    bytes: u64,
    permits_nul: bool,
    refused: bool,
}

impl NTriplesNulProof {
    /// Checks the entire finalized byte stream against its authenticated capture.
    #[must_use]
    pub fn matches_source(&self, hash: ContentHash, bytes: u64) -> bool {
        self.hash == hash && self.bytes == bytes
    }

    /// Whether every observed NUL has clean native string/comment membership.
    #[must_use]
    pub const fn permits_nul(&self) -> bool {
        self.permits_nul
    }

    /// Whether a bounded native line or aggregate work budget was exhausted.
    #[must_use]
    pub const fn was_refused(&self) -> bool {
        self.refused
    }
}

/// Incremental, bounded proof over the exact ordered bytes of one capture.
/// Invalid or over-budget native evidence never becomes a positive proof.
pub struct NTriplesNulProbe {
    language: tree_sitter::Language,
    hash: blake3::Hasher,
    text: StreamingSourceTextValidator,
    bytes: u64,
    line: Vec<u8>,
    line_overflow: bool,
    line_nul: bool,
    first_line: bool,
    saw_nul: bool,
    valid: bool,
    refused: bool,
    native_bytes: usize,
    native_lines: usize,
}

impl Default for NTriplesNulProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl NTriplesNulProbe {
    /// Starts a proof with at most 1 MiB of resident physical line bytes.
    #[must_use]
    pub fn new() -> Self {
        Self::with_language(rootlight_tree_sitter_ntriples::LANGUAGE.into())
    }

    // Legacy public proof conversion always means N-Triples; other grammars stay internal.
    pub(crate) fn for_nquads() -> Self {
        Self::with_language(rootlight_tree_sitter_nquads::LANGUAGE.into())
    }

    /// Keeps Dgraph lexical evidence separate from the legacy N-Triples proof route.
    pub(crate) fn for_nquads_dgraph() -> Self {
        Self::with_language(rootlight_tree_sitter_nquads_dgraph::LANGUAGE.into())
    }

    fn with_language(language: tree_sitter::Language) -> Self {
        Self {
            language,
            hash: blake3::Hasher::new(),
            text: StreamingSourceTextValidator::new(),
            bytes: 0,
            line: Vec::new(),
            line_overflow: false,
            line_nul: false,
            first_line: true,
            saw_nul: false,
            valid: true,
            refused: false,
            native_bytes: 0,
            native_lines: 0,
        }
    }

    /// Feeds ordered raw bytes without treating a chunk boundary as source EOF.
    ///
    /// # Errors
    /// Returns cancellation, allocation failure, or byte-accounting overflow.
    pub fn push(
        &mut self,
        bytes: &[u8],
        cancellation: &Cancellation,
    ) -> Result<(), DiscoveryError> {
        let result = self.push_chunks(bytes, cancellation);
        if result.is_err() {
            self.valid = false;
        }
        result
    }

    fn push_chunks(
        &mut self,
        bytes: &[u8],
        cancellation: &Cancellation,
    ) -> Result<(), DiscoveryError> {
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
            for part in block.split_inclusive(|byte| matches!(byte, b'\r' | b'\n')) {
                self.line_nul |= part.contains(&0);
                let available = MAX_LINE_BYTES.saturating_sub(self.line.len());
                let count = part.len().min(available);
                self.line
                    .try_reserve_exact(count)
                    .map_err(|_| DiscoveryError::Vfs(VfsError::MemoryUnavailable))?;
                self.line.extend_from_slice(&part[..count]);
                self.line_overflow |= count != part.len();
                if part
                    .last()
                    .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
                {
                    self.end_line(cancellation)?;
                }
            }
        }
        cancellation.check()?;
        Ok(())
    }

    /// Finalizes at actual source EOF, retaining identity even for refused evidence.
    ///
    /// # Errors
    /// Returns cancellation, allocation failure, or native parser setup failure.
    pub fn finish(
        mut self,
        cancellation: &Cancellation,
    ) -> Result<NTriplesNulProof, DiscoveryError> {
        cancellation.check()?;
        self.end_line(cancellation)?;
        let utf8 = self.text.finish() == Some(TextEncoding::Utf8);
        Ok(NTriplesNulProof {
            hash: ContentHash::from_bytes(*self.hash.finalize().as_bytes()),
            bytes: self.bytes,
            permits_nul: utf8 && self.saw_nul && self.valid && !self.refused,
            refused: self.refused,
        })
    }

    fn end_line(&mut self, cancellation: &Cancellation) -> Result<(), DiscoveryError> {
        if self.line_nul && self.valid && !self.refused {
            self.native_lines += 1;
            self.native_bytes = self.native_bytes.saturating_add(self.line.len());
            if self.line_overflow
                || self.native_lines > MAX_NATIVE_LINES
                || self.native_bytes > MAX_NATIVE_BYTES
            {
                self.refused = true;
            } else {
                let line = if self.first_line {
                    self.line
                        .strip_prefix(b"\xef\xbb\xbf")
                        .unwrap_or(&self.line)
                } else {
                    &self.line
                };
                self.valid = native_line(line, &self.language, cancellation)?;
            }
        }
        self.line.clear();
        self.line_nul = false;
        self.line_overflow = false;
        self.first_line = false;
        Ok(())
    }
}

fn native_line(
    bytes: &[u8],
    language: &tree_sitter::Language,
    cancellation: &Cancellation,
) -> Result<bool, DiscoveryError> {
    cancellation.check()?;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(language)
        .map_err(|_| DiscoveryError::StreamedCaptureUnavailable)?;
    let mut progress = |_: &tree_sitter::ParseState| {
        if cancellation.check().is_ok() {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    };
    let mut input = |offset: usize, _: tree_sitter::Point| {
        bytes
            .get(offset..offset.saturating_add(4096).min(bytes.len()))
            .unwrap_or_default()
    };
    let tree = parser.parse_with_options(
        &mut input,
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
    );
    cancellation.check()?;
    let Some(tree) = tree else { return Ok(false) };
    let root = tree.root_node();
    if root.has_error() || root.byte_range() != (0..bytes.len()) {
        return Ok(false);
    }
    // Native external token spans distinguish a legal NUL scalar from lexer EOF.
    for (offset, byte) in bytes.iter().enumerate() {
        if offset % 4096 == 0 {
            cancellation.check()?;
        }
        if *byte == 0
            && !root
                .descendant_for_byte_range(offset, offset + 1)
                .is_some_and(|node| {
                    matches!(node.kind(), "string" | "comment")
                        && !node.is_missing()
                        && !node.has_error()
                })
        {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::content_hash;

    #[test]
    fn native_nul_proof_is_complete_and_chunk_independent() {
        for source in [
            b"<s:><p:>\"a\0b\".\r\n# \0 comment\n".as_slice(),
            "<s:><p:>\"λ😀\0b\".\r\n# \0 comment\n".as_bytes(),
            "\u{feff}<s:><p:>\"λ\0😀\".\n".as_bytes(),
        ] {
            for split in 0..=source.len() {
                let cancel = Cancellation::new();
                let mut probe = NTriplesNulProbe::new();
                probe.push(&source[..split], &cancel).unwrap();
                probe.push(&source[split..], &cancel).unwrap();
                let proof = probe.finish(&cancel).unwrap();
                assert!(proof.permits_nul());
                assert!(
                    proof
                        .matches_source(content_hash(source), u64::try_from(source.len()).unwrap())
                );
                assert!(
                    !proof.matches_source(
                        content_hash(b"other"),
                        u64::try_from(source.len()).unwrap()
                    )
                );
            }
        }
    }

    #[test]
    fn native_nul_proof_rejects_foreign_contexts_and_keeps_late_bytes() {
        for source in [
            b"<s:>\0<p:><o:>.\n".as_slice(),
            b"<s:><p:><o:>.\0\n",
            b"<s:><p:>\"unclosed\0\n",
            b"<s:><p:>\"valid\0\".\n\xff",
            b"a\0b\0",
            b"\xff\xfea\0\0\0",
        ] {
            let cancel = Cancellation::new();
            let mut probe = NTriplesNulProbe::new();
            probe.push(source, &cancel).unwrap();
            let proof = probe.finish(&cancel).unwrap();
            assert!(!proof.permits_nul(), "{source:?}");
            assert!(!proof.was_refused());
        }
        let source = format!("{}<s:><p:>\"late\0\".\n", "# padding\n".repeat(10_000));
        let cancel = Cancellation::new();
        let mut probe = NTriplesNulProbe::new();
        for chunk in source.as_bytes().chunks(127) {
            probe.push(chunk, &cancel).unwrap();
        }
        let proof = probe.finish(&cancel).unwrap();
        assert!(proof.permits_nul());
        assert!(proof.matches_source(
            content_hash(source.as_bytes()),
            u64::try_from(source.len()).unwrap()
        ));
    }

    #[test]
    fn native_nul_bounds_and_cancellation_never_mint_positive_proof() {
        let mut source = vec![b'a'; MAX_LINE_BYTES + 1];
        source[0] = b'#';
        source[1] = 0;
        let cancel = Cancellation::new();
        let mut probe = NTriplesNulProbe::new();
        probe.push(&source, &cancel).unwrap();
        let proof = probe.finish(&cancel).unwrap();
        assert!(proof.was_refused());
        assert!(!proof.permits_nul());
        assert!(proof.matches_source(content_hash(&source), u64::try_from(source.len()).unwrap()));
        let cancel = Cancellation::new();
        let mut probe = NTriplesNulProbe::new();
        probe.push(b"<s:><p:>\"", &cancel).unwrap();
        assert!(cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(probe.push(b"\0\".\n", &cancel).is_err());
        assert!(probe.finish(&cancel).is_err());
    }
}
