//! Whole-call evidence built on exact project-owned member proofs.
//! Native parameter inventories must agree with the complete authored signature;
//! application references never add a second call edge for the same invocation.

use super::{ClassicIndex, Proof};
use crate::{ResolutionError, ResolutionRule, engine::ResolutionWorkBudget};
use rootlight_cancel::Cancellation;
use rootlight_ids::{FileId, SymbolId};
use rootlight_ir::{
    ContainerRef, EntityKind, EntityRecord, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    SourceRef,
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

struct Contract<'a> {
    parameters: Arc<Vec<&'a EntityRecord>>,
    optional: Vec<bool>,
    names: BTreeMap<String, usize>,
    variadic: bool,
    kind: rootlight_vba::ProcedureKind,
}

struct NamedArgument {
    span: std::ops::Range<usize>,
    parameter: usize,
}

pub(super) struct BoundCall<'a, 'o> {
    pub(super) callee: &'o OccurrenceRecord,
    pub(super) explicit: bool,
    pub(super) proof: Proof<'a>,
    pub(super) sources: &'o [SourceRef],
}

pub(super) struct CallIndex<'a, 'o> {
    parameters: BTreeMap<SymbolId, Vec<&'a EntityRecord>>,
    definitions: BTreeMap<SymbolId, Option<&'o SourceRef>>,
    callees: BTreeMap<(FileId, u64, u64), Option<&'o OccurrenceRecord>>,
    labels: BTreeMap<(FileId, u64, u64), Option<&'o OccurrenceRecord>>,
    applications: BTreeMap<(FileId, u64, u64), Option<&'o OccurrenceRecord>>,
    contracts: BTreeMap<SymbolId, Option<Contract<'a>>>,
}

