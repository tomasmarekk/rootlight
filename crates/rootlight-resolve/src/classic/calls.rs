//! Whole-call evidence built on exact project-owned member proofs.
//! Native parameter inventories must agree with the complete authored signature;
//! application references never add a second call edge for the same invocation.

use super::{ClassicIndex, Proof};
use crate::{
    ResolutionError, ResolutionRule,
    engine::{CandidateIndex, ResolutionWorkBudget},
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{FileId, SymbolId};
use rootlight_ir::{
    ContainerRef, EntityKind, EntityRecord, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    SourceRef,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

struct Contract<'a> {
    parameters: Arc<Vec<&'a EntityRecord>>,
    optional: Vec<bool>,
    names: BTreeMap<String, usize>,
}

pub(super) struct CallIndex<'a, 'o> {
    parameters: BTreeMap<SymbolId, Vec<&'a EntityRecord>>,
    definitions: BTreeMap<SymbolId, Option<&'o SourceRef>>,
    callees: BTreeMap<(FileId, u64, u64), Option<&'o OccurrenceRecord>>,
    applications: BTreeMap<(FileId, u64), BTreeSet<u64>>,
    contracts: BTreeMap<SymbolId, Option<Contract<'a>>>,
}

impl<'a, 'o> CallIndex<'a, 'o> {
    pub(super) fn build(
        index: &CandidateIndex<'a>,
        occurrences: &'o [OccurrenceRecord],
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<Self, ResolutionError> {
        let mut parameters: BTreeMap<SymbolId, Vec<&EntityRecord>> = BTreeMap::new();
        for entity in index.entities.values() {
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
        let mut applications: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
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
            if site.syntax_kind == "vba.application.reference" {
                let end = site.source.span().end_byte();
                applications.entry(key).or_default().insert(end);
            }
            if site.syntax_kind == "vba.invoked_member.reference" {
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
        if self.applications.get(&key).is_some_and(|ends| {
            ends.range(callee.source.span().end_byte()..site.source.span().end_byte())
                .next()
                .is_some()
        }) {
            return Ok(None);
        }
        Ok(Some((callee, explicit)))
    }

    pub(super) fn resolve(
        &mut self,
        result: &mut ClassicIndex<'a>,
        site: &OccurrenceRecord,
        sources: &BTreeMap<FileId, &[u8]>,
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<(), ResolutionError> {
        let Some((callee, explicit)) = self.callee(site, sources, cancellation, work)? else {
            return Ok(());
        };
        if !result.is_call(callee) {
            return Ok(());
        }
        let Some(bytes) = source_bytes(&site.source, sources) else {
            return Ok(());
        };
        let mut check = || {
            cancellation.check()?;
            work.consume()
        };
        let Some(proof) = result.proof(callee) else {
            return Ok(());
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
        if let std::collections::btree_map::Entry::Vacant(entry) =
            self.contracts.entry(proof.target.id)
        {
            let owned = self.parameters.remove(&proof.target.id).unwrap_or_default();
            entry.insert(contract(
                proof.target,
                owned,
                &self.definitions,
                sources,
                &mut check,
            )?);
        }
        let Some(contract) = self
            .contracts
            .get(&proof.target.id)
            .and_then(Option::as_ref)
        else {
            return Ok(());
        };
        if !arguments_match(
            tail,
            explicit,
            &contract.optional,
            &contract.names,
            &mut check,
        )? {
            return Ok(());
        }
        let application_proof = Proof {
            target: proof.target,
            context: Arc::clone(&proof.context),
            receiver: proof.receiver.clone(),
            rule: ResolutionRule::ClassicProjectMember,
            getter_read: false,
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
        result.application_parameters.insert(site.id, parameters);
        if let Some(sources) = result.receiver_sources.get(&callee.id).cloned() {
            result.receiver_sources.insert(site.id, sources);
        }
        result.proofs.insert(site.id, application_proof);
        Ok(())
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
    if !matches!(
        header.kind(),
        rootlight_vba::ProcedureKind::Sub | rootlight_vba::ProcedureKind::Function
    ) || !header
        .name()
        .eq_ignore_ascii_case(target.canonical_name.as_bytes())
    {
        return Ok(None);
    }
    let tail = bytes.get(header.name_span().end..).unwrap_or_default();
    let leading = rootlight_vba::logical_whitespace_len(tail, check)?;
    let tail = tail.get(leading..).unwrap_or_default();
    let Some(end) = rootlight_vba::parenthesized_expression_end(tail, check)? else {
        return Ok(None);
    };
    let Some(written) = rootlight_vba::argument_list(
        tail.get(1..end - 1).unwrap_or_default(),
        parameters.len(),
        check,
    )?
    else {
        return Ok(None);
    };
    if written.len() != parameters.len() {
        return Ok(None);
    }
    parameters.sort_by_key(|entity| {
        entity
            .evidence
            .source
            .as_ref()
            .map(|source| source.span().start_byte())
    });
    let mut optional = Vec::new();
    let mut names = BTreeMap::new();
    for (position, (parameter, written)) in parameters.iter().zip(written).enumerate() {
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
        if raw.trim_ascii() != written.expression().trim_ascii() {
            return Ok(None);
        }
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
        let Some(prefix) = raw.get(..name_offset) else {
            return Ok(None);
        };
        let Some(is_optional) = parameter_modifiers(prefix, check)? else {
            return Ok(None);
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
    }))
}

fn parameter_modifiers(
    mut bytes: &[u8],
    check: &mut impl FnMut() -> Result<(), ResolutionError>,
) -> Result<Option<bool>, ResolutionError> {
    let mut optional = false;
    let mut passing = false;
    loop {
        bytes = bytes
            .get(rootlight_vba::logical_whitespace_len(bytes, check)?..)
            .unwrap_or_default();
        if bytes.is_empty() {
            return Ok(Some(optional));
        }
        let length = bytes
            .iter()
            .take(9)
            .take_while(|byte| byte.is_ascii_alphabetic())
            .count();
        let word = bytes.get(..length).unwrap_or_default();
        if word.eq_ignore_ascii_case(b"Optional") && !optional && !passing {
            optional = true;
        } else if (word.eq_ignore_ascii_case(b"ByVal") || word.eq_ignore_ascii_case(b"ByRef"))
            && !passing
        {
            passing = true;
        } else {
            // ParamArray requires a separate variadic argument contract.
            return Ok(None);
        }
        bytes = bytes.get(length..).unwrap_or_default();
        if rootlight_vba::logical_whitespace_len(bytes, check)? == 0 {
            return Ok(None);
        }
    }
}

fn arguments_match(
    tail: &[u8],
    explicit: bool,
    optional_parameters: &[bool],
    names: &BTreeMap<String, usize>,
    check: &mut impl FnMut() -> Result<(), ResolutionError>,
) -> Result<bool, ResolutionError> {
    let leading = rootlight_vba::logical_whitespace_len(tail, check)?;
    let tail = tail.get(leading..).unwrap_or_default();
    let arguments = if tail.first() == Some(&b'(') {
        let Some(end) = rootlight_vba::parenthesized_expression_end(tail, check)? else {
            return Ok(false);
        };
        let rest = tail.get(end..).unwrap_or_default();
        if rootlight_vba::logical_whitespace_len(rest, check)? == rest.len() {
            tail.get(1..end - 1).unwrap_or_default()
        } else {
            if explicit || leading == 0 {
                return Ok(false);
            }
            // A statement argument can begin with a grouped expression, then
            // continue with an operator or further comma-delimited arguments.
            tail
        }
    } else {
        if (explicit || leading == 0) && !tail.is_empty() {
            return Ok(false);
        }
        tail
    };
    let Some(arguments) =
        rootlight_vba::argument_list(arguments, optional_parameters.len(), check)?
    else {
        return Ok(false);
    };
    let mut supplied = BTreeSet::new();
    for (position, argument) in arguments.iter().enumerate() {
        check()?;
        if argument
            .expression()
            .get(..5)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"ByVal"))
            && argument
                .expression()
                .get(5)
                .is_some_and(u8::is_ascii_whitespace)
        {
            return Ok(false);
        }
        let position = if let Some(name) = argument.name() {
            let Ok(name) = std::str::from_utf8(name) else {
                return Ok(false);
            };
            let Some(position) = names.get(&name.to_ascii_lowercase()) else {
                return Ok(false);
            };
            *position
        } else {
            position
        };
        let Some(optional) = optional_parameters.get(position) else {
            return Ok(false);
        };
        if !supplied.insert(position) || (argument.expression().is_empty() && !optional) {
            return Ok(false);
        }
    }
    Ok(optional_parameters
        .iter()
        .enumerate()
        .all(|(position, optional)| *optional || supplied.contains(&position)))
}

#[cfg(test)]
mod tests {
    use super::*;

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
                    &mut check
                )
                .unwrap(),
                expected,
                "{tail:?} explicit={explicit}"
            );
        }
        assert!(arguments_match(b" , 2", false, &[true, true], &names, &mut || Ok(())).unwrap());
        assert!(arguments_match(b"()", true, &[], &BTreeMap::new(), &mut || Ok(())).unwrap());
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
            assert_eq!(
                parameter_modifiers(prefix.as_bytes(), &mut || Ok(())).unwrap(),
                expected,
                "{prefix:?}"
            );
        }
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            parameter_modifiers(b"Optional ", &mut || {
                cancellation.check()?;
                Ok(())
            }),
            Err(ResolutionError::Cancelled(_))
        ));
        let mut work = ResolutionWorkBudget::new(0);
        assert!(matches!(
            arguments_match(b" 1", false, &[false], &BTreeMap::new(), &mut || work
                .consume()),
            Err(ResolutionError::WorkLimit { .. })
        ));
    }
}
