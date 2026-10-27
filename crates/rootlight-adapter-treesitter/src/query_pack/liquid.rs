//! Admit exact Liquid loop endpoints under a complete native source census.
//! A refused census keeps an explicit source gap and never partial binding facts.

use super::{QueryCandidate, StructuralRole};
use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use std::collections::HashSet;
use tree_sitter::Node;

pub(super) struct CaptureCache<'source, 'tree> {
    source: &'source [u8],
    root: Node<'tree>,
    maximum: usize,
    complete: bool,
    initialized: bool,
    admitted: bool,
    exhausted: bool,
    definitions: HashSet<(usize, usize)>,
    reads: HashSet<(usize, usize)>,
}

impl<'source, 'tree> CaptureCache<'source, 'tree> {
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
            admitted: false,
            exhausted: false,
            definitions: HashSet::new(),
            reads: HashSet::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn initialize(&mut self, cancel: &Cancellation) -> Result<(), AdapterError> {
        self.initialized = true;
        let (census, exhausted) = crate::liquid_bindings::census_with_status(
            self.root,
            self.source,
            self.maximum,
            self.complete,
            cancel,
        )?;
        self.exhausted = exhausted;
        let Some(census) = census else {
            return Ok(());
        };
        self.definitions
            .try_reserve(census.definitions.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        self.reads
            .try_reserve(census.reads.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for definition in census.definitions {
            cancel.check()?;
            self.definitions.insert(definition.name);
        }
        for read in census.reads {
            cancel.check()?;
            self.reads.insert(read.name);
        }
        self.admitted = true;
        Ok(())
    }
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    cancel: &Cancellation,
    cache: &mut CaptureCache<'_, '_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancel.check()?;
    if !cache.initialized {
        cache.initialize(cancel)?
    }
    let key = (node.start_byte(), node.end_byte());
    let (role, syntax) = match role {
        StructuralRole::Root => (
            role,
            if cache.admitted {
                "liquid.source_file"
            } else {
                "liquid.native_context_unavailable"
            },
        ),
        StructuralRole::Module if cache.admitted => (role, "liquid.file"),
        StructuralRole::Module => return Ok(None),
        StructuralRole::Declaration if cache.definitions.contains(&key) => {
            (role, "liquid.loop_variable")
        }
        StructuralRole::Definition if cache.definitions.contains(&key) => {
            (role, "liquid.loop_name")
        }
        StructuralRole::Reference if cache.reads.contains(&key) => (role, "liquid.loop_value"),
        StructuralRole::Reference
            if cache.admitted && crate::liquid_bindings::value_identifier(node) =>
        {
            (role, "liquid.runtime_value_unavailable")
        }
        StructuralRole::Reference => return Ok(None),
        StructuralRole::Declaration | StructuralRole::Definition => return Ok(None),
        StructuralRole::Signature => (role, "liquid.runtime_construct_unavailable"),
        StructuralRole::StringLiteral => (role, "liquid.string"),
        StructuralRole::Comment => (role, "liquid.comment"),
        _ => return Ok(None),
    };
    let mut candidate = QueryCandidate {
        start: node.start_byte(),
        end: node.end_byte(),
        role,
        syntax,
        required: role.belongs_to_identity_closure() || syntax == "liquid.loop_value",
        native_depth: 0,
    };
    if cache.admitted && matches!(role, StructuralRole::Root | StructuralRole::Module) {
        candidate.start = 0;
        candidate.end = cache.source.len();
    }
    super::canonicalize_file_root_preamble(&mut candidate, node, cache.source, cache.complete);
    Ok(Some(candidate))
}