impl<'a, 'o> CallIndex<'a, 'o> {
    pub(super) fn build(
        entities: &BTreeMap<SymbolId, &'a EntityRecord>,
        occurrences: impl IntoIterator<Item = &'o OccurrenceRecord>,
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<Self, ResolutionError> {
        let mut parameters: BTreeMap<SymbolId, Vec<&EntityRecord>> = BTreeMap::new();
        for entity in entities.values() {
            cancellation.check()?;
            work.consume()?;
            if entity.kind == EntityKind::Parameter
                && let Some(ContainerRef::Entity(owner)) = entity.container
            {
                parameters.entry(owner).or_default().push(entity);
            }
        }
        let mut definitions = BTreeMap::new();
        let mut callees = BTreeMap::new();
        let mut labels = BTreeMap::new();
        let mut applications = BTreeMap::new();
        for site in occurrences {
            cancellation.check()?;
            work.consume()?;
            if site.role == OccurrenceRole::Definition
                && let OccurrenceTarget::Resolved { symbol } = site.target
            {
                definitions
                    .entry(symbol)
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(&site.source));
            }
            let key = (site.file, site.source.span().start_byte());
            if site.syntax_kind == "vba.nonvalue_identifier.reference"
                && site.role == OccurrenceRole::Reference
                && matches!(site.target, OccurrenceTarget::Unresolved { .. })
            {
                labels
                    .entry((key.0, key.1, site.source.span().end_byte()))
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(site));
            }
            if site.syntax_kind == "vba.application.reference" {
                let end = site.source.span().end_byte();
                applications
                    .entry((key.0, key.1, end))
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(site));
            }
            if super::is_invoked_member(&site.syntax_kind)
                || matches!(
                    site.syntax_kind.as_str(),
                    "vba.invoked_get_identifier.reference"
                        | "vba.invoked_let_identifier.reference"
                        | "vba.invoked_set_identifier.reference"
                        | "vba.invoked_identifier.reference"
                        | "vba.member.reference"
                )
            {
                callees
                    .entry((key.0, key.1, site.source.span().end_byte()))
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(site));
            }
        }
        Ok(Self {
            parameters,
            definitions,
            callees,
            labels,
            applications,
            contracts: BTreeMap::new(),
        })
    }

    pub(super) fn callee(
        &self,
        site: &OccurrenceRecord,
        sources: &BTreeMap<FileId, &[u8]>,
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<Option<(&'o OccurrenceRecord, bool)>, ResolutionError> {
        cancellation.check()?;
        work.consume()?;
        if site.syntax_kind != "vba.application.reference"
            || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
        {
            return Ok(None);
        }
        let Some(bytes) = source_bytes(&site.source, sources) else {
            return Ok(None);
        };
        work.consume_many(bytes.len().div_ceil(64).max(1))?;
        let mut check = || {
            cancellation.check()?;
            work.consume()
        };
        let explicit = bytes
            .get(..4)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"Call"))
            && bytes.get(4).is_some_and(u8::is_ascii_whitespace);
        let offset = if explicit {
            4 + rootlight_vba::logical_whitespace_len(
                bytes.get(4..).unwrap_or_default(),
                &mut check,
            )?
        } else {
            0
        };
        let Some(start) = u64::try_from(offset)
            .ok()
            .and_then(|n| site.source.span().start_byte().checked_add(n))
        else {
            return Ok(None);
        };
        let key = (site.file, start);
        let Some(callee) = self
            .callees
            .range((key.0, key.1, 0)..=(key.0, key.1, site.source.span().end_byte()))
            .next_back()
            .and_then(|(_, site)| *site)
        else {
            return Ok(None);
        };
        // An intervening application dispatches on a returned object, not on
        // this callee. Earlier receiver calls may precede a later named callee.
        if self
            .applications
            .range(
                (key.0, key.1, callee.source.span().end_byte())
                    ..(key.0, key.1, site.source.span().end_byte()),
            )
            .next()
            .is_some()
        {
            return Ok(None);
        }
        Ok(Some((callee, explicit)))
    }

    pub(super) fn returned_receiver(
        &self,
        site: &OccurrenceRecord,
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<Option<&'o OccurrenceRecord>, ResolutionError> {
        cancellation.check()?;
        work.consume()?;
        if site.syntax_kind != "vba.application.reference" {
            return Ok(None);
        }
        let span = site.source.span();
        let start = (site.file, span.start_byte(), 0);
        let end = (site.file, span.start_byte(), span.end_byte());
        let Some(receiver) = self
            .applications
            .range(start..end)
            .next_back()
            .and_then(|(_, site)| *site)
        else {
            return Ok(None);
        };
        // A later named callee consumes its own arguments. Only consecutive
        // applications can dispatch through this earlier application's result.
        if self
            .callees
            .range(start..end)
            .next_back()
            .is_some_and(|(key, _)| key.2 > receiver.source.span().end_byte())
        {
            return Ok(None);
        }
        Ok(Some(receiver))
    }

    pub(super) fn resolve(
        &mut self,
        result: &mut ClassicIndex<'a>,
        site: &OccurrenceRecord,
        selected: Option<(&'o OccurrenceRecord, bool)>,
        sources: &BTreeMap<FileId, &[u8]>,
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<Option<(&'o OccurrenceRecord, bool)>, ResolutionError> {
        cancellation.check()?;
        // Dependency ordering already selected the callee against this exact
        // source inventory. Only the semantic contract depends on prior proofs.
        let Some((callee, explicit)) = selected else {
            return Ok(None);
        };
        // Setter index parentheses exclude the assigned value. They cannot
        // establish the complete assignment's argument contract on their own.
        if !result.is_call(callee)
            || matches!(
                super::member_property_access(&callee.syntax_kind),
                Some(super::PropertyAccessorKind::Let | super::PropertyAccessorKind::Set)
            )
        {
            return Ok(Some((callee, explicit)));
        }
        let Some(proof) = result.proof(callee) else {
            return Ok(Some((callee, explicit)));
        };
        let proof = Proof {
            target: proof.target,
            context: Arc::clone(&proof.context),
            receiver: proof.receiver.clone(),
            rule: ResolutionRule::ClassicProjectMember,
            property_call: false,
        };
        self.resolve_bound(
            result,
            site,
            BoundCall {
                callee,
                explicit,
                proof,
                sources: &[],
            },
            sources,
            cancellation,
            work,
        )?;
        Ok(Some((callee, explicit)))
    }

    pub(super) fn resolve_bound(
        &mut self,
        result: &mut ClassicIndex<'a>,
        site: &OccurrenceRecord,
        call: BoundCall<'a, '_>,
        sources: &BTreeMap<FileId, &[u8]>,
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<(), ResolutionError> {
        let BoundCall {
            callee,
            explicit,
            proof: application_proof,
            sources: additional_sources,
        } = call;
        let Some(bytes) = source_bytes(&site.source, sources) else {
            return Ok(());
        };
        let mut check = || {
            cancellation.check()?;
            work.consume()
        };
        let Some(relative_end) = callee
            .source
            .span()
            .end_byte()
            .checked_sub(site.source.span().start_byte())
            .and_then(|end| usize::try_from(end).ok())
        else {
            return Ok(());
        };
        let Some(tail) = bytes.get(relative_end..) else {
            return Ok(());
        };
        self.ensure_contract(application_proof.target, sources, &mut check)?;
        let Some(contract) = self
            .contracts
            .get(&application_proof.target.id)
            .and_then(Option::as_ref)
        else {
            return Ok(());
        };
        let mut additional_sources = Cow::Borrowed(additional_sources);
        if contract.kind == rootlight_vba::ProcedureKind::ExternalSub
            && callee.syntax_kind == "vba.invoked_get_member.reference"
        {
            return Ok(());
        }
        let (arguments, optional) = if matches!(
            contract.kind,
            rootlight_vba::ProcedureKind::PropertyLet | rootlight_vba::ProcedureKind::PropertySet
        ) {
            let Some((false, indices)) = contract.optional.split_last() else {
                return Ok(());
            };
            if let Some(end) =
                rootlight_vba::indexed_assignment_arguments_end(tail, tail.len(), &mut check)?
            {
                additional_sources.to_mut().push(site.source.clone());
                (tail.get(..end).unwrap_or_default(), indices)
            } else {
                let Some(source) = assignment_source(site, sources, &mut check)? else {
                    return Ok(());
                };
                additional_sources.to_mut().push(source);
                (tail, indices)
            }
        } else {
            (tail, contract.optional.as_slice())
        };
        let Some(named_arguments) = arguments_match(
            arguments,
            explicit,
            optional,
            &contract.names,
            contract.variadic,
            contract.kind,
            &mut check,
        )?
        else {
            return Ok(());
        };
        let parameters = if let Some(inherited) = result.application_parameters.get(&callee.id) {
            let mut combined = BTreeMap::new();
            for parameter in inherited.iter().chain(contract.parameters.iter()) {
                check()?;
                combined.insert(parameter.id, *parameter);
            }
            Arc::new(combined.into_values().collect())
        } else {
            Arc::clone(&contract.parameters)
        };
        let label_sources = if named_arguments.is_empty() {
            None
        } else {
            let mut evidence = BTreeSet::from([callee.source.clone(), site.source.clone()]);
            evidence.extend(additional_sources.iter().cloned());
            evidence.extend(application_proof.target.evidence.source.iter().cloned());
            if let Some((_, source)) = &application_proof.receiver {
                evidence.insert(source.clone());
            }
            if let Some(inherited) = result.receiver_sources.get(&callee.id) {
                for source in inherited.iter() {
                    check()?;
                    evidence.insert(source.clone());
                }
            }
            Some(Arc::new(evidence.into_iter().collect()))
        };
        for named in named_arguments {
            check()?;
            let Some(start) = u64::try_from(named.span.start)
                .ok()
                .and_then(|offset| callee.source.span().end_byte().checked_add(offset))
            else {
                continue;
            };
            let Some(end) = u64::try_from(named.span.end)
                .ok()
                .and_then(|offset| callee.source.span().end_byte().checked_add(offset))
            else {
                continue;
            };
            let Some(label) = self.labels.get(&(site.file, start, end)).copied().flatten() else {
                continue;
            };
            let Some(parameter) = contract.parameters.get(named.parameter).copied() else {
                continue;
            };
            // Only top-level authored labels from this complete argument list
            // can inherit its callee. Nested calls own their labels independently.
            if let Some(sources) = &label_sources {
                result
                    .receiver_sources
                    .insert(label.id, Arc::clone(sources));
            }
            result
                .application_parameters
                .insert(label.id, Arc::clone(&parameters));
            result.proofs.insert(
                label.id,
                Proof {
                    target: parameter,
                    context: Arc::clone(&application_proof.context),
                    receiver: Some((application_proof.target, site.source.clone())),
                    rule: ResolutionRule::ClassicProjectArgument,
                    property_call: false,
                },
            );
        }
        result.application_parameters.insert(site.id, parameters);
        if additional_sources.is_empty() {
            if let Some(sources) = result.receiver_sources.get(&callee.id).cloned() {
                result.receiver_sources.insert(site.id, sources);
            }
        } else {
            let mut evidence: BTreeSet<_> = additional_sources.iter().cloned().collect();
            if application_proof.rule == ResolutionRule::ClassicProjectDefaultMember {
                evidence.insert(callee.source.clone());
            }
            if let Some(sources) = result.receiver_sources.get(&callee.id) {
                evidence.extend(sources.iter().cloned());
            }
            result
                .receiver_sources
                .insert(site.id, Arc::new(evidence.into_iter().collect()));
        }
        result.proofs.insert(site.id, application_proof);
        Ok(())
    }

    fn ensure_contract(
        &mut self,
        target: &'a EntityRecord,
        sources: &BTreeMap<FileId, &[u8]>,
        check: &mut impl FnMut() -> Result<(), ResolutionError>,
    ) -> Result<(), ResolutionError> {
        if let std::collections::btree_map::Entry::Vacant(entry) = self.contracts.entry(target.id) {
            let owned = self.parameters.remove(&target.id).unwrap_or_default();
            entry.insert(contract(target, owned, &self.definitions, sources, check)?);
        }
        Ok(())
    }

    pub(super) fn has_empty_parameter_list(
        &mut self,
        target: &'a EntityRecord,
        sources: &BTreeMap<FileId, &[u8]>,
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<bool, ResolutionError> {
        let mut check = || {
            cancellation.check()?;
            work.consume()
        };
        self.ensure_contract(target, sources, &mut check)?;
        // Optional parameters can accept arguments and therefore cannot forward
        // an incompatible call to a returned object's default member (MS-VBAL 5.6.13).
        Ok(self
            .contracts
            .get(&target.id)
            .and_then(Option::as_ref)
            .is_some_and(|contract| contract.parameters.is_empty()))
    }

    pub(super) fn implicit_property_parameters(
        &mut self,
        target: &'a EntityRecord,
        setter: bool,
        sources: &BTreeMap<FileId, &[u8]>,
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<Option<Arc<Vec<&'a EntityRecord>>>, ResolutionError> {
        let mut check = || {
            cancellation.check()?;
            work.consume()
        };
        self.ensure_contract(target, sources, &mut check)?;
        let Some(contract) = self.contracts.get(&target.id).and_then(Option::as_ref) else {
            return Ok(None);
        };
        // A bare setter supplies its last (assigned-value) parameter. Every
        // omitted index must be optional; getters receive no implicit argument.
        let omitted = if setter {
            let Some((value, indices)) = contract.optional.split_last() else {
                return Ok(None);
            };
            if *value {
                return Ok(None);
            }
            indices
        } else {
            &contract.optional
        };
        for optional in omitted {
            check()?;
            if !optional {
                return Ok(None);
            }
        }
        Ok(Some(Arc::clone(&contract.parameters)))
    }
}

fn source_bytes<'s>(source: &SourceRef, sources: &BTreeMap<FileId, &'s [u8]>) -> Option<&'s [u8]> {
    let span = source.span();
    sources
        .get(&span.file())?
        .get(usize::try_from(span.start_byte()).ok()?..usize::try_from(span.end_byte()).ok()?)
}

fn contract<'a>(
    target: &EntityRecord,
    mut parameters: Vec<&'a EntityRecord>,
    definitions: &BTreeMap<SymbolId, Option<&SourceRef>>,
    sources: &BTreeMap<FileId, &[u8]>,
    check: &mut impl FnMut() -> Result<(), ResolutionError>,
) -> Result<Option<Contract<'a>>, ResolutionError> {
    let Some(source) = target.evidence.source.as_ref() else {
        return Ok(None);
    };
    let Some(bytes) = source_bytes(source, sources) else {
        return Ok(None);
    };
    let Some(header) = rootlight_vba::procedure_header(bytes, check)? else {
        return Ok(None);
    };
    if !header
        .name()
        .eq_ignore_ascii_case(target.canonical_name.as_bytes())
    {
        return Ok(None);
    }
    let Some(range) = rootlight_vba::procedure_parameter_range(bytes, check)? else {
        return Ok(None);
    };
    parameters.sort_by_key(|entity| {
        entity
            .evidence
            .source
            .as_ref()
            .map(|source| source.span().start_byte())
    });
    let Some(interior) = bytes.get(range.clone()) else {
        return Ok(None);
    };
    let parameter_start = source
        .span()
        .start_byte()
        .checked_add(u64::try_from(range.start).map_err(|_| ResolutionError::CountOverflow)?)
        .ok_or(ResolutionError::CountOverflow)?;
    enum CoverageFailure {
        IncompleteEvidence,
        Work(ResolutionError),
    }
    let declarations = parameters.iter().map(|parameter| {
        let declaration = parameter
            .evidence
            .source
            .as_ref()
            .ok_or(CoverageFailure::IncompleteEvidence)?;
        if declaration.span().file() != source.span().file() {
            return Err(CoverageFailure::IncompleteEvidence);
        }
        let start = declaration
            .span()
            .start_byte()
            .checked_sub(parameter_start)
            .and_then(|offset| usize::try_from(offset).ok())
            .ok_or(CoverageFailure::IncompleteEvidence)?;
        let end = declaration
            .span()
            .end_byte()
            .checked_sub(parameter_start)
            .and_then(|offset| usize::try_from(offset).ok())
            .ok_or(CoverageFailure::IncompleteEvidence)?;
        Ok(start..end)
    });
    // Preserve missing-source fail-closed behavior while forwarding the exact
    // work/cancellation error from the source-conservation scan.
    match rootlight_vba::parameter_declarations_cover(interior, declarations, &mut || {
        check().map_err(CoverageFailure::Work)
    }) {
        Ok(true) => {}
        Ok(false) | Err(CoverageFailure::IncompleteEvidence) => return Ok(None),
        Err(CoverageFailure::Work(error)) => return Err(error),
    }
    let mut optional = Vec::new();
    let mut names = BTreeMap::new();
    let mut variadic = false;
    for (position, parameter) in parameters.iter().enumerate() {
        check()?;
        let Some(declaration) = parameter.evidence.source.as_ref() else {
            return Ok(None);
        };
        let Some(definition) = definitions.get(&parameter.id).copied().flatten() else {
            return Ok(None);
        };
        if declaration.span().file() != source.span().file()
            || definition.span().file() != source.span().file()
            || declaration.span().start_byte() < source.span().start_byte()
            || declaration.span().end_byte() > source.span().end_byte()
            || definition.span().start_byte() < declaration.span().start_byte()
            || definition.span().end_byte() > declaration.span().end_byte()
        {
            return Ok(None);
        }
        let Some(raw) = source_bytes(declaration, sources) else {
            return Ok(None);
        };
        let Some(name_bytes) = source_bytes(definition, sources) else {
            return Ok(None);
        };
        if !rootlight_vba::procedure_pointer_path(name_bytes, check)?.is_some_and(|name| {
            name.components() == 1
                && name.end() == name_bytes.len()
                && name
                    .first()
                    .eq_ignore_ascii_case(parameter.canonical_name.as_bytes())
        }) {
            return Ok(None);
        }
        let Some(name_offset) = definition
            .span()
            .start_byte()
            .checked_sub(declaration.span().start_byte())
            .and_then(|offset| usize::try_from(offset).ok())
        else {
            return Ok(None);
        };
        if raw.get(..name_offset).is_none() {
            return Ok(None);
        }
        let is_optional = if let Some(name) = rootlight_vba::param_array_parameter(raw, check)? {
            if position + 1 != parameters.len()
                || optional.iter().any(|optional| *optional)
                || !name.eq_ignore_ascii_case(parameter.canonical_name.as_bytes())
                || matches!(
                    header.kind(),
                    rootlight_vba::ProcedureKind::PropertyLet
                        | rootlight_vba::ProcedureKind::PropertySet
                )
            {
                return Ok(None);
            }
            variadic = true;
            true
        } else {
            let Some(optional) = parameter_modifiers(raw, name_offset, check)? else {
                return Ok(None);
            };
            optional
        };
        if names
            .insert(parameter.canonical_name.to_ascii_lowercase(), position)
            .is_some()
        {
            return Ok(None);
        }
        optional.push(is_optional);
    }
    Ok(Some(Contract {
        parameters: Arc::new(parameters),
        optional,
        names,
        variadic,
        kind: header.kind(),
    }))
}

