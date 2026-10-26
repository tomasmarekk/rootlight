//! Admit separately budgeted companion References without changing Bash Reads.
//! Unsupported native owner paths are rejected before the allocation-free census;
//! every attempted proof consumes one shared, decreasing helper work counter.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use crate::bash_module_references::{self as native, Work};

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete_input: bool,
    work: Work,
}

impl<'source> CaptureCache<'source> {
    pub(super) const fn new(source: &'source [u8], maximum: usize, complete_input: bool) -> Self {
        Self {
            source,
            complete_input,
            work: Work::new(maximum),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.work.exhausted()
    }

    pub(super) fn syntax(
        &mut self,
        root: Node<'_>,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'static str>, AdapterError> {
        cancel.check()?;
        if !self.complete_input
            || !native::simple(node, self.source)
            || !native::eligible(
                root,
                node,
                self.source,
                self.complete_input,
                &mut self.work,
                cancel,
            )?
        {
            return Ok(None);
        }
        Ok(Some(
            if native::prove(
                root,
                node,
                self.source,
                self.complete_input,
                &mut self.work,
                cancel,
            )?
            .is_some()
            {
                "bash.module_reference"
            } else {
                "bash.unproven_module_reference"
            },
        ))
    }
}

#[cfg(test)]
mod tests;
