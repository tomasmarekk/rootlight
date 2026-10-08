//! Match-arm bindings are visible only within an authenticated native body.
//! Complete pattern tiling conserves every authored binder; missing witnesses
//! retain the arm as a closed lexical barrier rather than exposing outer names.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;

use super::{
    DEFINITION, UniqueFact, allocation_failed, contains, invalid_capture, source_bytes,
    source_name, unqualified_written_name, whitespace_between,
};

pub(super) const SCOPE: &str = "fsharp.match_branch.scope";
const HEADER: &str = "fsharp.match_branch_header.expression";
const PATTERN: &str = "fsharp.match_branch_pattern.expression";
const BODY: &str = "fsharp.match_branch_body.expression";
const VARIABLE: &str = "fsharp.variable.declaration";

struct Frame<'a> {
    scope: &'a SyntaxFact,
    header: UniqueFact<'a>,
    pattern: UniqueFact<'a>,
    body: UniqueFact<'a>,
    bindings: Vec<Binding<'a>>,
    invalid: bool,
}

struct Binding<'a> {
    declaration: &'a SyntaxFact,
    definition: UniqueFact<'a>,
}

pub(super) struct BoundName<'fact, 'source> {
    pub(super) definition: &'fact SyntaxFact,
    pub(super) name: &'source str,
}

pub(super) struct Proof<'fact, 'source> {
    pub(super) body: &'fact SyntaxFact,
    pub(super) bindings: Vec<BoundName<'fact, 'source>>,
}

pub(super) struct Environment<'fact, 'source> {
    pub(super) scope: &'fact SyntaxFact,
    pub(super) proof: Option<Proof<'fact, 'source>>,
}

