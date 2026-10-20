//! Resolve only clean native Sass reads against earlier lexical declarations.
//! Hyphen/underscore equivalence affects lookup; entity spelling and immediate
//! containers retain their authored source and exact generation/file identity.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRecord, OccurrenceRole};

use super::MaterializedEntity;

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Variable, "sass.variable.declaration")
            | (EntityKind::StyleRule, "sass.style_rule.declaration")
            | (EntityKind::Property, "sass.property.declaration")
    )
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "sass.name.signature" | "sass.selector.signature"
        )
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "sass.plain_read.reference" | "sass.unresolved_read.reference",
        ) => Some(OccurrenceRole::Read),
        (SyntaxFactKind::Occurrence, "sass.assignment.write") => Some(OccurrenceRole::Write),
        _ => None,
    }
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let rows: &[(FactDomain, &'static str)] = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "sass.source_file") => &[
            (
                FactDomain::Entities,
                "sass-mixin-function-keyframe-and-general-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "sass-general-expression-parameters-and-string-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "sass-module-flow-mixin-evaluation-and-runtime-bindings-unavailable",
            ),
        ],
        (SyntaxFactKind::Signature, "sass.unsupported.expression") => &[
            (
                FactDomain::Entities,
                "sass-source-construct-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "sass-source-construct-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "sass-source-construct-semantics-unavailable",
            ),
        ],
        (SyntaxFactKind::ErrorRecovery, "sass.initial_indentation_unavailable") => &[
            (
                FactDomain::Entities,
                "sass-initial-indentation-analysis-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "sass-initial-indentation-analysis-unavailable",
            ),
            (
                FactDomain::Relations,
                "sass-initial-indentation-analysis-unavailable",
            ),
        ],
        (SyntaxFactKind::ErrorRecovery, "sass.rejected_indentation") => &[
            (FactDomain::Entities, "sass-invalid-indentation"),
            (FactDomain::Occurrences, "sass-invalid-indentation"),
            (FactDomain::Relations, "sass-invalid-indentation"),
        ],
        (SyntaxFactKind::ErrorRecovery, "sass.rejected_syntax") => &[
            (FactDomain::Entities, "sass-native-parse-failed"),
            (FactDomain::Occurrences, "sass-native-parse-failed"),
            (FactDomain::Relations, "sass-native-parse-failed"),
        ],
        (SyntaxFactKind::ErrorRecovery, "sass.parse_recovery") => &[
            (
                FactDomain::Occurrences,
                "sass-recovered-source-occurrences-unavailable",
            ),
            (
                FactDomain::Entities,
                "sass-recovered-source-declarations-unavailable",
            ),
            (
                FactDomain::Relations,
                "sass-recovered-source-bindings-unavailable",
            ),
        ],
        (SyntaxFactKind::Occurrence, "sass.assignment.write") => &[(
            FactDomain::Relations,
            "sass-written-assignment-target-unavailable",
        )],
        _ => &[],
    };
    rows.iter().copied()
}

fn folded(name: &str) -> Result<String, AdapterError> {
    let mut result = String::new();
    result
        .try_reserve_exact(name.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for character in name.chars() {
        result.push(if character == '_' { '-' } else { character });
    }
    Ok(result)
}

pub(super) struct Bindings<'a> {
    facts: HashMap<u64, &'a SyntaxFact>,
    declarations: HashMap<(u64, String), Option<(u64, u64)>>,
    entities: &'a HashMap<u64, MaterializedEntity>,
    clean: bool,
}

impl<'a> Bindings<'a> {
    pub(super) fn new(
        entities: &'a HashMap<u64, MaterializedEntity>,
        facts: &'a [SyntaxFact],
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            facts: HashMap::new(),
            declarations: HashMap::new(),
            entities,
            clean: false,
        };
        result
            .facts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        result
            .declarations
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut roots = facts.iter().filter(|fact| {
            fact.kind() == SyntaxFactKind::Root && fact.syntax_kind().as_str() == "sass.source_file"
        });
        if let Some(root) = roots.next().filter(|_| roots.next().is_none()) {
            result.clean = facts
                .iter()
                .filter(|fact| {
                    fact.kind() == SyntaxFactKind::Signature
                        && fact.syntax_kind().as_str() == "sass.clean_parse.signature"
                        && fact.parent() == Some(root.local_id())
                        && fact.span() == root.span()
                })
                .count()
                == 1;
        }
        for fact in facts {
            cancel.check()?;
            if result.facts.insert(fact.local_id(), fact).is_some() {
                return Err(SinkError::SourceMismatch.into());
            }
            if fact.kind() != SyntaxFactKind::Declaration
                || fact.syntax_kind().as_str() != "sass.variable.declaration"
            {
                continue;
            }
            let Some(entity) = entities.get(&fact.local_id()).filter(|entity| {
                entity.record.kind == EntityKind::Variable && entity.record.language == "sass"
            }) else {
                continue;
            };
            let Some(parent) = fact.parent() else {
                continue;
            };
            result
                .declarations
                .entry((parent, folded(&entity.record.canonical_name)?))
                .and_modify(|slot| *slot = None)
                .or_insert(Some((fact.local_id(), fact.span().end_byte())));
        }
        Ok(result)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        name: &str,
        site: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        if !self.clean
            || fact.syntax_kind().as_str() != "sass.plain_read.reference"
            || site.role != OccurrenceRole::Read
            || fact.span() != site.source.span()
        {
            return Ok(None);
        }
        let name = folded(name)?;
        let mut parent = fact.parent();
        let mut remaining = self.facts.len();
        while let Some(id) = parent {
            cancel.check()?;
            if remaining == 0 {
                return Ok(None);
            }
            remaining -= 1;
            let Some(scope) = self.facts.get(&id) else {
                return Ok(None);
            };
            if scope.span().file() != fact.span().file()
                || scope.span().start_byte() > fact.span().start_byte()
                || scope.span().end_byte() < fact.span().end_byte()
            {
                return Ok(None);
            }
            if let Some(binding) = self.declarations.get(&(id, name.clone())) {
                let Some((target, initialized)) = binding else {
                    return Ok(None);
                };
                if *initialized <= fact.span().start_byte() {
                    let Some(entity) = self.entities.get(target) else {
                        return Ok(None);
                    };
                    let Some(source) = &entity.record.evidence.source else {
                        return Ok(None);
                    };
                    return Ok((source.repository() == site.source.repository()
                        && source.generation() == site.source.generation()
                        && source.content_hash() == site.source.content_hash()
                        && source.span().file() == site.source.span().file())
                    .then_some(entity.record.id));
                }
            }
            parent = scope.parent();
        }
        Ok(None)
    }
}
