//! Canonical native names used only to allocate bounded relationship evidence.
//! Sharing this view across priority and retention passes keeps escape spelling
//! from changing admission; it does not establish a semantic target.

use std::borrow::Cow;

use super::*;

pub(in crate::project_semantics) struct SchedulingNames<'source> {
    pub(in crate::project_semantics) definitions: BTreeMap<u64, Cow<'source, str>>,
    pub(in crate::project_semantics) calls: BTreeMap<u64, ScheduledCall<'source>>,
}

pub(in crate::project_semantics) struct ScheduledCall<'source> {
    pub(in crate::project_semantics) name: Cow<'source, str>,
    pub(in crate::project_semantics) receiver: Option<String>,
    pub(in crate::project_semantics) path_fact: Option<u64>,
}

impl<'source> SchedulingNames<'source> {
    /// Shares bounded native field names across syntax admission passes.
    /// Invalid names or paths remain absent; cancellation returns no partial view.
    pub(in crate::project_semantics) fn new(
        facts: &[SyntaxFact],
        source: &'source [u8],
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let by_id: BTreeMap<_, _> = facts.iter().map(|fact| (fact.local_id(), fact)).collect();
        let terminals = terminal_call_names(facts);
        let member_paths = references::member_paths(facts, cancellation)?;
        let mut path_facts = BTreeMap::new();
        for fact in facts.iter().filter(|fact| references::is_member_path(fact)) {
            cancellation.check()?;
            path_facts.entry(fact.span()).or_insert(fact.local_id());
        }
        let mut definitions = BTreeMap::new();
        let mut calls = BTreeMap::new();
        for fact in facts {
            cancellation.check()?;
            if is_definition_fact(fact)
                && let Some(name) = source_text(source, fact.span())
                    .and_then(|text| canonical_ecmascript_identifier(text, maximum_name_bytes))
            {
                definitions.insert(fact.local_id(), name);
            }
            if !is_call_fact(fact) {
                continue;
            }
            // ECMAScript call captures are the native callee field; producers
            // with a separate terminal capture can expose that narrower field.
            let terminal = terminals
                .get(&fact.local_id())
                .and_then(|id| by_id.get(id))
                .copied()
                .unwrap_or(fact);
            let (name, receiver, path_fact) = if let Some(span) =
                member_paths.get(&terminal.span().end_byte())
            {
                let Some(span) = span.filter(|span| contains_span(*span, terminal.span())) else {
                    continue;
                };
                let Some(text) = source_text(source, span) else {
                    continue;
                };
                let Some((receiver, member)) =
                    references::parse_member_path(text, maximum_name_bytes, cancellation)?
                else {
                    continue;
                };
                (
                    Cow::Owned(member),
                    Some(receiver),
                    path_facts.get(&span).copied(),
                )
            } else {
                let Some(name) = source_text(source, terminal.span())
                    .and_then(|text| canonical_ecmascript_identifier(text, maximum_name_bytes))
                else {
                    continue;
                };
                (name, None, None)
            };
            calls.insert(
                fact.local_id(),
                ScheduledCall {
                    name,
                    receiver,
                    path_fact,
                },
            );
        }
        Ok(Self { definitions, calls })
    }
}

#[cfg(test)]
mod tests {
    use rootlight_adapter_sdk::SyntaxKindLabel;

    use super::*;

