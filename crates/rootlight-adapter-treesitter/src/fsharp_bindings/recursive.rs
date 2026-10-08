//! Complete native recursive-group witnesses without changing authored identities.
//! Exact prefix/member/separator tiling prevents clipped captures from shrinking
//! membership. Every unavailable member remains a closed lexical binding.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{FileId, SymbolId};

use super::{
    DEFINITION, FSharpBindings, FUNCTION, UniqueFact, allocation_failed, contains, function_owner,
    invalid_capture, source_bytes, source_name, unqualified_written_name,
};

const GROUP: &str = "fsharp.recursive_group.expression";
const PREFIX: &str = "fsharp.recursive_prefix.expression";
const MEMBER: &str = "fsharp.recursive_member.expression";
const SEPARATOR: &str = "fsharp.recursive_separator.expression";

#[derive(Clone, Copy)]
pub(super) struct Member {
    pub(super) group: u64,
    pub(super) start: u64,
    pub(super) end: u64,
}

#[derive(Clone, Copy)]
struct Group<'a> {
    module: u64,
    fact: &'a SyntaxFact,
    ambiguous: bool,
}

impl Group<'_> {
    fn key(&self) -> (u64, FileId, u64, u64) {
        let span = self.fact.span();
        (self.module, span.file(), span.start_byte(), span.end_byte())
    }

    fn scope(&self) -> (u64, FileId) {
        (self.module, self.fact.span().file())
    }
}

#[derive(Clone, Copy)]
struct Tile<'a> {
    module: u64,
    fact: &'a SyntaxFact,
}

impl Tile<'_> {
    fn key(self) -> (u64, FileId, u64, u64) {
        let span = self.fact.span();
        (self.module, span.file(), span.start_byte(), span.end_byte())
    }

    fn start(self) -> (u64, FileId, u64) {
        (
            self.module,
            self.fact.span().file(),
            self.fact.span().start_byte(),
        )
    }
}

struct ProvenMember<'a> {
    declaration: u64,
    name: &'a str,
    symbol: Option<SymbolId>,
}

#[derive(Default)]
pub(super) struct RecursiveBindings<'a> {
    members: HashMap<u64, Member>,
    bindings: HashMap<(u64, &'a str), Option<SymbolId>>,
}