fn assignment_source(
    site: &OccurrenceRecord,
    sources: &BTreeMap<FileId, &[u8]>,
    check: &mut impl FnMut() -> Result<(), ResolutionError>,
) -> Result<Option<SourceRef>, ResolutionError> {
    let span = site.source.span();
    let Some(tail) = usize::try_from(span.end_byte())
        .ok()
        .and_then(|end| sources.get(&site.file)?.get(end..))
    else {
        return Ok(None);
    };
    // The assigned value supplies the setter's final formal parameter, outside
    // the index parentheses. Preserve this separate syntax proof in provenance.
    let Some(length) = rootlight_vba::assigned_value_end(tail, tail.len(), check)? else {
        return Ok(None);
    };
    let end = u64::try_from(length)
        .ok()
        .and_then(|length| span.end_byte().checked_add(length))
        .ok_or(ResolutionError::CountOverflow)?;
    let span = rootlight_ir::SourceSpan::new(site.file, span.start_byte(), end)
        .map_err(ResolutionError::InvalidProducer)?;
    Ok(Some(SourceRef::new(
        site.source.repository(),
        site.source.generation(),
        span,
        site.source.content_hash(),
        None,
    )))
}

fn parameter_modifiers(
    bytes: &[u8],
    name_offset: usize,
    check: &mut impl FnMut() -> Result<(), ResolutionError>,
) -> Result<Option<bool>, ResolutionError> {
    if name_offset > bytes.len() {
        return Ok(None);
    }
    let mut cursor = 0;
    let mut optional = false;
    let mut passing = false;
    loop {
        let Some(remaining) = bytes.get(cursor..) else {
            return Ok(None);
        };
        cursor = cursor
            .checked_add(rootlight_vba::parameter_trivia_len(remaining, check)?)
            .ok_or(ResolutionError::CountOverflow)?;
        if cursor > name_offset {
            return Ok(None);
        }
        if cursor == name_offset {
            return Ok(Some(optional));
        }
        let Some(remaining) = bytes.get(cursor..) else {
            return Ok(None);
        };
        let length = remaining
            .iter()
            .take(9)
            .take_while(|byte| byte.is_ascii_alphabetic())
            .count();
        let end = cursor
            .checked_add(length)
            .ok_or(ResolutionError::CountOverflow)?;
        if end > name_offset {
            return Ok(None);
        }
        let word = remaining.get(..length).unwrap_or_default();
        if word.eq_ignore_ascii_case(b"Optional") && !optional && !passing {
            optional = true;
        } else if (word.eq_ignore_ascii_case(b"ByVal") || word.eq_ignore_ascii_case(b"ByRef"))
            && !passing
        {
            passing = true;
        } else {
            // ParamArray is validated from its complete declaration, not a prefix.
            return Ok(None);
        }
        cursor = end;
        let Some(remaining) = bytes.get(cursor..) else {
            return Ok(None);
        };
        // Scan the full clause: a prefix ending in Rem must not turn the joined
        // spelling Remvalue into a comment merely by hiding its name bytes.
        if rootlight_vba::parameter_trivia_len(remaining, check)? == 0 {
            return Ok(None);
        }
    }
}

