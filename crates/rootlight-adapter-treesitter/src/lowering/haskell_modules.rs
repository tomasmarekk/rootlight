//! Explicit Haskell module headers own their top-level source declarations.
//! Header evidence keeps its written extent; file-root frontiers certify module
//! membership without reparenting nested declarations or separate partitions.

use super::*;

#[derive(Default)]
pub(super) struct Plan {
    owners: HashMap<u64, Option<u64>>,
}

#[derive(Clone, Copy)]
struct Frontier {
    root: u64,
    span: SourceSpan,
}

impl Plan {
    pub(super) fn new(
        facts: &[&SyntaxFact],
        captures: &HashMap<u64, AssociatedCaptures<'_>>,
        source: &str,
        maximum_name_bytes: usize,
        complete: bool,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut frontiers = HashMap::<u64, Frontier>::new();
        let mut headers = HashMap::<u64, Option<u64>>::new();
        for fact in facts {
            cancellation.check()?;
            if fact.kind() == SyntaxFactKind::Root
                && fact.syntax_kind().as_str() == "haskell.file.root"
            {
                frontiers
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                frontiers.insert(
                    fact.local_id(),
                    Frontier {
                        root: fact.local_id(),
                        span: fact.span(),
                    },
                );
                continue;
            }
            let Some(frontier) = fact.parent().and_then(|id| frontiers.get(&id)).copied() else {
                continue;
            };
            if !span_contains(frontier.span, fact.span()) {
                continue;
            }
            if matches!(
                (fact.kind(), fact.syntax_kind().as_str()),
                (SyntaxFactKind::Module, "haskell.file.module")
                    | (SyntaxFactKind::Scope, "haskell.file.scope")
            ) && fact.span() == frontier.span
                || is_haskell_type_annotation(fact)
            {
                frontiers
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                frontiers.insert(fact.local_id(), frontier);
            }
            if fact.kind() != SyntaxFactKind::Declaration
                || fact.syntax_kind().as_str() != "haskell.module.declaration"
            {
                continue;
            }
            let definition = captures
                .get(&fact.local_id())
                .and_then(|capture| select_unique_capture(&capture.definitions));
            let valid_name = definition
                .filter(|definition| span_contains(fact.span(), definition.span()))
                .and_then(|definition| {
                    let begin = usize::try_from(definition.span().start_byte()).ok()?;
                    let end = usize::try_from(definition.span().end_byte()).ok()?;
                    let text = source.get(begin..end)?;
                    rootlight_adapter_sdk::structural_captured_name_for_fact(
                        "haskell",
                        definition,
                        text,
                        maximum_name_bytes,
                    )
                })
                .is_some();
            headers
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            headers
                .entry(frontier.root)
                .and_modify(|owner| *owner = None)
                .or_insert((complete && valid_name).then_some(fact.local_id()));
        }
        let mut owners = HashMap::new();
        for fact in facts {
            cancellation.check()?;
            if !matches!(
                fact.kind(),
                SyntaxFactKind::Declaration | SyntaxFactKind::Signature | SyntaxFactKind::Import
            ) || fact.syntax_kind().as_str() == "haskell.module.declaration"
            {
                continue;
            }
            let Some(frontier) = fact.parent().and_then(|id| frontiers.get(&id)) else {
                continue;
            };
            if !fact.syntax_kind().as_str().starts_with("haskell.")
                || !span_contains(frontier.span, fact.span())
            {
                continue;
            }
            let Some(owner) = headers.get(&frontier.root).copied() else {
                continue;
            };
            owners
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            owners.insert(fact.local_id(), owner);
        }
        Ok(Self { owners })
    }