impl<'a> RecursiveBindings<'a> {
    pub(super) fn new(
        plan: &FSharpBindings<'a>,
        symbols: &HashMap<u64, SymbolId>,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            members: HashMap::new(),
            bindings: HashMap::new(),
        };
        let mut has_group = false;
        for fact in plan.facts.values() {
            cancellation.check()?;
            if fact.syntax_kind().as_str() == GROUP {
                has_group = true;
                break;
            }
        }
        if !has_group {
            return Ok(result);
        }
        let mut groups = Vec::new();
        let mut tiles = Vec::new();
        let mut definitions = HashMap::<u64, UniqueFact<'a>>::new();
        for fact in plan.facts.values().copied() {
            cancellation.check()?;
            let label = fact.syntax_kind().as_str();
            if matches!(label, GROUP | PREFIX | MEMBER | SEPARATOR) {
                source_bytes(fact, plan.source)?;
                let Some(module) = tile_module(fact, plan) else {
                    continue;
                };
                if label == GROUP {
                    groups.try_reserve(1).map_err(allocation_failed)?;
                    groups.push(Group {
                        module: module.local_id(),
                        fact,
                        ambiguous: false,
                    });
                } else {
                    tiles.try_reserve(1).map_err(allocation_failed)?;
                    tiles.push(Tile {
                        module: module.local_id(),
                        fact,
                    });
                }
            } else if label.ends_with(".definition")
                && let Some(owner) = function_owner(fact, &plan.facts)
            {
                definitions.try_reserve(1).map_err(allocation_failed)?;
                definitions.entry(owner).or_default().retain(fact)?;
            }
        }
        crate::runtime::sort_cancellable_by(&mut groups, cancellation, |left, right| {
            left.key().cmp(&right.key())
        })?;
        crate::runtime::sort_cancellable_by(&mut tiles, cancellation, |left, right| {
            left.key().cmp(&right.key())
        })?;
        let mut tile_ends = Vec::new();
        tile_ends
            .try_reserve(tiles.len())
            .map_err(allocation_failed)?;
        let mut latest_end = None::<((u64, FileId), u64)>;
        for tile in &tiles {
            cancellation.check()?;
            let scope = (tile.module, tile.fact.span().file());
            let end = latest_end
                .filter(|(previous, _)| *previous == scope)
                .map_or(tile.fact.span().end_byte(), |(_, end)| {
                    end.max(tile.fact.span().end_byte())
                });
            latest_end = Some((scope, end));
            tile_ends.push((scope, end));
        }
        let mut furthest = None::<usize>;
        for index in 0..groups.len() {
            cancellation.check()?;
            if let Some(previous) = furthest {
                let previous_group = groups.get(previous).ok_or_else(invalid_capture)?;
                let current = groups.get(index).ok_or_else(invalid_capture)?;
                if previous_group.scope() == current.scope()
                    && previous_group.fact.span().end_byte() > current.fact.span().start_byte()
                {
                    groups
                        .get_mut(previous)
                        .ok_or_else(invalid_capture)?
                        .ambiguous = true;
                    groups.get_mut(index).ok_or_else(invalid_capture)?.ambiguous = true;
                }
                let previous_group = groups.get(previous).ok_or_else(invalid_capture)?;
                let current = groups.get(index).ok_or_else(invalid_capture)?;
                if previous_group.scope() != current.scope()
                    || previous_group.fact.span().end_byte() < current.fact.span().end_byte()
                {
                    furthest = Some(index);
                }
            } else {
                furthest = Some(index);
            }
        }
        let mut proven = Vec::new();
        for group in groups {
            cancellation.check()?;
            let span = group.fact.span();
            if group.ambiguous
                || group.fact.kind() != SyntaxFactKind::Signature
                || group.fact.parent() != Some(group.module)
                || span.start_byte() >= span.end_byte()
            {
                continue;
            }
            let start = (group.module, span.file(), span.start_byte());
            let end = (group.module, span.file(), span.end_byte());
            let first = tiles.partition_point(|tile| tile.start() < start);
            let after = tiles.partition_point(|tile| tile.start() < end);
            if first
                .checked_sub(1)
                .and_then(|index| tile_ends.get(index))
                .is_some_and(|(scope, end)| *scope == group.scope() && *end > span.start_byte())
            {
                continue;
            }
            proven.clear();
            let mut position = span.start_byte();
            let mut expected = PREFIX;
            let mut valid = true;
            for tile in tiles.get(first..after).ok_or_else(invalid_capture)? {
                cancellation.check()?;
                let fact = tile.fact;
                if fact.kind() != SyntaxFactKind::Signature
                    || fact.syntax_kind().as_str() != expected
                    || fact.span().start_byte() != position
                    || fact.span().start_byte() >= fact.span().end_byte()
                    || !contains(group.fact, fact)
                {
                    valid = false;
                    break;
                }
                if expected == MEMBER {
                    let Some(member) = prove_member(fact, &group, plan, &definitions, symbols)?
                    else {
                        valid = false;
                        break;
                    };
                    proven.try_reserve(1).map_err(allocation_failed)?;
                    proven.push(member);
                    expected = SEPARATOR;
                } else {
                    if fact.parent() != Some(group.module) {
                        valid = false;
                        break;
                    }
                    expected = MEMBER;
                }
                position = fact.span().end_byte();
            }
            if !valid || expected != SEPARATOR || proven.is_empty() || position != span.end_byte() {
                continue;
            }
            result
                .members
                .try_reserve(proven.len())
                .map_err(allocation_failed)?;
            result
                .bindings
                .try_reserve(proven.len())
                .map_err(allocation_failed)?;
            for member in proven.drain(..) {
                cancellation.check()?;
                result.members.insert(
                    member.declaration,
                    Member {
                        group: group.fact.local_id(),
                        start: span.start_byte(),
                        end: span.end_byte(),
                    },
                );
                // A duplicated or unavailable member shadows older homonyms;
                // absence from this table is the only license for outer lookup.
                result
                    .bindings
                    .entry((group.fact.local_id(), member.name))
                    .and_modify(|symbol| *symbol = None)
                    .or_insert(member.symbol);
            }
        }
        Ok(result)
    }

    pub(super) fn member(&self, declaration: u64) -> Option<Member> {
        self.members.get(&declaration).copied()
    }

    pub(super) fn resolve(&self, group: u64, name: &str) -> Option<Option<SymbolId>> {
        self.bindings.get(&(group, name)).copied()
    }

    pub(super) fn close(&mut self, group: u64, name: &'a str) {
        if let Some(symbol) = self.bindings.get_mut(&(group, name)) {
            *symbol = None;
        }
    }
}