pub(super) fn standard_arguments_match(
    tail: &[u8],
    explicit: bool,
    parameters: &[(&str, bool)],
    procedure: rootlight_vba::ProcedureKind,
    check: &mut impl FnMut() -> Result<(), ResolutionError>,
) -> Result<bool, ResolutionError> {
    let optional = parameters
        .iter()
        .map(|(_, optional)| *optional)
        .collect::<Vec<_>>();
    let names = parameters
        .iter()
        .enumerate()
        .map(|(i, (name, _))| ((*name).to_owned(), i))
        .collect();
    Ok(arguments_match(tail, explicit, &optional, &names, false, procedure, check)?.is_some())
}

fn arguments_match(
    tail: &[u8],
    explicit: bool,
    optional_parameters: &[bool],
    names: &BTreeMap<String, usize>,
    variadic: bool,
    procedure: rootlight_vba::ProcedureKind,
    check: &mut impl FnMut() -> Result<(), ResolutionError>,
) -> Result<Option<Vec<NamedArgument>>, ResolutionError> {
    let leading = rootlight_vba::logical_whitespace_len(tail, check)?;
    let tail = tail.get(leading..).unwrap_or_default();
    let (arguments, offset) = if tail.first() == Some(&b'(') {
        let Some(end) = rootlight_vba::parenthesized_expression_end(tail, check)? else {
            return Ok(None);
        };
        let rest = tail.get(end..).unwrap_or_default();
        if rootlight_vba::logical_whitespace_len(rest, check)? == rest.len() {
            (tail.get(1..end - 1).unwrap_or_default(), leading + 1)
        } else {
            if explicit || leading == 0 {
                return Ok(None);
            }
            // A statement argument can begin with a grouped expression, then
            // continue with an operator or further comma-delimited arguments.
            (tail, leading)
        }
    } else {
        if (explicit || leading == 0) && !tail.is_empty() {
            return Ok(None);
        }
        (tail, leading)
    };
    // Variadic element count is bounded by the authored input and work budget,
    // not by the number of formal parameters.
    let maximum = if variadic {
        arguments.len()
    } else {
        optional_parameters.len()
    };
    let Some(arguments) = rootlight_vba::argument_list(arguments, maximum, check)? else {
        return Ok(None);
    };
    if variadic {
        let Some(required) = optional_parameters.len().checked_sub(1) else {
            return Ok(None);
        };
        return Ok(
            rootlight_vba::variadic_arguments_match(&arguments, required, check)?.then(Vec::new),
        );
    }
    let mut supplied = BTreeSet::new();
    let mut named = Vec::new();
    for (position, argument) in arguments.iter().enumerate() {
        check()?;
        if !rootlight_vba::argument_passing_matches(
            argument.expression(),
            procedure,
            argument.expression().len(),
            check,
        )? {
            return Ok(None);
        }
        let position = if let Some(name) = argument.name() {
            let Ok(name) = std::str::from_utf8(name) else {
                return Ok(None);
            };
            let Some(position) = names.get(&name.to_ascii_lowercase()) else {
                return Ok(None);
            };
            *position
        } else {
            position
        };
        let Some(optional) = optional_parameters.get(position) else {
            return Ok(None);
        };
        if !supplied.insert(position) || (argument.expression().is_empty() && !optional) {
            return Ok(None);
        }
        if let Some(span) = argument.name_span() {
            named.push(NamedArgument {
                span: offset + span.start..offset + span.end,
                parameter: position,
            });
        }
    }
    Ok(optional_parameters
        .iter()
        .enumerate()
        .all(|(position, optional)| *optional || supplied.contains(&position))
        .then_some(named))
}

