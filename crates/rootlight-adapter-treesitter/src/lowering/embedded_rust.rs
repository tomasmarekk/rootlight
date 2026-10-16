//! Private Rust environments keep physical evidence within one written example.
//! The host's admitted facts and public code-block ownership remain unchanged.

use super::*;

#[derive(Default)]
pub(super) struct Plan {
    file: Option<(RustBindings, bool)>,
    examples: Vec<(RustBindings, bool)>,
    example_facts: HashMap<u64, usize>,
}

impl Plan {
    pub(super) fn standalone(bindings: RustBindings, complete: bool) -> Self {
        Self {
            file: Some((bindings, complete)),
            ..Self::default()
        }
    }

    pub(super) fn embedded(
        facts: &[SyntaxFact],
        diagnostics: &[AdapterDiagnostic],
        names: &BTreeMap<u64, String>,
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let mut result = Self::default();
        let mut unclipped = true;
        for diagnostic in diagnostics {
            cancellation.check()?;
            // A clean native tree does not prove that all binding barriers survived.
            if diagnostic.code().as_str() == "syntax-extraction-limit" {
                unclipped = false;
            }
        }
        for (block, group) in groups(facts, cancellation)? {
            cancellation.check()?;
            let Some(partition) = certified_partition(block, &group, facts, cancellation)? else {
                continue;
            };
            let bindings = RustBindings::new(&partition, names, maximum_name_bytes, cancellation)?;
            result
                .examples
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result
                .example_facts
                .try_reserve(group.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            let position = result.examples.len();
            for fact in group {
                cancellation.check()?;
                result.example_facts.insert(fact.local_id(), position);
            }
            result.examples.push((bindings, unclipped));
        }
        Ok(result)
    }

    pub(super) fn for_fact(&self, fact: u64) -> Option<&RustBindings> {
        self.file
            .as_ref()
            .map(|(bindings, _)| bindings)
            .or_else(|| {
                self.example_facts
                    .get(&fact)
                    .and_then(|position| self.examples.get(*position))
                    .map(|(bindings, _)| bindings)
            })
    }

    pub(super) fn proven_for_fact(&self, fact: u64) -> Option<&RustBindings> {
        match &self.file {
            Some((bindings, true)) => Some(bindings),
            Some((_, false)) => None,
            None => self
                .example_facts
                .get(&fact)
                .and_then(|position| self.examples.get(*position))
                .filter(|(_, complete)| *complete)
                .map(|(bindings, _)| bindings),
        }
    }
}

fn contains(owner: SourceSpan, child: SourceSpan) -> bool {
    owner.file() == child.file()
        && owner.start_byte() <= child.start_byte()
        && owner.end_byte() >= child.end_byte()
}

type ExampleGroup<'a> = (&'a SyntaxFact, Vec<&'a SyntaxFact>);

fn groups<'a>(
    facts: &'a [SyntaxFact],
    cancellation: &Cancellation,
) -> Result<Vec<ExampleGroup<'a>>, AdapterError> {
    let mut blocks = Vec::new();
    for fact in facts {
        cancellation.check()?;
        if fact.kind() == SyntaxFactKind::Module
            && fact.syntax_kind().as_str() == "markdown.code_block.module"
        {
            blocks
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            blocks.push(fact);
        }
    }
    crate::runtime::sort_cancellable_by(&mut blocks, cancellation, |a, b| {
        a.span().start_byte().cmp(&b.span().start_byte())
    })?;
    for neighbors in blocks.windows(2) {
        cancellation.check()?;
        if neighbors[0].span().end_byte() > neighbors[1].span().start_byte() {
            return Ok(Vec::new());
        }
    }
    let mut result: Vec<ExampleGroup<'_>> = Vec::new();
    result
        .try_reserve_exact(blocks.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    result.extend(blocks.into_iter().map(|block| (block, Vec::new())));
    for fact in facts {
        cancellation.check()?;
        if !fact.syntax_kind().as_str().starts_with("rust.") {
            continue;
        }
        let position = result
            .partition_point(|(block, _)| block.span().start_byte() <= fact.span().start_byte());
        let Some(position) = position.checked_sub(1) else {
            return Ok(Vec::new());
        };
        let (block, group) = &mut result[position];
        if !contains(block.span(), fact.span()) {
            return Ok(Vec::new());
        }
        group
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        group.push(fact);
    }
    Ok(result)
}