/// Retain every native arm, authorizing names only after complete source proof.
///
/// # Errors
/// Returns typed cancellation, allocation and invalid source-coordinate errors.
pub(super) fn environments<'fact, 'source>(
    facts: &HashMap<u64, &'fact SyntaxFact>,
    source: &'source [u8],
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Vec<Environment<'fact, 'source>>, AdapterError> {
    let mut frames = HashMap::<u64, Frame<'_>>::new();
    let mut positions = HashMap::new();
    for fact in facts.values().copied() {
        cancellation.check()?;
        if matches!(fact.syntax_kind().as_str(), SCOPE | HEADER | PATTERN | BODY) {
            source_bytes(fact, source)?;
        }
        if fact.kind() != SyntaxFactKind::Scope || fact.syntax_kind().as_str() != SCOPE {
            continue;
        }
        frames.try_reserve(1).map_err(allocation_failed)?;
        positions.try_reserve(1).map_err(allocation_failed)?;
        let previous = positions.insert((fact.parent(), fact.span()), fact.local_id());
        if let Some(previous) = previous
            && let Some(frame) = frames.get_mut(&previous)
        {
            frame.invalid = true;
        }
        frames.insert(
            fact.local_id(),
            Frame {
                scope: fact,
                header: UniqueFact::default(),
                pattern: UniqueFact::default(),
                body: UniqueFact::default(),
                bindings: Vec::new(),
                invalid: previous.is_some(),
            },
        );
    }
    if frames.is_empty() {
        return Ok(Vec::new());
    }
    let mut owners = HashMap::new();
    for fact in facts.values().copied() {
        cancellation.check()?;
        let Some(frame) = fact.parent().and_then(|parent| frames.get_mut(&parent)) else {
            continue;
        };
        match fact.syntax_kind().as_str() {
            HEADER => frame.header.retain(fact)?,
            PATTERN => frame.pattern.retain(fact)?,
            BODY => frame.body.retain(fact)?,
            VARIABLE if fact.kind() == SyntaxFactKind::Declaration => {
                source_bytes(fact, source)?;
                owners.try_reserve(1).map_err(allocation_failed)?;
                owners.insert(
                    fact.local_id(),
                    (frame.scope.local_id(), frame.bindings.len()),
                );
                frame.bindings.try_reserve(1).map_err(allocation_failed)?;
                frame.bindings.push(Binding {
                    declaration: fact,
                    definition: UniqueFact::default(),
                });
            }
            _ if fact.kind() == SyntaxFactKind::Declaration => frame.invalid = true,
            _ => {}
        }
    }
    for fact in facts.values().copied() {
        cancellation.check()?;
        if !fact.syntax_kind().as_str().ends_with(".definition") {
            continue;
        }
        if let Some(&(scope, index)) = fact.parent().and_then(|parent| owners.get(&parent))
            && let Some(binding) = frames
                .get_mut(&scope)
                .and_then(|frame| frame.bindings.get_mut(index))
        {
            binding.definition.retain(fact)?;
        }
    }
    let mut result = Vec::new();
    result
        .try_reserve_exact(frames.len())
        .map_err(allocation_failed)?;
    for frame in frames.into_values() {
        cancellation.check()?;
        let proof = proof(&frame, source, maximum_name_bytes, cancellation)?;
        result.push(Environment {
            scope: frame.scope,
            proof,
        });
    }
    Ok(result)
}

fn proof<'fact, 'source>(
    frame: &Frame<'fact>,
    source: &'source [u8],
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Option<Proof<'fact, 'source>>, AdapterError> {
    let (Some(header), Some(pattern), Some(body)) =
        (frame.header.one(), frame.pattern.one(), frame.body.one())
    else {
        return Ok(None);
    };
    let scope = frame.scope;
    if frame.invalid
        || [header, pattern, body].into_iter().any(|fact| {
            fact.kind() != SyntaxFactKind::Signature
                || fact.parent() != Some(scope.local_id())
                || !contains(scope, fact)
                || fact.span().start_byte() >= fact.span().end_byte()
        })
        || header.span().start_byte() != scope.span().start_byte()
        || pattern.span().start_byte() != scope.span().start_byte()
        || !contains(header, pattern)
        || header.span().end_byte() > body.span().start_byte()
        || body.span().end_byte() != scope.span().end_byte()
        || !source_bytes(header, source)?.ends_with(b"->")
    {
        return Ok(None);
    }
    let pattern_text = source_bytes(pattern, source)?;
    let body_text = source_bytes(body, source)?;
    // Native finite patterns and named expression bodies exclude surrounding
    // trivia. Widened witnesses must not authenticate bytes outside that node.
    if pattern_text.first().is_none_or(u8::is_ascii_whitespace)
        || pattern_text.last().is_none_or(u8::is_ascii_whitespace)
        || body_text.first().is_none_or(u8::is_ascii_whitespace)
    {
        return Ok(None);
    }
    let before_arrow = header
        .span()
        .end_byte()
        .checked_sub(2)
        .ok_or_else(invalid_capture)?;
    // A guard is not proven absent by a missing optional capture. Only a
    // complete pattern followed by whitespace and the authored arrow is admitted.
    if pattern.span().end_byte() > before_arrow
        || !whitespace_between(
            source,
            pattern.span().end_byte(),
            before_arrow,
            cancellation,
        )?
        || !whitespace_between(
            source,
            header.span().end_byte(),
            body.span().start_byte(),
            cancellation,
        )?
    {
        return Ok(None);
    }
    let mut ordered = Vec::new();
    ordered
        .try_reserve_exact(frame.bindings.len())
        .map_err(allocation_failed)?;
    for binding in &frame.bindings {
        cancellation.check()?;
        ordered.push(binding);
    }
    crate::runtime::sort_cancellable_by(&mut ordered, cancellation, |left, right| {
        left.declaration.span().cmp(&right.declaration.span())
    })?;
    let mut bindings = Vec::new();
    let mut names = HashSet::new();
    bindings
        .try_reserve_exact(frame.bindings.len())
        .map_err(allocation_failed)?;
    names
        .try_reserve(frame.bindings.len())
        .map_err(allocation_failed)?;
    for binding in &ordered {
        cancellation.check()?;
        let declaration = binding.declaration;
        let (Some(definition), Some(name)) = (
            binding.definition.one(),
            source_name(declaration, source, maximum_name_bytes)?,
        ) else {
            return Ok(None);
        };
        if declaration.parent() != Some(scope.local_id())
            || !contains(pattern, declaration)
            || definition.kind() != SyntaxFactKind::Occurrence
            || definition.syntax_kind().as_str() != DEFINITION
            || definition.parent() != Some(declaration.local_id())
            || definition.span() != declaration.span()
            || !name.chars().next().is_some_and(char::is_lowercase)
            || !unqualified_written_name(definition, name, maximum_name_bytes)
            || !names.insert(name)
        {
            return Ok(None);
        }
        bindings.push(BoundName { definition, name });
    }
    if !pattern_bytes(pattern, &ordered, source, cancellation)? {
        return Ok(None);
    }
    Ok(Some(Proof { body, bindings }))
}

fn pattern_bytes(
    pattern: &SyntaxFact,
    bindings: &[&Binding<'_>],
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut position =
        usize::try_from(pattern.span().start_byte()).map_err(|_| invalid_capture())?;
    let end = usize::try_from(pattern.span().end_byte()).map_err(|_| invalid_capture())?;
    source.get(position..end).ok_or_else(invalid_capture)?;
    let mut next = 0usize;
    let mut depth = 0usize;
    let mut expects_value = true;
    // Native binder spans consume every name atomically. The remaining finite
    // syntax is only wildcard, grouping and tuple punctuation; an omitted leaf
    // cannot shrink the pattern or reveal an outer same-spelled declaration.
    while position < end {
        cancellation.check()?;
        if let Some(binding) = bindings.get(next) {
            let span = binding.declaration.span();
            let start = usize::try_from(span.start_byte()).map_err(|_| invalid_capture())?;
            let finish = usize::try_from(span.end_byte()).map_err(|_| invalid_capture())?;
            if start < position {
                return Ok(false);
            }
            if start == position {
                if !expects_value || finish <= position || finish > end {
                    return Ok(false);
                }
                position = finish;
                next = next.checked_add(1).ok_or_else(invalid_capture)?;
                expects_value = false;
                continue;
            }
        }
        let byte = *source.get(position).ok_or_else(invalid_capture)?;
        if byte.is_ascii_whitespace() {
            position = position.checked_add(1).ok_or_else(invalid_capture)?;
            continue;
        }
        match byte {
            b'(' if expects_value => depth = depth.checked_add(1).ok_or_else(invalid_capture)?,
            b'_' if expects_value => expects_value = false,
            b',' if !expects_value => expects_value = true,
            b')' if depth > 0 && !expects_value => depth -= 1,
            _ => return Ok(false),
        }
        position = position.checked_add(1).ok_or_else(invalid_capture)?;
    }
    Ok(depth == 0 && !expects_value && next == bindings.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsharp_bindings::{FSharpBindings, QualifiedRoot, READ};
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{FileId, SymbolId};
    use rootlight_ir::SourceSpan;

    const SOURCE: &str =
        "let outer input = (match input with | input -> input | _ -> input); input";

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

    fn base(source: &str) -> (Vec<SyntaxFact>, HashMap<u64, SymbolId>) {
        let end = source.len();
        let parameter = source.find("input").unwrap();
        let header_end = source.find('=').unwrap();
        let facts = vec![
            fact(1, None, SyntaxFactKind::Scope, "fsharp.file.scope", 0, end),
            fact(
                10,
                Some(1),
                SyntaxFactKind::Declaration,
                "fsharp.function.declaration",
                4,
                end,
            ),
            fact(
                11,
                Some(10),
                SyntaxFactKind::Scope,
                "fsharp.function.scope",
                4,
                end,
            ),
            fact(
                12,
                Some(11),
                SyntaxFactKind::Signature,
                "fsharp.function.signature",
                4,
                header_end,
            ),
            fact(
                13,
                Some(11),
                SyntaxFactKind::Signature,
                "fsharp.simple_parameters.signature",
                parameter,
                parameter + 5,
            ),
            fact(
                14,
                Some(11),
                SyntaxFactKind::Declaration,
                "fsharp.parameter.declaration",
                parameter,
                parameter + 5,
            ),
            fact(
                15,
                Some(14),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                parameter,
                parameter + 5,
            ),
            fact(16, Some(11), SyntaxFactKind::Occurrence, DEFINITION, 4, 9),
            fact(
                17,
                Some(10),
                SyntaxFactKind::Signature,
                "fsharp.nonrecursive_binding.expression",
                4,
                end,
            ),
        ];
        let symbols = [15u64, 16]
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                (
                    id,
                    SymbolId::from_bytes([u8::try_from(index + 1).unwrap(); 20]),
                )
            })
            .collect();
        (facts, symbols)
    }

    fn arm(
        facts: &mut Vec<SyntaxFact>,
        id: u64,
        parent: u64,
        pattern: (usize, usize),
        header_end: usize,
        body: (usize, usize),
    ) {
        facts.extend([
            fact(
                id,
                Some(parent),
                SyntaxFactKind::Scope,
                SCOPE,
                pattern.0,
                body.1,
            ),
            fact(
                id + 1,
                Some(id),
                SyntaxFactKind::Signature,
                HEADER,
                pattern.0,
                header_end,
            ),
            fact(
                id + 2,
                Some(id),
                SyntaxFactKind::Signature,
                PATTERN,
                pattern.0,
                pattern.1,
            ),
            fact(
                id + 3,
                Some(id),
                SyntaxFactKind::Signature,
                BODY,
                body.0,
                body.1,
            ),
        ]);
    }

    fn binder(
        facts: &mut Vec<SyntaxFact>,
        symbols: &mut HashMap<u64, SymbolId>,
        id: u64,
        parent: u64,
        start: usize,
        end: usize,
    ) {
        facts.extend([
            fact(
                id,
                Some(parent),
                SyntaxFactKind::Declaration,
                VARIABLE,
                start,
                end,
            ),
            fact(
                id + 1,
                Some(id),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                start,
                end,
            ),
        ]);
        symbols.insert(
            id + 1,
            SymbolId::from_bytes([u8::try_from(id).unwrap(); 20]),
        );
    }

    fn fixture() -> (Vec<SyntaxFact>, HashMap<u64, SymbolId>) {
        let (mut facts, mut symbols) = base(SOURCE);
        let first = SOURCE.find("input -> input").unwrap();
        let second = SOURCE.find("_ -> input").unwrap();
        arm(
            &mut facts,
            20,
            11,
            (first, first + 5),
            first + 8,
            (first + 9, first + 14),
        );
        binder(&mut facts, &mut symbols, 24, 20, first, first + 5);
        arm(
            &mut facts,
            40,
            11,
            (second, second + 1),
            second + 4,
            (second + 5, second + 10),
        );
        let scrutinee = SOURCE.find("match input").unwrap() + 6;
        facts.extend([
            fact(
                30,
                Some(20),
                SyntaxFactKind::Occurrence,
                READ,
                first + 9,
                first + 14,
            ),
            fact(
                31,
                Some(11),
                SyntaxFactKind::Occurrence,
                READ,
                scrutinee,
                scrutinee + 5,
            ),
            fact(
                32,
                Some(11),
                SyntaxFactKind::Occurrence,
                READ,
                SOURCE.len() - 5,
                SOURCE.len(),
            ),
            fact(
                50,
                Some(40),
                SyntaxFactKind::Occurrence,
                READ,
                second + 5,
                second + 10,
            ),
        ]);
        (facts, symbols)
    }

    fn read(facts: &[SyntaxFact], id: u64) -> &SyntaxFact {
        facts.iter().find(|fact| fact.local_id() == id).unwrap()
    }

    fn replace(facts: &mut [SyntaxFact], id: u64, parent: Option<u64>, start: usize, end: usize) {
        let index = facts.iter().position(|fact| fact.local_id() == id).unwrap();
        let old = &facts[index];
        facts[index] = fact(
            id,
            parent,
            old.kind(),
            old.syntax_kind().as_str(),
            start,
            end,
        );
    }

    fn assert_targets(
        facts: &[SyntaxFact],
        symbols: &HashMap<u64, SymbolId>,
        expected: [Option<u64>; 4],
    ) {
        assert_targets_with_context(facts, symbols, expected, "unmodified ownership");
    }

    fn assert_targets_with_context(
        facts: &[SyntaxFact],
        symbols: &HashMap<u64, SymbolId>,
        expected: [Option<u64>; 4],
        context: &str,
    ) {
        let cancellation = Cancellation::new();
        let plan =
            FSharpBindings::new(facts, SOURCE.as_bytes(), symbols, 128, &cancellation).unwrap();
        for (id, expected) in [30u64, 31, 32, 50].into_iter().zip(expected) {
            assert_eq!(
                plan.resolve(read(facts, id), "input", &cancellation)
                    .unwrap(),
                expected.map(|id| symbols[&id]),
                "{context}: read {id}",
            );
        }
    }

    #[test]
    fn arm_binders_shadow_only_their_body_and_restore_outer_and_sibling_reads() {
        let (mut facts, symbols) = fixture();
        for _ in 0..2 {
            assert_targets(&facts, &symbols, [Some(25), Some(15), Some(15), Some(15)]);
            facts.reverse();
        }
    }

    #[test]
    fn missing_duplicate_and_foreign_arm_witnesses_close_outer_homonyms() {
        for id in [21u64, 22, 23, 24, 25] {
            let (mut facts, symbols) = fixture();
            facts.retain(|fact| fact.local_id() != id);
            assert_targets(&facts, &symbols, [None, Some(15), Some(15), Some(15)]);
        }
        for id in [21u64, 22, 23, 25] {
            let (mut facts, symbols) = fixture();
            let original = read(&facts, id);
            facts.push(fact(
                80,
                original.parent(),
                original.kind(),
                original.syntax_kind().as_str(),
                usize::try_from(original.span().start_byte()).unwrap(),
                usize::try_from(original.span().end_byte()).unwrap(),
            ));
            assert_targets(&facts, &symbols, [None, Some(15), Some(15), Some(15)]);
        }
        for id in [21u64, 22, 23, 25] {
            let (mut facts, symbols) = fixture();
            let original = read(&facts, id);
            let start = usize::try_from(original.span().start_byte()).unwrap();
            let end = usize::try_from(original.span().end_byte()).unwrap();
            replace(&mut facts, id, Some(1), start, end);
            assert_targets(&facts, &symbols, [None, Some(15), Some(15), Some(15)]);
        }
    }

    #[test]
    fn clipped_widened_and_relocated_arm_proofs_are_not_transparent() {
        for (id, start_delta, end_delta) in [
            (21u64, 1isize, 0isize),
            (21, 0, 1),
            (22, 0, -1),
            (22, 0, 1),
            (23, -1, 0),
            (23, 0, -1),
            (24, 0, -1),
            (25, 0, -1),
        ] {
            let (mut facts, symbols) = fixture();
            let original = read(&facts, id);
            let parent = original.parent();
            let start = usize::try_from(original.span().start_byte())
                .unwrap()
                .checked_add_signed(start_delta)
                .unwrap();
            let end = usize::try_from(original.span().end_byte())
                .unwrap()
                .checked_add_signed(end_delta)
                .unwrap();
            replace(&mut facts, id, parent, start, end);
            assert_targets_with_context(
                &facts,
                &symbols,
                [None, Some(15), Some(15), Some(15)],
                &format!("mutated proof {id} start delta {start_delta} end delta {end_delta}"),
            );
        }
        let (mut facts, symbols) = fixture();
        let body = read(&facts, 23);
        let start = usize::try_from(body.span().start_byte()).unwrap();
        let end = usize::try_from(body.span().end_byte()).unwrap();
        replace(&mut facts, 22, Some(20), start, end);
        replace(&mut facts, 24, Some(20), start, end);
        replace(&mut facts, 25, Some(24), start, end);
        assert_targets(&facts, &symbols, [None, Some(15), Some(15), Some(15)]);
    }

    #[test]
    fn unavailable_symbols_and_duplicate_arm_scopes_remain_closed() {
        let (facts, mut symbols) = fixture();
        symbols.remove(&25);
        assert_targets(&facts, &symbols, [None, Some(15), Some(15), Some(15)]);
        let (facts, mut symbols) = fixture();
        symbols.insert(25, symbols[&15]);
        assert_targets(&facts, &symbols, [None; 4]);
        let (mut facts, symbols) = fixture();
        let scope = read(&facts, 20);
        facts.push(fact(
            80,
            scope.parent(),
            SyntaxFactKind::Scope,
            SCOPE,
            usize::try_from(scope.span().start_byte()).unwrap(),
            usize::try_from(scope.span().end_byte()).unwrap(),
        ));
        assert_targets(&facts, &symbols, [None, Some(15), Some(15), Some(15)]);
    }

    fn pattern_fixture(
        pattern: &str,
        names: &[&str],
    ) -> (String, Vec<SyntaxFact>, HashMap<u64, SymbolId>) {
        let source = format!("let outer input = match input with | {pattern} -> input");
        let (mut facts, mut symbols) = base(&source);
        let start = source.find("with | ").unwrap() + 7;
        let end = start + pattern.len();
        let body = end + 4;
        arm(
            &mut facts,
            20,
            11,
            (start, end),
            end + 3,
            (body, source.len()),
        );
        let mut after = 0usize;
        for (index, name) in names.iter().enumerate() {
            let offset = pattern[after..].find(name).unwrap() + after;
            after = offset + name.len();
            binder(
                &mut facts,
                &mut symbols,
                24 + u64::try_from(index).unwrap() * 2,
                20,
                start + offset,
                start + after,
            );
        }
        facts.push(fact(
            50,
            Some(20),
            SyntaxFactKind::Occurrence,
            READ,
            body,
            source.len(),
        ));
        (source, facts, symbols)
    }

    #[test]
    fn complete_plain_grouped_tuple_and_wildcard_patterns_conserve_all_leaves() {
        for (pattern, names) in [
            ("input", vec!["input"]),
            ("_", vec![]),
            ("(input)", vec!["input"]),
            ("(input, _)", vec!["input"]),
            ("(_, (input, other))", vec!["input", "other"]),
            ("(_, _)", vec![]),
            ("input, other", vec!["input", "other"]),
            ("éclair", vec!["éclair"]),
        ] {
            let (source, facts, symbols) = pattern_fixture(pattern, &names);
            let cancellation = Cancellation::new();
            let by_id = facts.iter().map(|fact| (fact.local_id(), fact)).collect();
            let environments = environments(&by_id, source.as_bytes(), 128, &cancellation).unwrap();
            let proof = environments[0]
                .proof
                .as_ref()
                .unwrap_or_else(|| panic!("{pattern}"));
            assert_eq!(
                proof
                    .bindings
                    .iter()
                    .map(|binding| binding.name)
                    .collect::<Vec<_>>(),
                names
            );
            let plan = FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation)
                .unwrap();
            assert_eq!(
                plan.resolve(read(&facts, 50), "input", &cancellation)
                    .unwrap(),
                Some(symbols[&if names.contains(&"input") { 25 } else { 15 }])
            );
        }
        let (source, mut facts, symbols) = pattern_fixture("(input, other)", &["input", "other"]);
        facts.retain(|fact| ![26, 27].contains(&fact.local_id()));
        let cancellation = Cancellation::new();
        let plan =
            FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation).unwrap();
        assert_eq!(
            plan.resolve(read(&facts, 50), "input", &cancellation)
                .unwrap(),
            None
        );
    }

    #[test]
    fn ambiguous_heads_guards_incomplete_patterns_and_extra_binders_stay_closed() {
        for (pattern, names) in [
            ("Input", vec!["Input"]),
            ("``Input``", vec!["``Input``"]),
            ("Some input", vec!["input"]),
            ("input when input > 0", vec!["input"]),
            ("(input,)", vec!["input"]),
            ("input | other", vec!["input", "other"]),
            ("(input, input)", vec!["input", "input"]),
            ("_input", vec!["_input"]),
            ("[]", vec![]),
        ] {
            let (source, facts, symbols) = pattern_fixture(pattern, &names);
            let cancellation = Cancellation::new();
            let plan = FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation)
                .unwrap();
            assert_eq!(
                plan.resolve(read(&facts, 50), "input", &cancellation)
                    .unwrap(),
                None,
                "{pattern}"
            );
        }
        let (source, mut facts, mut symbols) = pattern_fixture("_", &[]);
        let body = source.len() - 5;
        binder(&mut facts, &mut symbols, 24, 20, body, source.len());
        let cancellation = Cancellation::new();
        let plan =
            FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation).unwrap();
        assert_eq!(
            plan.resolve(read(&facts, 50), "input", &cancellation)
                .unwrap(),
            None
        );
    }

    #[test]
    fn malformed_coordinates_forged_reads_and_cancellation_return_safe_outcomes() {
        let (mut facts, symbols) = fixture();
        let body = read(&facts, 23);
        let start = usize::try_from(body.span().start_byte()).unwrap();
        replace(&mut facts, 23, Some(20), start, SOURCE.len() + 1);
        assert!(matches!(
            FSharpBindings::new(
                &facts,
                SOURCE.as_bytes(),
                &symbols,
                128,
                &Cancellation::new()
            ),
            Err(AdapterError::ProviderFailed { .. })
        ));
        let (facts, symbols) = fixture();
        let cancellation = Cancellation::new();
        let plan =
            FSharpBindings::new(&facts, SOURCE.as_bytes(), &symbols, 128, &cancellation).unwrap();
        let forged = fact(
            30,
            Some(20),
            SyntaxFactKind::Occurrence,
            READ,
            SOURCE.len() - 5,
            SOURCE.len(),
        );
        assert_eq!(plan.resolve(&forged, "input", &cancellation).unwrap(), None);
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            plan.resolve(read(&facts, 30), "input", &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            FSharpBindings::new(&facts, SOURCE.as_bytes(), &symbols, 128, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn branch_bindings_block_qualified_module_roots_without_callable_arity() {
        let source = "let outer input = match input with | input -> input.offset";
        let (mut facts, mut symbols) = base(source);
        let pattern = source.find("with | ").unwrap() + 7;
        let body = pattern + 9;
        arm(
            &mut facts,
            20,
            11,
            (pattern, pattern + 5),
            pattern + 8,
            (body, source.len()),
        );
        binder(&mut facts, &mut symbols, 24, 20, pattern, pattern + 5);
        facts.push(fact(
            30,
            Some(20),
            SyntaxFactKind::Occurrence,
            super::super::QUALIFIED_READ,
            body,
            source.len(),
        ));
        let cancellation = Cancellation::new();
        let plan =
            FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation).unwrap();
        assert_eq!(
            plan.qualified_root(read(&facts, 30), "input"),
            QualifiedRoot::Bound
        );
        assert_eq!(plan.local.as_ref().unwrap().arity(symbols[&25]), None);
        facts.retain(|fact| fact.local_id() != 22);
        let plan =
            FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation).unwrap();
        assert_eq!(
            plan.qualified_root(read(&facts, 30), "input"),
            QualifiedRoot::Closed
        );
    }

    #[test]
    fn branch_captures_enter_nested_local_and_lambda_environments_without_invented_calls() {
        let source = "let outer input = match input with | input -> let cached = input in fun extra -> cached + input + extra";
        let (mut facts, mut symbols) = base(source);
        let pattern = source.find("with | ").unwrap() + 7;
        let local = source.find("let cached").unwrap();
        let cached = local + 4;
        let initializer = source.find("= input in").unwrap() + 2;
        let lambda = source.find("fun extra").unwrap();
        let extra = lambda + 4;
        let arrow_end = source.rfind("->").unwrap() + 2;
        let body = arrow_end + 1;
        let end = source.len();
        arm(
            &mut facts,
            20,
            11,
            (pattern, pattern + 5),
            pattern + 8,
            (local, end),
        );
        binder(&mut facts, &mut symbols, 24, 20, pattern, pattern + 5);
        facts.extend([
            fact(
                40,
                Some(20),
                SyntaxFactKind::Scope,
                "fsharp.local_binding.scope",
                local,
                end,
            ),
            fact(
                41,
                Some(40),
                SyntaxFactKind::Signature,
                "fsharp.local_binding.expression",
                local,
                end,
            ),
            fact(
                42,
                Some(40),
                SyntaxFactKind::Signature,
                "fsharp.local_group.expression",
                local,
                initializer + 5,
            ),
            fact(
                43,
                Some(40),
                SyntaxFactKind::Signature,
                "fsharp.local_continuation.expression",
                lambda,
                end,
            ),
            fact(
                50,
                Some(40),
                SyntaxFactKind::Declaration,
                VARIABLE,
                cached,
                initializer + 5,
            ),
            fact(
                51,
                Some(50),
                SyntaxFactKind::Scope,
                "fsharp.variable.scope",
                cached,
                initializer + 5,
            ),
            fact(
                52,
                Some(51),
                SyntaxFactKind::Signature,
                "fsharp.variable.signature",
                cached,
                initializer - 2,
            ),
            fact(
                53,
                Some(50),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                cached,
                cached + 6,
            ),
            fact(
                60,
                Some(40),
                SyntaxFactKind::Scope,
                "fsharp.lambda.scope",
                lambda,
                end,
            ),
            fact(
                61,
                Some(60),
                SyntaxFactKind::Signature,
                "fsharp.lambda_header.expression",
                lambda,
                arrow_end,
            ),
            fact(
                62,
                Some(60),
                SyntaxFactKind::Signature,
                "fsharp.simple_parameters.signature",
                extra,
                extra + 5,
            ),
            fact(
                63,
                Some(60),
                SyntaxFactKind::Signature,
                "fsharp.lambda_body.expression",
                body,
                end,
            ),
            fact(
                64,
                Some(60),
                SyntaxFactKind::Declaration,
                "fsharp.parameter.declaration",
                extra,
                extra + 5,
            ),
            fact(
                65,
                Some(64),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                extra,
                extra + 5,
            ),
            fact(
                66,
                Some(64),
                SyntaxFactKind::Signature,
                "fsharp.parameter_header.expression",
                extra,
                extra + 5,
            ),
            fact(
                80,
                Some(51),
                SyntaxFactKind::Occurrence,
                READ,
                initializer,
                initializer + 5,
            ),
            fact(
                81,
                Some(60),
                SyntaxFactKind::Occurrence,
                READ,
                body,
                body + 6,
            ),
            fact(
                82,
                Some(60),
                SyntaxFactKind::Occurrence,
                READ,
                body + 9,
                body + 14,
            ),
            fact(83, Some(60), SyntaxFactKind::Occurrence, READ, end - 5, end),
        ]);
        symbols.insert(53, SymbolId::from_bytes([53; 20]));
        symbols.insert(65, SymbolId::from_bytes([65; 20]));
        let cancellation = Cancellation::new();
        let plan =
            FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation).unwrap();
        for (id, name, target) in [
            (80u64, "input", 25u64),
            (81, "cached", 53),
            (82, "input", 25),
            (83, "extra", 65),
        ] {
            assert_eq!(
                plan.resolve(read(&facts, id), name, &cancellation).unwrap(),
                Some(symbols[&target]),
                "{name}"
            );
        }
        assert_eq!(plan.local.as_ref().unwrap().arity(symbols[&25]), None);
        assert_eq!(plan.local.as_ref().unwrap().arity(symbols[&53]), None);
        facts.retain(|fact| fact.local_id() != 21);
        let plan =
            FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation).unwrap();
        assert_eq!(
            plan.resolve(read(&facts, 80), "input", &cancellation)
                .unwrap(),
            None
        );
        assert_eq!(
            plan.resolve(read(&facts, 82), "input", &cancellation)
                .unwrap(),
            None
        );
        assert_eq!(
            plan.resolve(read(&facts, 83), "extra", &cancellation)
                .unwrap(),
            Some(symbols[&65])
        );
    }

    #[test]
    fn nested_match_arms_keep_nearest_binders_without_reopening_outer_captures() {
        let source =
            "let outer input = match input with | input -> match input with | input -> input";
        let (mut facts, mut symbols) = base(source);
        let outer = source.find("with | ").unwrap() + 7;
        let inner = source.rfind("with | ").unwrap() + 7;
        let body = outer + 9;
        let end = source.len();
        arm(
            &mut facts,
            20,
            11,
            (outer, outer + 5),
            outer + 8,
            (body, end),
        );
        binder(&mut facts, &mut symbols, 24, 20, outer, outer + 5);
        arm(
            &mut facts,
            40,
            20,
            (inner, inner + 5),
            inner + 8,
            (inner + 9, end),
        );
        binder(&mut facts, &mut symbols, 44, 40, inner, inner + 5);
        facts.extend([
            fact(
                30,
                Some(20),
                SyntaxFactKind::Occurrence,
                READ,
                body + 6,
                body + 11,
            ),
            fact(50, Some(40), SyntaxFactKind::Occurrence, READ, end - 5, end),
        ]);
        let cancellation = Cancellation::new();
        let plan =
            FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation).unwrap();
        assert_eq!(
            plan.resolve(read(&facts, 30), "input", &cancellation)
                .unwrap(),
            Some(symbols[&25])
        );
        assert_eq!(
            plan.resolve(read(&facts, 50), "input", &cancellation)
                .unwrap(),
            Some(symbols[&45])
        );
        facts.retain(|fact| fact.local_id() != 22);
        let plan =
            FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation).unwrap();
        assert_eq!(
            plan.resolve(read(&facts, 30), "input", &cancellation)
                .unwrap(),
            None
        );
        assert_eq!(
            plan.resolve(read(&facts, 50), "input", &cancellation)
                .unwrap(),
            Some(symbols[&45])
        );
    }
}