    pub(super) fn owner(&self, fact: &SyntaxFact) -> Option<Option<u64>> {
        self.owners.get(&fact.local_id()).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        span: (u64, u64),
        label: &str,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            SourceSpan::new(FileId::from_bytes([1; 20]), span.0, span.1).unwrap(),
            usize::try_from(id).unwrap(),
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn file_frontiers_keep_same_name_partitions_and_nested_scopes_separate() {
        let text = "module Same where\nx = 1\nmodule Same where\nx = 2\n";
        let facts = vec![
            fact(1, None, SyntaxFactKind::Root, (0, 24), "haskell.file.root"),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Module,
                (0, 24),
                "haskell.file.module",
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Scope,
                (0, 24),
                "haskell.file.scope",
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Declaration,
                (0, 17),
                "haskell.module.declaration",
            ),
            fact(
                5,
                Some(4),
                SyntaxFactKind::Occurrence,
                (7, 11),
                "haskell.binding_name.definition",
            ),
            fact(
                6,
                Some(3),
                SyntaxFactKind::Declaration,
                (18, 23),
                "haskell.function.declaration",
            ),
            fact(
                7,
                Some(6),
                SyntaxFactKind::Scope,
                (18, 23),
                "haskell.function_clause.scope",
            ),
            fact(
                8,
                Some(7),
                SyntaxFactKind::Declaration,
                (18, 19),
                "haskell.variable.declaration",
            ),
            fact(9, None, SyntaxFactKind::Root, (24, 48), "haskell.file.root"),
            fact(
                10,
                Some(9),
                SyntaxFactKind::Module,
                (24, 48),
                "haskell.file.module",
            ),
            fact(
                11,
                Some(10),
                SyntaxFactKind::Scope,
                (24, 48),
                "haskell.file.scope",
            ),
            fact(
                12,
                Some(11),
                SyntaxFactKind::Declaration,
                (24, 41),
                "haskell.module.declaration",
            ),
            fact(
                13,
                Some(12),
                SyntaxFactKind::Occurrence,
                (31, 35),
                "haskell.binding_name.definition",
            ),
            fact(
                14,
                Some(11),
                SyntaxFactKind::Declaration,
                (42, 47),
                "haskell.function.declaration",
            ),
        ];
        let mut captures = HashMap::new();
        captures.insert(
            4,
            AssociatedCaptures {
                definitions: vec![&facts[4]],
                ..Default::default()
            },
        );
        captures.insert(
            12,
            AssociatedCaptures {
                definitions: vec![&facts[12]],
                ..Default::default()
            },
        );
        let ordered: Vec<_> = facts.iter().collect();
        let plan = Plan::new(&ordered, &captures, text, 1024, true, &Cancellation::new()).unwrap();
        assert_eq!(plan.owner(&facts[5]), Some(Some(4)));
        assert_eq!(plan.owner(&facts[13]), Some(Some(12)));
        assert_eq!(plan.owner(&facts[7]), None);
        assert_eq!(plan.owner(&facts[3]), None);
        assert_eq!(plan.owner(&facts[11]), None);
    }

    #[test]
    fn missing_ambiguous_and_recovered_header_evidence_never_selects_an_explicit_owner() {
        let text = "module Same where\nx = 1\n";
        let facts = [
            fact(1, None, SyntaxFactKind::Root, (0, 24), "haskell.file.root"),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Scope,
                (0, 24),
                "haskell.file.scope",
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Declaration,
                (0, 17),
                "haskell.module.declaration",
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Occurrence,
                (7, 11),
                "haskell.binding_name.definition",
            ),
            fact(
                5,
                Some(2),
                SyntaxFactKind::Declaration,
                (18, 23),
                "haskell.function.declaration",
            ),
        ];
        let mut captures = HashMap::new();
        let ordered: Vec<_> = facts.iter().collect();
        let missing =
            Plan::new(&ordered, &captures, text, 1024, true, &Cancellation::new()).unwrap();
        assert_eq!(missing.owner(&facts[4]), Some(None));
        captures.insert(
            3,
            AssociatedCaptures {
                definitions: vec![&facts[3]],
                ..Default::default()
            },
        );
        let recovered =
            Plan::new(&ordered, &captures, text, 1024, false, &Cancellation::new()).unwrap();
        assert_eq!(recovered.owner(&facts[4]), Some(None));
        let duplicate = fact(
            6,
            Some(2),
            SyntaxFactKind::Declaration,
            (0, 17),
            "haskell.module.declaration",
        );
        let mut ambiguous = ordered.clone();
        ambiguous.push(&duplicate);
        let ambiguous = Plan::new(
            &ambiguous,
            &captures,
            text,
            1024,
            true,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(ambiguous.owner(&facts[4]), Some(None));
        let absent: Vec<_> = ordered
            .into_iter()
            .filter(|fact| fact.local_id() != 3 && fact.local_id() != 4)
            .collect();
        let absent = Plan::new(&absent, &captures, text, 1024, true, &Cancellation::new()).unwrap();
        assert_eq!(absent.owner(&facts[4]), None);
    }

    #[test]
    fn module_frontier_planning_observes_cancellation_before_allocating() {
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        let root = fact(1, None, SyntaxFactKind::Root, (0, 1), "haskell.file.root");
        assert!(Plan::new(&[&root], &HashMap::new(), "x", 1, true, &cancellation).is_err());
    }
}
