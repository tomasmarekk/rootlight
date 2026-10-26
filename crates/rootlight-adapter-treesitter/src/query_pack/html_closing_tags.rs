//! Keep closing-tag captures only after one complete, bounded native census.
//! Ordinary unsupported HTML withholds the new carriers without changing older
//! declarations; only actual proof-work exhaustion reports the query limit.

use std::collections::HashSet;

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

pub(super) struct CaptureCache<'source, 'tree> {
    source: &'source [u8],
    root: Node<'tree>,
    maximum: usize,
    complete: bool,
    initialized: bool,
    exhausted: bool,
    names: HashSet<(usize, usize)>,
}

impl<'source, 'tree> CaptureCache<'source, 'tree> {
    /// Bind this request's complete logical source and existing proof-work ceiling.
    pub(super) fn new(
        source: &'source [u8],
        root: Node<'tree>,
        maximum: usize,
        complete: bool,
    ) -> Self {
        Self {
            source,
            root,
            maximum,
            complete,
            initialized: false,
            exhausted: false,
            names: HashSet::new(),
        }
    }

    /// Report actual bounded proof-work exhaustion independently of unsupported syntax.
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn initialize(&mut self, cancel: &Cancellation) -> Result<(), AdapterError> {
        self.initialized = true;
        if !self.complete {
            return Ok(());
        }
        let (proofs, exhausted) = crate::html_closing_tags::census_with_status(
            self.root,
            self.source,
            self.maximum,
            cancel,
        )?;
        self.exhausted = exhausted;
        let Some(proofs) = proofs else {
            return Ok(());
        };
        self.names
            .try_reserve(proofs.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for proof in proofs {
            cancel.check()?;
            if !self.names.insert(proof.closing_name) {
                self.names.clear();
                return Ok(());
            }
        }
        Ok(())
    }

    /// Emit paired name carriers only for an explicit same-element closing name.
    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if !matches!(role, StructuralRole::Reference | StructuralRole::Signature)
            || node.kind() != "tag_name"
            || !node
                .parent()
                .is_some_and(|parent| parent.kind() == "end_tag")
        {
            return Ok(true);
        }
        if !self.initialized {
            self.initialize(cancel)?;
        }
        if !self.names.contains(&(node.start_byte(), node.end_byte())) {
            return Ok(false);
        }
        candidate.start = node.start_byte();
        candidate.end = node.end_byte();
        candidate.syntax = if role == StructuralRole::Reference {
            "html.closing_tag"
        } else {
            "html.closing_tag_context"
        };
        candidate.required = true;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