fn certified_partition(
    block: &SyntaxFact,
    group: &[&SyntaxFact],
    facts: &[SyntaxFact],
    cancellation: &Cancellation,
) -> Result<Option<Vec<SyntaxFact>>, AdapterError> {
    let mut signature = None;
    for fact in group {
        cancellation.check()?;
        if fact.kind() == SyntaxFactKind::Root {
            return Ok(None);
        }
        if fact.syntax_kind().as_str() == "rust.proof_context.signature"
            && (fact.kind() != SyntaxFactKind::Signature || signature.replace(*fact).is_some())
        {
            return Ok(None);
        }
    }
    let Some(signature) = signature else {
        return Ok(None);
    };
    let body = signature.span();
    if body.start_byte() >= body.end_byte()
        || !contains(block.span(), body)
        || !clean_owner(block, body, facts, cancellation)?
    {
        return Ok(None);
    }
    let by_id: HashMap<_, _> = group.iter().map(|fact| (fact.local_id(), *fact)).collect();
    if by_id.len() != group.len() || by_id.contains_key(&block.local_id()) {
        return Ok(None);
    }
    for fact in group {
        cancellation.check()?;
        if !contains(body, fact.span())
            || !owned_ancestry(fact, block.local_id(), &by_id, cancellation)?
        {
            return Ok(None);
        }
    }
    let mut result = Vec::new();
    result
        .try_reserve_exact(
            group
                .len()
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?,
        )
        .map_err(|_| SinkError::AllocationFailed)?;
    result.push(SyntaxFact::new(
        block.local_id(),
        None,
        SyntaxFactKind::Root,
        body,
        0,
        rootlight_adapter_sdk::SyntaxKindLabel::new("rust.file.root")
            .map_err(|_| provider_failure("embedded-rust-root"))?,
    ));
    for fact in group {
        cancellation.check()?;
        // A same-span function can geometrically own this noncontainer capture.
        // The private root authenticates the complete native source-file context.
        result.push(SyntaxFact::new(
            fact.local_id(),
            if fact.local_id() == signature.local_id() {
                Some(block.local_id())
            } else {
                fact.parent()
            },
            fact.kind(),
            fact.span(),
            fact.depth(),
            fact.syntax_kind().clone(),
        ));
    }
    Ok(Some(result))
}

fn clean_owner(
    block: &SyntaxFact,
    body: SourceSpan,
    facts: &[SyntaxFact],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut clean = false;
    for fact in facts {
        cancellation.check()?;
        let label = fact.syntax_kind().as_str();
        if !label.starts_with("markdown.")
            || !label.contains("code_")
            || !label.ends_with(".signature")
            || fact.span().file() != body.file()
            || fact.span().start_byte() >= body.end_byte()
            || fact.span().end_byte() <= body.start_byte()
        {
            continue;
        }
        if clean
            || label != "markdown.code_parsed.signature"
            || fact.kind() != SyntaxFactKind::Signature
            || fact.span() != block.span()
        {
            return Ok(false);
        }
        clean = true;
    }
    Ok(clean)
}