#[cfg(test)]
mod tests {
    use super::*;

    type WrittenParameter<'a> = (&'a str, &'a str, &'a str);
    type ParameterContractCase<'a> = (&'a str, &'a [WrittenParameter<'a>], &'a [bool]);

    struct ParameterContractFixture {
        source: String,
        file: FileId,
        target: EntityRecord,
        parameters: Vec<EntityRecord>,
        definitions: BTreeMap<SymbolId, SourceRef>,
    }

    impl ParameterContractFixture {
        fn new(header: &str, parameters: &[WrittenParameter<'_>]) -> Self {
            use rootlight_ids::{FactId, GenerationId, RepositoryId, content_hash};
            use rootlight_ir::{AnalysisTier, EntityVisibility, FactEvidence, SourceSpan};

            let source =
                format!("Attribute VB_Name = \"Authored\"\nSub Target({header})\nEnd Sub\n");
            let repository = RepositoryId::from_bytes([1; 16]);
            let generation = GenerationId::from_bytes([2; 20]);
            let file = FileId::from_bytes([3; 20]);
            let target_id = SymbolId::from_bytes([4; 20]);
            let parser = FactId::from_bytes([5; 20]);
            let reference = |start, end| {
                SourceRef::new(
                    repository,
                    generation,
                    SourceSpan::new(
                        file,
                        u64::try_from(start).unwrap(),
                        u64::try_from(end).unwrap(),
                    )
                    .unwrap(),
                    content_hash(source.as_bytes()),
                    None,
                )
            };
            let entity = |id, kind, name: &str, container, start, end| EntityRecord {
                id,
                repository,
                generation,
                kind,
                language: "vba".to_owned(),
                tier: AnalysisTier::TierB,
                canonical_name: name.to_owned(),
                display_name: name.to_owned(),
                qualified_name: format!("Authored.Target.{name}"),
                container: Some(container),
                visibility: EntityVisibility::Public,
                flags: Vec::new(),
                provenance: parser,
                evidence: FactEvidence {
                    source: Some(reference(start, end)),
                    derivation: Vec::new(),
                },
            };
            let target = entity(
                target_id,
                EntityKind::Function,
                "Target",
                ContainerRef::File(file),
                source.find("Sub Target(").unwrap(),
                source.find("End Sub").unwrap() + "End Sub".len(),
            );
            let mut owned = Vec::new();
            let mut definitions = BTreeMap::new();
            for (index, &(written, name, token)) in parameters.iter().enumerate() {
                let id = SymbolId::from_bytes([u8::try_from(index + 6).unwrap(); 20]);
                let start = source.find(written).unwrap();
                owned.push(entity(
                    id,
                    EntityKind::Parameter,
                    name,
                    ContainerRef::Entity(target_id),
                    start,
                    start + written.len(),
                ));
                let definition_start = start + written.find(token).unwrap();
                definitions.insert(
                    id,
                    reference(definition_start, definition_start + token.len()),
                );
            }
            Self {
                source,
                file,
                target,
                parameters: owned,
                definitions,
            }
        }

        fn check<'a>(
            &'a self,
            parameters: Vec<&'a EntityRecord>,
            check: &mut impl FnMut() -> Result<(), ResolutionError>,
        ) -> Result<Option<Contract<'a>>, ResolutionError> {
            let definitions = self
                .definitions
                .iter()
                .map(|(id, source)| (*id, Some(source)))
                .collect();
            contract(
                &self.target,
                parameters,
                &definitions,
                &BTreeMap::from([(self.file, self.source.as_bytes())]),
                check,
            )
        }
    }

    #[test]
    fn parameter_contract_trivia_covers_only_exact_normalized_declarations() {
        let cases: &[ParameterContractCase<'_>] = &[
            ("Rem", &[], &[]),
            (
                "Remvalue",
                &[("Remvalue", "Remvalue", "Remvalue")],
                &[false],
            ),
            (
                "Rem[value], Optional extra = 7",
                &[
                    ("[value]", "value", "[value]"),
                    ("Optional extra = 7", "extra", "extra"),
                ],
                &[false, true],
            ),
            (
                "\x0cByVal\x0cvalue\x0cAs\x0cLong\x0c",
                &[("ByVal\x0cvalue\x0cAs\x0cLong", "value", "value")],
                &[false],
            ),
            (
                "ByVal Rem[value], Optional extra = \"x,y Rem\"",
                &[
                    ("ByVal Rem[value]", "value", "[value]"),
                    ("Optional extra = \"x,y Rem\"", "extra", "extra"),
                ],
                &[false, true],
            ),
        ];
        for &(header, written, expected_optional) in cases {
            let fixture = ParameterContractFixture::new(header, written);
            let contract = fixture
                .check(fixture.parameters.iter().rev().collect(), &mut || Ok(()))
                .unwrap()
                .unwrap();
            assert_eq!(contract.optional, expected_optional, "{header:?}");
            assert_eq!(
                contract
                    .parameters
                    .iter()
                    .map(|parameter| parameter.id)
                    .collect::<Vec<_>>(),
                fixture
                    .parameters
                    .iter()
                    .map(|parameter| parameter.id)
                    .collect::<Vec<_>>(),
            );
            assert_eq!(contract.names.len(), written.len());
            assert!(!contract.names.contains_key("rem"));
            let call = if written.is_empty() {
                b"()".as_slice()
            } else {
                b"(1)".as_slice()
            };
            assert!(
                arguments_match(
                    call,
                    true,
                    &contract.optional,
                    &contract.names,
                    contract.variadic,
                    contract.kind,
                    &mut || Ok(()),
                )
                .unwrap()
                .is_some()
            );
            assert!(
                arguments_match(
                    b"(1, 2, 3)",
                    true,
                    &contract.optional,
                    &contract.names,
                    contract.variadic,
                    contract.kind,
                    &mut || Ok(()),
                )
                .unwrap()
                .is_none()
            );
            if !written.is_empty() {
                assert!(
                    arguments_match(
                        b"()",
                        true,
                        &contract.optional,
                        &contract.names,
                        contract.variadic,
                        contract.kind,
                        &mut || Ok(()),
                    )
                    .unwrap()
                    .is_none()
                );
            }
        }
        let fixture = ParameterContractFixture::new(
            "Rem[value], Optional extra = 7",
            &[
                ("[value]", "value", "[value]"),
                ("Optional extra = 7", "extra", "extra"),
            ],
        );
        assert!(fixture.check(Vec::new(), &mut || Ok(())).unwrap().is_none());
        assert!(
            fixture
                .check(vec![&fixture.parameters[1]], &mut || Ok(()))
                .unwrap()
                .is_none()
        );
        assert!(
            fixture
                .check(
                    vec![
                        &fixture.parameters[0],
                        &fixture.parameters[0],
                        &fixture.parameters[1]
                    ],
                    &mut || Ok(())
                )
                .unwrap()
                .is_none()
        );
        let mut missing_source = fixture.parameters[0].clone();
        missing_source.evidence.source = None;
        assert!(
            fixture
                .check(
                    vec![&missing_source, &fixture.parameters[1]],
                    &mut || Ok(())
                )
                .unwrap()
                .is_none()
        );
        let mut moved = fixture.parameters[0].clone();
        let original = moved.evidence.source.as_ref().unwrap();
        moved.evidence.source = Some(SourceRef::new(
            original.repository(),
            original.generation(),
            rootlight_ir::SourceSpan::new(
                original.span().file(),
                original.span().start_byte() + 1,
                original.span().end_byte(),
            )
            .unwrap(),
            original.content_hash(),
            None,
        ));
        assert!(
            fixture
                .check(vec![&moved, &fixture.parameters[1]], &mut || Ok(()))
                .unwrap()
                .is_none()
        );
        let mut wrong_file = fixture.parameters[0].clone();
        let original = wrong_file.evidence.source.as_ref().unwrap();
        wrong_file.evidence.source = Some(SourceRef::new(
            original.repository(),
            original.generation(),
            rootlight_ir::SourceSpan::new(
                FileId::from_bytes([99; 20]),
                original.span().start_byte(),
                original.span().end_byte(),
            )
            .unwrap(),
            original.content_hash(),
            None,
        ));
        assert!(
            fixture
                .check(vec![&wrong_file, &fixture.parameters[1]], &mut || Ok(()))
                .unwrap()
                .is_none()
        );
        let mut wrong_name = fixture;
        let definition = wrong_name.definitions.values_mut().next().unwrap();
        *definition = SourceRef::new(
            definition.repository(),
            definition.generation(),
            rootlight_ir::SourceSpan::new(
                definition.span().file(),
                definition.span().start_byte() + 1,
                definition.span().end_byte(),
            )
            .unwrap(),
            definition.content_hash(),
            None,
        );
        assert!(
            wrong_name
                .check(wrong_name.parameters.iter().collect(), &mut || Ok(()))
                .unwrap()
                .is_none()
        );
        let comment_formal = ParameterContractFixture::new("Rem", &[("Rem", "Rem", "Rem")]);
        assert!(
            comment_formal
                .check(comment_formal.parameters.iter().collect(), &mut || Ok(()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn parameter_contract_trivia_preserves_callback_errors_and_work_bounds() {
        let fixture = ParameterContractFixture::new(
            "ByVal Rem[value], Optional extra = 7",
            &[
                ("ByVal Rem[value]", "value", "[value]"),
                ("Optional extra = 7", "extra", "extra"),
            ],
        );
        let mut measured = ResolutionWorkBudget::new(usize::MAX);
        assert!(
            fixture
                .check(fixture.parameters.iter().collect(), &mut || measured
                    .consume())
                .unwrap()
                .is_some()
        );
        let required = measured.consumed();
        assert!(required > 1);
        for limit in [0, required - 1] {
            let mut work = ResolutionWorkBudget::new(limit);
            assert!(matches!(
                fixture.check(fixture.parameters.iter().collect(), &mut || work.consume()),
                Err(ResolutionError::WorkLimit { .. })
            ));
        }
        let mut work = ResolutionWorkBudget::new(required);
        assert!(
            fixture
                .check(fixture.parameters.iter().collect(), &mut || work.consume())
                .unwrap()
                .is_some()
        );
        let sources = BTreeMap::from([(fixture.file, fixture.source.as_bytes())]);
        let bytes =
            source_bytes(fixture.target.evidence.source.as_ref().unwrap(), &sources).unwrap();
        let mut prefix_checks = 0;
        rootlight_vba::procedure_header(bytes, &mut || {
            prefix_checks += 1;
            Ok::<(), ResolutionError>(())
        })
        .unwrap()
        .unwrap();
        rootlight_vba::procedure_parameter_range(bytes, &mut || {
            prefix_checks += 1;
            Ok::<(), ResolutionError>(())
        })
        .unwrap()
        .unwrap();
        let mut checks = 0;
        assert!(matches!(
            fixture.check(fixture.parameters.iter().collect(), &mut || {
                checks += 1;
                if checks == prefix_checks + 1 {
                    Err(ResolutionError::CountOverflow)
                } else {
                    Ok(())
                }
            }),
            Err(ResolutionError::CountOverflow)
        ));
        assert_eq!(checks, prefix_checks + 1);
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            fixture.check(fixture.parameters.iter().collect(), &mut || {
                cancellation.check()?;
                Ok(())
            }),
            Err(ResolutionError::Cancelled(_))
        ));
    }

    #[test]
    fn unresolved_calls_return_the_selected_callee_without_repeating_work() {
        use rootlight_ids::{FactId, GenerationId, RepositoryId, content_hash};
        use rootlight_ir::{Confidence, FactEvidence, SourceSpan};

        for (bytes, start, explicit) in [
            (b"Worker(1)".as_slice(), 0, false),
            (b"Call Worker(1)".as_slice(), 5, true),
        ] {
            let repository = RepositoryId::from_bytes([1; 16]);
            let generation = GenerationId::from_bytes([2; 20]);
            let file = FileId::from_bytes([3; 20]);
            let source = |start, end| {
                SourceRef::new(
                    repository,
                    generation,
                    SourceSpan::new(file, start, end).unwrap(),
                    content_hash(bytes),
                    None,
                )
            };
            let callee = OccurrenceRecord {
                id: FactId::from_bytes([4; 20]),
                repository,
                generation,
                file,
                source: source(start, start + 6),
                role: OccurrenceRole::Reference,
                enclosing: None,
                target: OccurrenceTarget::Unresolved {
                    text_hash: content_hash(b"Worker"),
                },
                syntactic_text_hash: content_hash(b"Worker"),
                syntax_kind: "vba.invoked_identifier.reference".to_owned(),
                provenance: FactId::from_bytes([5; 20]),
                confidence: Confidence::new(0).unwrap(),
                evidence: FactEvidence {
                    source: None,
                    derivation: Vec::new(),
                },
            };
            let mut application = callee.clone();
            application.id = FactId::from_bytes([6; 20]);
            application.source = source(0, u64::try_from(bytes.len()).unwrap());
            application.syntax_kind = "vba.application.reference".to_owned();
            let mut index = CallIndex {
                parameters: BTreeMap::new(),
                definitions: BTreeMap::new(),
                callees: BTreeMap::from([((file, start, start + 6), Some(&callee))]),
                labels: BTreeMap::new(),
                applications: BTreeMap::new(),
                contracts: BTreeMap::new(),
            };
            let sources = BTreeMap::from([(file, bytes)]);
            let cancellation = Cancellation::new();
            let mut measured = ResolutionWorkBudget::new(1024);
            let selected = index
                .callee(&application, &sources, &cancellation, &mut measured)
                .unwrap();
            let mut exact = ResolutionWorkBudget::new(0);
            let mut result = ClassicIndex::default();
            let selected = index
                .resolve(
                    &mut result,
                    &application,
                    selected,
                    &sources,
                    &cancellation,
                    &mut exact,
                )
                .unwrap()
                .unwrap();
            assert_eq!((selected.0.id, selected.1), (callee.id, explicit));
            assert_eq!(exact.remaining(), 0);
            assert!(result.proof(&application).is_none());

            let mut insufficient = ResolutionWorkBudget::new(measured.consumed() - 1);
            assert!(matches!(
                index.callee(&application, &sources, &cancellation, &mut insufficient),
                Err(ResolutionError::WorkLimit { .. })
            ));
            cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
            assert!(matches!(
                index.resolve(
                    &mut result,
                    &application,
                    Some(selected),
                    &sources,
                    &cancellation,
                    &mut measured
                ),
                Err(ResolutionError::Cancelled(_))
            ));
        }
    }

    #[test]
    fn named_mapping_keeps_authored_spans_and_excludes_nested_labels() {
        let names = BTreeMap::from([("value".to_owned(), 0), ("extra".to_owned(), 1)]);
        for tail in [
            "(extra:=Nested(value:=1), [VALUE] := 2)",
            " extra:=Nested(value:=1), [VALUE] _\r\n := 2",
        ] {
            let bindings = arguments_match(
                tail.as_bytes(),
                false,
                &[false, true],
                &names,
                false,
                rootlight_vba::ProcedureKind::Function,
                &mut || Ok(()),
            )
            .unwrap()
            .unwrap();
            let actual: Vec<_> = bindings
                .iter()
                .map(|binding| (&tail[binding.span.clone()], binding.parameter))
                .collect();
            assert_eq!(actual, [("extra", 1), ("[VALUE]", 0)], "{tail}");
        }
    }

    #[test]
    fn argument_mapping_preserves_required_optional_and_named_slots() {
        let names = BTreeMap::from([("value".to_owned(), 0), ("extra".to_owned(), 1)]);
        for (tail, explicit, expected) in [
            (" 1", false, true),
            (" value:=1", false, true),
            (" extra:=2, [VALUE]:=1", false, true),
            (" 1, extra:=2", false, true),
            (" (1), 2", false, true),
            ("(1, extra:=2)", true, true),
            (" _\r\n value:=1", false, true),
            ("", false, false),
            (" extra:=2", false, false),
            (" , 2", false, false),
            (" 1, value:=2", false, false),
            (" value:=1, VALUE:=2", false, false),
            (" value:=1, 2", false, false),
            (" unknown:=1", false, false),
            (" ByVal 1", false, false),
            (" 1, 2, 3", false, false),
            ("(1)(2)", true, false),
            ("(1", true, false),
            (" 1", true, false),
        ] {
            let mut check = || Ok(());
            assert_eq!(
                arguments_match(
                    tail.as_bytes(),
                    explicit,
                    &[false, true],
                    &names,
                    false,
                    rootlight_vba::ProcedureKind::Function,
                    &mut check
                )
                .unwrap()
                .is_some(),
                expected,
                "{tail:?} explicit={explicit}"
            );
        }
        assert!(
            arguments_match(
                b" , 2",
                false,
                &[true, true],
                &names,
                false,
                rootlight_vba::ProcedureKind::Function,
                &mut || Ok(())
            )
            .unwrap()
            .is_some()
        );
        assert!(
            arguments_match(
                b"()",
                true,
                &[],
                &BTreeMap::new(),
                false,
                rootlight_vba::ProcedureKind::Function,
                &mut || Ok(())
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn parameter_prefixes_reject_variadic_and_conflicting_modifiers() {
        for (prefix, expected) in [
            ("", Some(false)),
            ("ByRef ", Some(false)),
            ("Optional _\r\n ByVal ", Some(true)),
            ("ParamArray ", None),
            ("ByVal ByRef ", None),
            ("Optional Optional ", None),
            ("ByVal Optional ", None),
            ("ByVal", None),
            ("ByVal\n", None),
        ] {
            let source = format!("{prefix}value");
            assert_eq!(
                parameter_modifiers(source.as_bytes(), prefix.len(), &mut || Ok(())).unwrap(),
                expected,
                "{prefix:?}"
            );
        }
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            parameter_modifiers(b"Optional value", "Optional ".len(), &mut || {
                cancellation.check()?;
                Ok(())
            }),
            Err(ResolutionError::Cancelled(_))
        ));
        let mut work = ResolutionWorkBudget::new(0);
        assert!(matches!(
            arguments_match(
                b" 1",
                false,
                &[false],
                &BTreeMap::new(),
                false,
                rootlight_vba::ProcedureKind::Function,
                &mut || { work.consume() }
            ),
            Err(ResolutionError::WorkLimit { .. })
        ));
    }

    #[test]
    fn parameter_modifier_trivia_uses_full_clauses_and_exact_name_boundaries() {
        for (source, name, expected) in [
            ("ByVal\x0cvalue", "value", Some(false)),
            ("ByVal Rem[value]", "[value]", Some(false)),
            ("Optional\x0cvalue = 7", "value", Some(true)),
            (
                "Optional Rem\x0cByRef Rem[value] = \"Remvalue\"",
                "[value]",
                Some(true),
            ),
            ("ByRef Remvalue", "Remvalue", Some(false)),
            ("ByVal Rem$", "Rem$", Some(false)),
            ("ByVal Rem[value]", "value", None),
            ("Optionality value", "value", None),
            ("ByVal Optional value", "value", None),
            ("Optional Rem value", "value", None),
        ] {
            assert_eq!(
                parameter_modifiers(
                    source.as_bytes(),
                    source.find(name).unwrap(),
                    &mut || Ok(()),
                )
                .unwrap(),
                expected,
                "{source:?}: {name:?}",
            );
        }
        assert_eq!(
            parameter_modifiers(b"ByVal value", 5, &mut || Ok(())).unwrap(),
            None,
        );
        assert_eq!(
            parameter_modifiers(b"value", 6, &mut || Ok(())).unwrap(),
            None,
        );
    }

    #[test]
    fn parameter_modifier_trivia_preserves_work_limits_and_cancellation() {
        let source = b"Optional Rem\x0cByRef Rem[value] = 7";
        let name_offset = "Optional Rem\x0cByRef Rem".len();
        let mut required = 0;
        assert_eq!(
            parameter_modifiers(source, name_offset, &mut || {
                required += 1;
                Ok(())
            })
            .unwrap(),
            Some(true),
        );
        assert!(required > 1);
        for limit in [0, required - 1] {
            let mut work = ResolutionWorkBudget::new(limit);
            assert!(matches!(
                parameter_modifiers(source, name_offset, &mut || work.consume()),
                Err(ResolutionError::WorkLimit { .. }),
            ));
        }
        let mut work = ResolutionWorkBudget::new(required);
        assert_eq!(
            parameter_modifiers(source, name_offset, &mut || work.consume()).unwrap(),
            Some(true),
        );
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            parameter_modifiers(source, name_offset, &mut || {
                cancellation.check()?;
                Ok(())
            }),
            Err(ResolutionError::Cancelled(_)),
        ));
    }
}