    fn fact(
        source: &str,
        spelling: &str,
        occurrence: usize,
        id: u64,
        label: &str,
        kind: SyntaxFactKind,
    ) -> SyntaxFact {
        let start = source.match_indices(spelling).nth(occurrence).unwrap().0;
        SyntaxFact::new(
            id,
            None,
            kind,
            SourceSpan::new(
                FileId::from_bytes([1; 20]),
                u64::try_from(start).unwrap(),
                u64::try_from(start + spelling.len()).unwrap(),
            )
            .unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn equivalent_callee_spellings_share_local_demand_and_representatives() {
        let source = r"function \u0052un() {} Run(); \u0052un(); Other();";
        let facts = [
            fact(
                source,
                r"\u0052un",
                0,
                1,
                "typescript.identifier.definition",
                SyntaxFactKind::Occurrence,
            ),
            fact(
                source,
                "Run",
                0,
                2,
                "typescript.identifier.call",
                SyntaxFactKind::Occurrence,
            ),
            fact(
                source,
                r"\u0052un",
                1,
                3,
                "typescript.identifier.call",
                SyntaxFactKind::Occurrence,
            ),
            fact(
                source,
                "Other",
                0,
                4,
                "typescript.identifier.call",
                SyntaxFactKind::Occurrence,
            ),
        ];
        let names =
            SchedulingNames::new(&facts, source.as_bytes(), 1024, &Cancellation::new()).unwrap();
        let by_id = facts.iter().map(|fact| (fact.local_id(), fact)).collect();
        let terminals = terminal_call_names(&facts);
        let declared =
            declared_call_ids(&facts, source.as_bytes(), &by_id, &terminals, Some(&names));
        assert_eq!(declared, BTreeSet::from([2, 3]));
        let preferred = preferred_local_call_syntax_fact_group(
            SemanticProjectLanguage::TypeScript,
            &facts,
            source.as_bytes(),
            &by_id,
            &terminals,
            &declared,
            Some(&names),
        )
        .unwrap();
        assert_eq!((preferred.call_id, preferred.demand), (2, 2));
        let representatives = distinct_call_syntax_representatives(
            &facts,
            source.as_bytes(),
            &by_id,
            &terminals,
            &BTreeSet::from([2]),
            Some(&names),
        );
        assert_eq!(
            representatives
                .iter()
                .map(|fact| fact.local_id())
                .collect::<Vec<_>>(),
            [4]
        );
    }

    #[test]
    fn namespace_members_keep_distinct_groups_and_atomic_path_evidence() {
        let source =
            r"import * as Space from './dep'; Space.Run(); \u0053pace.Other(); Space.\u0052un();";
        let facts = [
            fact(
                source,
                "import * as Space from './dep';",
                0,
                1,
                "typescript.native_import.import",
                SyntaxFactKind::Import,
            ),
            fact(
                source,
                "Run",
                0,
                2,
                "typescript.identifier.call",
                SyntaxFactKind::Occurrence,
            ),
            fact(
                source,
                "Space.Run",
                0,
                3,
                "typescript.member_path.reference",
                SyntaxFactKind::Occurrence,
            ),
            fact(
                source,
                "Other",
                0,
                4,
                "typescript.identifier.call",
                SyntaxFactKind::Occurrence,
            ),
            fact(
                source,
                r"\u0053pace.Other",
                0,
                5,
                "typescript.member_path.reference",
                SyntaxFactKind::Occurrence,
            ),
            fact(
                source,
                r"\u0052un",
                0,
                6,
                "typescript.identifier.call",
                SyntaxFactKind::Occurrence,
            ),
            fact(
                source,
                r"Space.\u0052un",
                0,
                7,
                "typescript.member_path.reference",
                SyntaxFactKind::Occurrence,
            ),
        ];
        let names =
            SchedulingNames::new(&facts, source.as_bytes(), 1024, &Cancellation::new()).unwrap();
        let by_id = facts.iter().map(|fact| (fact.local_id(), fact)).collect();
        let terminals = terminal_call_names(&facts);
        let imports = BTreeMap::from([(
            1,
            ParsedImport {
                module: "./dep".to_owned(),
                bindings: vec![ImportBinding::Namespace {
                    local: "Space".to_owned(),
                }],
                type_only: BTreeSet::new(),
            },
        )]);
        let groups = imported_call_syntax_fact_groups(
            SemanticProjectLanguage::TypeScript,
            source.as_bytes(),
            &by_id,
            &terminals,
            &BTreeSet::new(),
            &imports,
            Some(&names),
        );
        assert_eq!(groups.len(), 2);
        assert_eq!((groups[0].demand, groups[1].demand), (2, 1));
        assert_eq!(groups[0].call_ids, [2]);
        assert_eq!(groups[1].call_ids, [4]);
        assert_eq!(
            imported_call_syntax_fact_group_ids(&groups[0], &by_id, &terminals, Some(&names)),
            BTreeSet::from([1, 2, 3])
        );
        let mut selected = BTreeSet::new();
        let mut remaining = 1;
        select_call_syntax_fact_group(
            &facts[1],
            &terminals,
            &by_id,
            &mut selected,
            &mut remaining,
            Some(&names),
        );
        assert!(selected.is_empty());
        assert_eq!(remaining, 1);
        remaining = 2;
        select_call_syntax_fact_group(
            &facts[1],
            &terminals,
            &by_id,
            &mut selected,
            &mut remaining,
            Some(&names),
        );
        assert_eq!(selected, BTreeSet::from([2, 3]));
        assert_eq!(remaining, 0);
    }

    #[test]
    fn unavailable_member_paths_and_name_limits_never_become_local_calls() {
        for path in ["Space().Run", r"Space.\u002e", "Space.Run"] {
            let name = path.rsplit('.').next().unwrap();
            let facts = [
                fact(
                    path,
                    name,
                    0,
                    1,
                    "typescript.identifier.call",
                    SyntaxFactKind::Occurrence,
                ),
                fact(
                    path,
                    path,
                    0,
                    2,
                    "typescript.member_path.reference",
                    SyntaxFactKind::Occurrence,
                ),
            ];
            let maximum = if path == "Space.Run" {
                path.len() - 1
            } else {
                1024
            };
            let names =
                SchedulingNames::new(&facts, path.as_bytes(), maximum, &Cancellation::new())
                    .unwrap();
            assert!(names.calls.is_empty());
        }
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            SchedulingNames::new(&[], b"", 1024, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