fn owned_ancestry(
    fact: &SyntaxFact,
    block: u64,
    by_id: &HashMap<u64, &SyntaxFact>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut parent = fact.parent();
    for _ in 0..=by_id.len() {
        cancellation.check()?;
        let Some(id) = parent else {
            return Ok(false);
        };
        if id == block {
            return Ok(true);
        }
        let Some(ancestor) = by_id.get(&id) else {
            return Ok(false);
        };
        parent = ancestor.parent();
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        label: &str,
        start: u64,
        end: u64,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            SourceSpan::new(FileId::from_bytes([1; 20]), start, end).unwrap(),
            usize::from(parent.is_some()),
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn fixture() -> Vec<SyntaxFact> {
        vec![
            fact(
                1,
                None,
                SyntaxFactKind::Module,
                "markdown.code_block.module",
                10,
                60,
            ),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Signature,
                "markdown.code_parsed.signature",
                10,
                60,
            ),
            fact(
                3,
                Some(4),
                SyntaxFactKind::Signature,
                "rust.proof_context.signature",
                20,
                50,
            ),
            fact(
                4,
                Some(1),
                SyntaxFactKind::Declaration,
                "rust.function.declaration",
                20,
                50,
            ),
            fact(
                5,
                Some(4),
                SyntaxFactKind::Occurrence,
                "rust.identifier.definition",
                23,
                29,
            ),
        ]
    }

    fn plan(facts: &[SyntaxFact]) -> Plan {
        Plan::embedded(
            facts,
            &[],
            &BTreeMap::from([(5, "target".to_owned())]),
            64,
            &Cancellation::new(),
        )
        .unwrap()
    }

    #[test]
    fn private_rust_root_preserves_admitted_facts_and_ignores_recovered_neighbors() {
        let mut facts = fixture();
        facts.push(fact(
            8,
            None,
            SyntaxFactKind::Signature,
            "markdown.code_error.signature",
            70,
            100,
        ));
        let original = facts.clone();
        let cancellation = Cancellation::new();
        let grouped = groups(&facts, &cancellation).unwrap();
        let partition = certified_partition(grouped[0].0, &grouped[0].1, &facts, &cancellation)
            .unwrap()
            .unwrap();
        assert_eq!(facts, original);
        assert_eq!(partition[0].local_id(), 1);
        assert_eq!(partition[0].kind(), SyntaxFactKind::Root);
        assert_eq!(partition[0].span(), facts[2].span());
        assert_eq!(partition[1].parent(), Some(1));
        assert_eq!(partition[1].local_id(), facts[2].local_id());
        assert_eq!(partition[2].parent(), facts[3].parent());
        assert!(plan(&facts).proven_for_fact(5).is_some());
        assert!(plan(&facts).for_fact(1).is_none());
    }

    #[test]
    fn rust_example_certificate_rejects_missing_duplicate_or_conflicting_context() {
        for variant in 0..7 {
            let mut facts = fixture();
            match variant {
                0 => {
                    facts.remove(2);
                }
                1 => facts.push(fact(
                    6,
                    Some(1),
                    SyntaxFactKind::Signature,
                    "rust.proof_context.signature",
                    20,
                    50,
                )),
                2 => {
                    facts[1] = fact(
                        2,
                        Some(1),
                        SyntaxFactKind::Signature,
                        "markdown.code_error.signature",
                        10,
                        60,
                    );
                }
                3 => facts.push(fact(
                    6,
                    Some(1),
                    SyntaxFactKind::Signature,
                    "markdown.code_limit.signature",
                    10,
                    60,
                )),
                4 => {
                    facts[4] = fact(
                        5,
                        Some(4),
                        SyntaxFactKind::Occurrence,
                        "rust.identifier.definition",
                        49,
                        51,
                    );
                }
                5 => {
                    facts[3] = fact(
                        4,
                        Some(5),
                        SyntaxFactKind::Declaration,
                        "rust.function.declaration",
                        20,
                        50,
                    );
                }
                6 => {
                    facts[3] = fact(
                        4,
                        Some(90),
                        SyntaxFactKind::Declaration,
                        "rust.function.declaration",
                        20,
                        50,
                    );
                }
                _ => unreachable!(),
            }
            assert!(
                plan(&facts).proven_for_fact(5).is_none(),
                "certificate variant {variant}"
            );
        }
    }

    #[test]
    fn rust_example_extraction_loss_and_cancellation_cannot_authorize_bindings() {
        let diagnostic = AdapterDiagnostic::new(
            DiagnosticCode::new("syntax-extraction-limit").unwrap(),
            DiagnosticSeverity::Warning,
            None,
            CoverageStatus::Unknown,
        );
        let facts = fixture();
        let names = BTreeMap::from([(5, "target".to_owned())]);
        let cancellation = Cancellation::new();
        let partial = Plan::embedded(&facts, &[diagnostic], &names, 64, &cancellation).unwrap();
        assert!(partial.for_fact(5).is_some());
        assert!(partial.proven_for_fact(5).is_none());
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(Plan::embedded(&facts, &[], &names, 64, &cancellation).is_err());
    }
}