fn tile_module<'a>(fact: &SyntaxFact, plan: &FSharpBindings<'a>) -> Option<&'a SyntaxFact> {
    let parent = fact.parent().and_then(|id| plan.facts.get(&id).copied())?;
    let module = if super::lexical::module_scope(parent) {
        parent
    } else if parent.kind() == SyntaxFactKind::Declaration {
        parent
            .parent()
            .and_then(|id| plan.facts.get(&id).copied())?
    } else if parent.kind() == SyntaxFactKind::Scope {
        let declaration = parent
            .parent()
            .and_then(|id| plan.facts.get(&id).copied())?;
        declaration
            .parent()
            .and_then(|id| plan.facts.get(&id).copied())?
    } else {
        return None;
    };
    (super::lexical::module_scope(module) && contains(module, fact)).then_some(module)
}

fn prove_member<'a>(
    fact: &SyntaxFact,
    group: &Group<'_>,
    plan: &FSharpBindings<'a>,
    definitions: &HashMap<u64, UniqueFact<'a>>,
    symbols: &HashMap<u64, SymbolId>,
) -> Result<Option<ProvenMember<'a>>, AdapterError> {
    let Some(declaration) = fact.parent().and_then(|id| plan.facts.get(&id).copied()) else {
        return Ok(None);
    };
    let Some(function) = plan.functions.get(&declaration.local_id()) else {
        return Ok(None);
    };
    if declaration.kind() != SyntaxFactKind::Declaration
        || declaration.syntax_kind().as_str() != FUNCTION
        || declaration.parent() != Some(group.module)
        || declaration.span() != fact.span()
        || !function.valid_header()
    {
        return Ok(None);
    }
    let (Some(scope), Some(header), Some(definition)) = (
        function.scope.one(),
        function.header.one(),
        definitions
            .get(&declaration.local_id())
            .and_then(UniqueFact::one),
    ) else {
        return Ok(None);
    };
    if definition.kind() != SyntaxFactKind::Occurrence
        || definition.syntax_kind().as_str() != DEFINITION
        || !contains(header, definition)
        || (definition.parent() != Some(declaration.local_id())
            && definition.parent() != Some(scope.local_id()))
    {
        return Ok(None);
    }
    let Some(name) = source_name(definition, plan.source, plan.maximum_name_bytes)? else {
        return Ok(None);
    };
    if !unqualified_written_name(definition, name, plan.maximum_name_bytes) {
        return Ok(None);
    }
    let symbol = symbols
        .get(&definition.local_id())
        .copied()
        .filter(|symbol| !plan.conflicting_symbols.contains(symbol));
    Ok(Some(ProvenMember {
        declaration: declaration.local_id(),
        name,
        symbol,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ir::SourceSpan;

    const SOURCE: &str = "let rec first x = second x\nand second y = first y\n";

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        label: &str,
        start: usize,
        end: usize,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            SourceSpan::new(
                FileId::from_bytes([7; 20]),
                u64::try_from(start).unwrap(),
                u64::try_from(end).unwrap(),
            )
            .unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn function(
        source: &str,
        id: u64,
        name: &str,
        parameter: &str,
        callee: &str,
    ) -> Vec<SyntaxFact> {
        let start = source.find(&format!("{name} {parameter} =")).unwrap();
        let tail = &source[start..];
        let end = start + tail.find('\n').unwrap_or(tail.len());
        let header_end = start + tail.find('=').unwrap();
        let parameter_start = start + name.len() + 1;
        let body_start = header_end + 2;
        let argument_start = body_start + callee.len() + 1;
        vec![
            fact(
                id,
                Some(1),
                SyntaxFactKind::Declaration,
                FUNCTION,
                start,
                end,
            ),
            fact(
                id + 1,
                Some(id),
                SyntaxFactKind::Scope,
                super::super::SCOPE,
                start,
                end,
            ),
            fact(
                id + 2,
                Some(id + 1),
                SyntaxFactKind::Signature,
                super::super::HEADER,
                start,
                header_end,
            ),
            fact(
                id + 3,
                Some(id + 1),
                SyntaxFactKind::Signature,
                super::super::PARAMETERS,
                parameter_start,
                parameter_start + parameter.len(),
            ),
            fact(
                id + 4,
                Some(id + 1),
                SyntaxFactKind::Declaration,
                super::super::PARAMETER,
                parameter_start,
                parameter_start + parameter.len(),
            ),
            fact(
                id + 5,
                Some(id + 4),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                parameter_start,
                parameter_start + parameter.len(),
            ),
            fact(
                id + 6,
                Some(id + 1),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                start,
                start + name.len(),
            ),
            fact(
                id + 7,
                Some(id + 1),
                SyntaxFactKind::Occurrence,
                super::super::READ,
                body_start,
                body_start + callee.len(),
            ),
            fact(
                id + 8,
                Some(id + 1),
                SyntaxFactKind::Occurrence,
                super::super::lexical::CALL,
                body_start,
                end,
            ),
            fact(
                id + 9,
                Some(id + 1),
                SyntaxFactKind::Signature,
                "fsharp.application_argument.expression",
                argument_start,
                end,
            ),
            fact(
                id + 10,
                Some(id + 1),
                SyntaxFactKind::Occurrence,
                super::super::READ,
                argument_start,
                end,
            ),
        ]
    }

    fn facts(source: &str) -> Vec<SyntaxFact> {
        let mut result = vec![fact(
            1,
            None,
            SyntaxFactKind::Scope,
            "fsharp.file.scope",
            0,
            source.len(),
        )];
        let first = function(source, 10, "first", "x", "second");
        let second = function(source, 30, "second", "y", "first");
        let first_span = first[0].span();
        let second_span = second[0].span();
        let group_start = source.find("let rec").unwrap();
        let group_end = usize::try_from(second_span.end_byte()).unwrap();
        result.extend(first);
        result.extend(second);
        result.extend([
            fact(
                100,
                Some(1),
                SyntaxFactKind::Signature,
                GROUP,
                group_start,
                group_end,
            ),
            fact(
                101,
                Some(1),
                SyntaxFactKind::Signature,
                PREFIX,
                group_start,
                usize::try_from(first_span.start_byte()).unwrap(),
            ),
            fact(
                102,
                Some(10),
                SyntaxFactKind::Signature,
                MEMBER,
                usize::try_from(first_span.start_byte()).unwrap(),
                usize::try_from(first_span.end_byte()).unwrap(),
            ),
            fact(
                103,
                Some(1),
                SyntaxFactKind::Signature,
                SEPARATOR,
                usize::try_from(first_span.end_byte()).unwrap(),
                usize::try_from(second_span.start_byte()).unwrap(),
            ),
            fact(
                104,
                Some(30),
                SyntaxFactKind::Signature,
                MEMBER,
                usize::try_from(second_span.start_byte()).unwrap(),
                group_end,
            ),
        ]);
        result
    }

    fn symbols() -> HashMap<u64, SymbolId> {
        HashMap::from([
            (16, SymbolId::from_bytes([2; 20])),
            (36, SymbolId::from_bytes([3; 20])),
            (15, SymbolId::from_bytes([4; 20])),
            (35, SymbolId::from_bytes([5; 20])),
        ])
    }

    fn site(facts: &[SyntaxFact], id: u64) -> &SyntaxFact {
        facts.iter().find(|fact| fact.local_id() == id).unwrap()
    }

    fn replace(
        facts: &mut [SyntaxFact],
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        label: &str,
        start: usize,
        end: usize,
    ) {
        let current = facts.iter_mut().find(|fact| fact.local_id() == id).unwrap();
        *current = fact(id, parent, kind, label, start, end);
    }

    #[test]
    fn recursive_members_resolve_mutual_calls_and_keep_own_formals() {
        let mut facts = facts(SOURCE);
        for _ in 0..2 {
            let symbols = symbols();
            let plan = FSharpBindings::new(
                &facts,
                SOURCE.as_bytes(),
                &symbols,
                64,
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(
                plan.resolve(site(&facts, 17), "second", &Cancellation::new())
                    .unwrap(),
                symbols.get(&36).copied()
            );
            assert_eq!(
                plan.resolve_call(site(&facts, 18), &Cancellation::new())
                    .unwrap(),
                symbols.get(&36).copied()
            );
            assert_eq!(
                plan.resolve_call(site(&facts, 38), &Cancellation::new())
                    .unwrap(),
                symbols.get(&16).copied()
            );
            assert_eq!(
                plan.resolve(site(&facts, 20), "x", &Cancellation::new())
                    .unwrap(),
                symbols.get(&15).copied()
            );
            facts.reverse();
        }
    }

    #[test]
    fn recursive_proof_loss_and_conflicts_do_not_shrink_membership() {
        for case in 0..14 {
            let mut facts = facts(SOURCE);
            match case {
                0 => facts.retain(|fact| fact.local_id() != 100),
                1 => facts.retain(|fact| fact.local_id() != 101),
                2 => facts.retain(|fact| fact.local_id() != 103),
                3 => facts.retain(|fact| fact.local_id() != 104),
                4..=7 => {
                    let id = [100, 101, 102, 103][case - 4];
                    let original = site(&facts, id);
                    facts.push(fact(
                        200,
                        original.parent(),
                        original.kind(),
                        original.syntax_kind().as_str(),
                        usize::try_from(original.span().start_byte()).unwrap(),
                        usize::try_from(original.span().end_byte()).unwrap(),
                    ));
                }
                8 => {
                    let span = site(&facts, 104).span();
                    replace(
                        &mut facts,
                        104,
                        Some(10),
                        SyntaxFactKind::Signature,
                        MEMBER,
                        usize::try_from(span.start_byte()).unwrap(),
                        usize::try_from(span.end_byte()).unwrap(),
                    );
                }
                9 => {
                    let span = site(&facts, 103).span();
                    replace(
                        &mut facts,
                        103,
                        Some(31),
                        SyntaxFactKind::Signature,
                        SEPARATOR,
                        usize::try_from(span.start_byte()).unwrap(),
                        usize::try_from(span.end_byte()).unwrap(),
                    );
                }
                10 => {
                    let span = site(&facts, 104).span();
                    replace(
                        &mut facts,
                        104,
                        Some(30),
                        SyntaxFactKind::Signature,
                        MEMBER,
                        usize::try_from(span.start_byte()).unwrap() + 1,
                        usize::try_from(span.end_byte()).unwrap(),
                    );
                }
                11 => facts.retain(|fact| fact.local_id() != 36),
                12 => facts.retain(|fact| !matches!(fact.local_id(), 30..=40 | 104)),
                13 => {
                    let span = site(&facts, 104).span();
                    let current = facts
                        .iter_mut()
                        .find(|fact| fact.local_id() == 104)
                        .unwrap();
                    *current = SyntaxFact::new(
                        104,
                        Some(30),
                        SyntaxFactKind::Signature,
                        SourceSpan::new(
                            FileId::from_bytes([8; 20]),
                            span.start_byte(),
                            span.end_byte(),
                        )
                        .unwrap(),
                        0,
                        SyntaxKindLabel::new(MEMBER).unwrap(),
                    );
                }
                _ => unreachable!(),
            }
            for _ in 0..2 {
                let symbols = symbols();
                let plan = FSharpBindings::new(
                    &facts,
                    SOURCE.as_bytes(),
                    &symbols,
                    64,
                    &Cancellation::new(),
                )
                .unwrap();
                assert_eq!(
                    plan.resolve_call(site(&facts, 18), &Cancellation::new())
                        .unwrap(),
                    None,
                    "case {case}"
                );
                assert_eq!(
                    plan.resolve(site(&facts, 20), "x", &Cancellation::new())
                        .unwrap(),
                    symbols.get(&15).copied(),
                    "case {case}"
                );
                facts.reverse();
            }
        }
    }

    #[test]
    fn unavailable_recursive_member_symbols_are_closed_bindings() {
        let facts = facts(SOURCE);
        let mut symbols = symbols();
        symbols.remove(&36);
        let plan = FSharpBindings::new(
            &facts,
            SOURCE.as_bytes(),
            &symbols,
            64,
            &Cancellation::new(),
        )
        .unwrap();
        let recursive = RecursiveBindings::new(&plan, &symbols, &Cancellation::new()).unwrap();
        assert_eq!(recursive.resolve(100, "second"), Some(None));
        assert_eq!(recursive.resolve(100, "absent"), None);
        assert_eq!(
            plan.resolve(site(&facts, 17), "second", &Cancellation::new())
                .unwrap(),
            None
        );
        assert_eq!(
            plan.resolve_call(site(&facts, 38), &Cancellation::new())
                .unwrap(),
            symbols.get(&16).copied()
        );
    }

    #[test]
    fn unsupported_recursive_formals_preserve_exact_head_not_callable_arity() {
        let mut facts = facts(SOURCE);
        facts.retain(|fact| fact.local_id() != 33);
        let symbols = symbols();
        let plan = FSharpBindings::new(
            &facts,
            SOURCE.as_bytes(),
            &symbols,
            64,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(
            plan.resolve(site(&facts, 17), "second", &Cancellation::new())
                .unwrap(),
            symbols.get(&36).copied()
        );
        assert_eq!(
            plan.resolve_call(site(&facts, 18), &Cancellation::new())
                .unwrap(),
            None
        );
        assert_eq!(
            plan.resolve(site(&facts, 37), "first", &Cancellation::new())
                .unwrap(),
            None
        );
    }

    #[test]
    fn recursive_witness_coordinates_and_cancellation_return_typed_errors() {
        let mut facts = facts(SOURCE);
        replace(
            &mut facts,
            100,
            Some(1),
            SyntaxFactKind::Signature,
            GROUP,
            0,
            SOURCE.len() + 1,
        );
        assert!(matches!(
            FSharpBindings::new(
                &facts,
                SOURCE.as_bytes(),
                &symbols(),
                64,
                &Cancellation::new()
            ),
            Err(AdapterError::ProviderFailed { .. })
        ));
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            FSharpBindings::new(
                &self::facts(SOURCE),
                SOURCE.as_bytes(),
                &symbols(),
                64,
                &cancellation
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
