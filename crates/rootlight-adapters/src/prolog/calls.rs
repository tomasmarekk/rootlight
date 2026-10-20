//! Certify a closed own-file subset of ordinary Prolog conjunction goals.
//! Complete sentence admission precedes every certificate; rejected syntax or
//! mutable reader/runtime authority cannot donate an otherwise matching name.

use std::collections::HashMap;

use super::*;

#[derive(Clone, Copy)]
pub(super) struct DirectCall {
    pub(super) owner: usize,
    pub(super) target: usize,
    pub(super) start: usize,
    pub(super) name_end: usize,
    pub(super) end: usize,
}

struct Work<'a> {
    remaining: usize,
    maximum_calls: usize,
    cancel: &'a Cancellation,
}

impl Work<'_> {
    fn step(&mut self) -> Result<(), AdapterError> {
        self.cancel.check()?;
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or_else(|| failure("prolog-call-proof-work-limit"))?;
        Ok(())
    }
}

fn trim(
    mask: &[u8],
    mut start: usize,
    mut end: usize,
    work: &mut Work<'_>,
) -> Result<(usize, usize), AdapterError> {
    while start < end && mask[start].is_ascii_whitespace() {
        work.step()?;
        start += 1;
    }
    while start < end && mask[end - 1].is_ascii_whitespace() {
        work.step()?;
        end -= 1;
    }
    Ok((start, end))
}

fn ordinary(name: &str) -> bool {
    name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !matches!(
            name,
            "call"
                | "once"
                | "catch"
                | "throw"
                | "setup_call_cleanup"
                | "call_cleanup"
                | "forall"
                | "findall"
                | "bagof"
                | "setof"
                | "phrase"
                | "assert"
                | "asserta"
                | "assertz"
                | "retract"
                | "retractall"
                | "abolish"
                | "consult"
                | "reconsult"
                | "load_files"
                | "ensure_loaded"
                | "include"
                | "use_module"
                | "module"
                | "op"
                | "set_prolog_flag"
                | "set_stream"
                | "read"
                | "read_term"
                | "read_clause"
                | "term_expansion"
                | "goal_expansion"
                | "expand_term"
                | "expand_goal"
                | "initialization"
                | "halt"
                | "true"
                | "fail"
                | "not"
                | "is"
        )
}

struct Terms<'a, 'work, 'cancel> {
    bytes: &'a [u8],
    at: usize,
    work: &'work mut Work<'cancel>,
}

impl Terms<'_, '_, '_> {
    fn spaces(&mut self) -> Result<(), AdapterError> {
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.work.step()?;
            self.at += 1;
        }
        Ok(())
    }

    fn sequence(&mut self, end: u8, empty: bool, depth: usize) -> Result<bool, AdapterError> {
        self.spaces()?;
        if empty && self.bytes.get(self.at) == Some(&end) {
            self.at += 1;
            return Ok(true);
        }
        loop {
            if !self.term(depth)? {
                return Ok(false);
            }
            self.spaces()?;
            match self.bytes.get(self.at).copied() {
                Some(byte) if byte == end => {
                    self.at += 1;
                    return Ok(true);
                }
                Some(b',') => self.at += 1,
                _ => return Ok(false),
            }
        }
    }

    fn term(&mut self, depth: usize) -> Result<bool, AdapterError> {
        self.work.step()?;
        if depth >= 128 {
            return Ok(false);
        }
        self.spaces()?;
        let Some(mut first) = self.bytes.get(self.at).copied() else {
            return Ok(false);
        };
        if first == b'[' {
            self.at += 1;
            return self.sequence(b']', true, depth + 1);
        }
        if matches!(first, b'+' | b'-') {
            self.at += 1;
            let Some(next) = self.bytes.get(self.at).copied().filter(u8::is_ascii_digit) else {
                return Ok(false);
            };
            first = next;
        }
        if first.is_ascii_digit() {
            while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
                self.work.step()?;
                self.at += 1;
            }
            return Ok(true);
        }
        if !first.is_ascii_alphabetic() && first != b'_' {
            return Ok(false);
        }
        while self
            .bytes
            .get(self.at)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            self.work.step()?;
            self.at += 1;
        }
        let identifier_end = self.at;
        self.spaces()?;
        if first.is_ascii_lowercase() && self.bytes.get(self.at) == Some(&b'(') {
            if self.at != identifier_end {
                return Ok(false);
            }
            self.at += 1;
            return self.sequence(b')', false, depth + 1);
        }
        Ok(true)
    }
}

fn closed_term(bytes: &[u8], work: &mut Work<'_>) -> Result<bool, AdapterError> {
    let mut terms = Terms { bytes, at: 0, work };
    if !terms.term(0)? {
        return Ok(false);
    }
    terms.spaces()?;
    Ok(terms.at == terms.bytes.len())
}

// A comparison is a non-binding guard only within this closed arithmetic syntax.
// Lowercase operators/functions, quotes, lists and arbitrary balanced remainders
// are deliberately unavailable, rather than silently classified as arithmetic.
fn graphic(byte: u8) -> bool {
    matches!(
        byte,
        b'#' | b'$'
            | b'&'
            | b'*'
            | b'+'
            | b'-'
            | b'.'
            | b'/'
            | b':'
            | b'<'
            | b'='
            | b'>'
            | b'?'
            | b'@'
            | b'^'
            | b'~'
            | b'\\'
    )
}

fn numeric_guard(raw: &[u8], work: &mut Work<'_>) -> Result<bool, AdapterError> {
    let mut index = 0;
    let mut need_operand = true;
    let mut depth = 0usize;
    let mut comparison = false;
    let mut unary_used = false;
    while index < raw.len() {
        work.step()?;
        let byte = raw[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if graphic(byte) {
            let begin = index;
            while raw.get(index).is_some_and(|byte| graphic(*byte)) {
                work.step()?;
                index += 1;
            }
            let token = &raw[begin..index];
            if need_operand {
                if unary_used || !matches!(token, b"+" | b"-") {
                    return Ok(false);
                }
                unary_used = true;
            } else if matches!(token, b"+" | b"-" | b"*" | b"/") {
                need_operand = true;
                unary_used = false;
            } else if depth == 0 && !comparison && matches!(token, b">" | b"<" | b">=" | b"=<") {
                comparison = true;
                need_operand = true;
                unary_used = false;
            } else {
                return Ok(false);
            }
            continue;
        }
        if need_operand {
            if byte == b'(' {
                depth += 1;
                unary_used = false;
                index += 1;
                continue;
            }
            if byte.is_ascii_digit() || byte.is_ascii_uppercase() || byte == b'_' {
                let variable = !byte.is_ascii_digit();
                index += 1;
                while index < raw.len()
                    && if variable {
                        raw[index].is_ascii_alphanumeric() || raw[index] == b'_'
                    } else {
                        raw[index].is_ascii_digit()
                    }
                {
                    work.step()?;
                    index += 1;
                }
                need_operand = false;
                unary_used = false;
                continue;
            }
            return Ok(false);
        }
        if byte == b')' {
            let Some(next) = depth.checked_sub(1) else {
                return Ok(false);
            };
            depth = next;
            index += 1;
        } else {
            return Ok(false);
        }
    }
    Ok(comparison && !need_operand && depth == 0)
}

struct GoalContext<'source, 'census> {
    source: &'source [u8],
    mask: &'source [u8],
    names: &'census HashMap<(&'source str, usize), Option<usize>>,
    groups: &'census [Clause],
}

fn goals(
    context: &GoalContext<'_, '_>,
    start: usize,
    end: usize,
    owner: Option<usize>,
    output: &mut Vec<DirectCall>,
    work: &mut Work<'_>,
) -> Result<bool, AdapterError> {
    let source = context.source;
    let mask = context.mask;
    let names = context.names;
    let groups = context.groups;
    let mut segment = start;
    let (mut parens, mut brackets, mut braces) = (0usize, 0usize, 0usize);
    for index in start..=end {
        work.step()?;
        let boundary =
            index == end || mask[index] == b',' && parens == 0 && brackets == 0 && braces == 0;
        if boundary {
            let (first, last) = trim(mask, segment, index, work)?;
            if first == last {
                return Ok(false);
            }
            let raw =
                std::str::from_utf8(&source[first..last]).map_err(|_| failure("invalid-utf8"))?;
            if !numeric_guard(&mask[first..last], work)? {
                if !closed_term(&mask[first..last], work)? {
                    return Ok(false);
                }
                let Some((name, arity)) = predicate_head(raw) else {
                    return Ok(false);
                };
                work.cancel.check()?;
                if !ordinary(name) {
                    return Ok(false);
                }
                let Some(Some(target)) = names.get(&(name, arity)) else {
                    return Ok(false);
                };
                if let Some(owner) = owner {
                    let name_end = first
                        .checked_add(name.len())
                        .ok_or_else(|| failure("prolog-source-offset-limit"))?;
                    if groups[*target].head_start <= first {
                        if output.len() >= work.maximum_calls {
                            return Err(failure("prolog-syntax-node-limit"));
                        }
                        output
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        output.push(DirectCall {
                            owner,
                            target: *target,
                            start: first,
                            name_end,
                            end: last,
                        });
                    }
                }
            }
            segment = index + 1;
            continue;
        }
        match mask[index] {
            b'(' => parens += 1,
            b'[' => brackets += 1,
            b'{' => braces += 1,
            b')' => {
                if !decrement(&mut parens) {
                    return Ok(false);
                }
            }
            b']' => {
                if !decrement(&mut brackets) {
                    return Ok(false);
                }
            }
            b'}' => {
                if !decrement(&mut braces) {
                    return Ok(false);
                }
            }
            // Control syntax outside a data argument is not an ordinary goal.
            b';' | b':' | b'!' if parens == 0 && brackets == 0 && braces == 0 => return Ok(false),
            _ => {}
        }
    }
    Ok(parens == 0 && brackets == 0 && braces == 0)
}

pub(super) fn certify(
    source: &[u8],
    masked: &MaskedSource,
    groups: &[Clause],
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<Vec<DirectCall>, AdapterError> {
    let mut output = Vec::new();
    if masked.incomplete_start.is_some() {
        return Ok(output);
    }
    let mut work = Work {
        remaining: source
            .len()
            .checked_mul(64)
            .ok_or_else(|| failure("prolog-call-proof-work-limit"))?,
        maximum_calls: maximum.saturating_sub(syntax_fact_count(groups)) / 3,
        cancel: cancellation,
    };
    let mut names = HashMap::new();
    names
        .try_reserve(groups.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for (index, group) in groups.iter().enumerate() {
        work.step()?;
        let name = std::str::from_utf8(&source[group.head_start..group.name_end])
            .map_err(|_| failure("invalid-utf8"))?;
        for _ in name.bytes() {
            work.step()?;
        }
        if group.kind != ClauseKind::Predicate || !ordinary(name) {
            return Ok(output);
        }
        names
            .entry((name, group.arity))
            .and_modify(|target| *target = None)
            .or_insert(Some(index));
    }
    // Any competing noncontiguous group prevents the complete source certificate.
    if names.values().any(Option::is_none) {
        return Ok(output);
    }
    let context = GoalContext {
        source,
        mask: &masked.bytes,
        names: &names,
        groups,
    };
    let mut start = 0;
    let (mut parens, mut brackets, mut braces) = (0usize, 0usize, 0usize);
    let mut group_index = 0usize;
    for (index, &byte) in masked.bytes.iter().enumerate() {
        work.step()?;
        match byte {
            b'(' => parens += 1,
            b'[' => brackets += 1,
            b'{' => braces += 1,
            b')' => {
                if !decrement(&mut parens) {
                    return Ok(Vec::new());
                }
            }
            b']' => {
                if !decrement(&mut brackets) {
                    return Ok(Vec::new());
                }
            }
            b'}' => {
                if !decrement(&mut braces) {
                    return Ok(Vec::new());
                }
            }
            b'.' if parens == 0
                && brackets == 0
                && braces == 0
                && masked
                    .bytes
                    .get(index + 1)
                    .is_none_or(u8::is_ascii_whitespace) =>
            {
                let (first, _) = trim(&masked.bytes, start, index, &mut work)?;
                let accepted = if let Some(clause) =
                    clause_head(source, &masked.bytes, start, index)
                {
                    if clause.has_body
                        && masked
                            .bytes
                            .get(clause.head_end + 2)
                            .is_some_and(|byte| graphic(*byte))
                    {
                        return Ok(Vec::new());
                    }
                    if !closed_term(&masked.bytes[clause.head_start..clause.head_end], &mut work)? {
                        return Ok(Vec::new());
                    }
                    while groups
                        .get(group_index)
                        .is_some_and(|group| group.clause_end <= first)
                    {
                        work.step()?;
                        group_index += 1;
                    }
                    let Some(group) = groups.get(group_index) else {
                        return Ok(Vec::new());
                    };
                    if clause.kind != ClauseKind::Predicate
                        || clause.head_start < group.head_start
                        || clause.clause_end > group.clause_end
                    {
                        return Ok(Vec::new());
                    }
                    !clause.has_body
                        || goals(
                            &context,
                            clause.head_end + 2,
                            index,
                            Some(group_index),
                            &mut output,
                            &mut work,
                        )?
                } else if masked
                    .bytes
                    .get(first..index)
                    .is_some_and(|text| text.starts_with(b"true"))
                {
                    let (separator, _) = trim(&masked.bytes, first + 4, index, &mut work)?;
                    masked
                        .bytes
                        .get(separator..index)
                        .is_some_and(|text| text.starts_with(b":+"))
                        && !masked
                            .bytes
                            .get(separator + 2)
                            .is_some_and(|byte| graphic(*byte))
                        && goals(&context, separator + 2, index, None, &mut output, &mut work)?
                } else {
                    false
                };
                if !accepted {
                    return Ok(Vec::new());
                }
                let base = syntax_fact_count(groups);
                if output
                    .len()
                    .checked_mul(3)
                    .and_then(|count| count.checked_add(base))
                    .is_none_or(|count| count > maximum)
                {
                    return Err(failure("prolog-syntax-node-limit"));
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    let (tail, _) = trim(&masked.bytes, start, source.len(), &mut work)?;
    if tail != source.len() || parens != 0 || brackets != 0 || braces != 0 {
        output.clear();
    }
    cancellation.check()?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_goal_certificates_follow_literal_heads_and_physical_operator_ranges() {
        let source = include_bytes!("../../tests/fixtures/prolog_head/age.pl");
        let parsed = scan_source(source, 16, 128, &Cancellation::new()).unwrap();
        assert_eq!(parsed.groups.len(), 3);
        assert_eq!(parsed.calls.len(), 2);
        assert_eq!((parsed.calls[0].owner, parsed.calls[0].target), (2, 0));
        assert_eq!(
            (
                parsed.calls[0].start,
                parsed.calls[0].name_end,
                parsed.calls[0].end
            ),
            (200, 208, 225)
        );
        assert_eq!(
            (
                parsed.calls[1].start,
                parsed.calls[1].name_end,
                parsed.calls[1].end
            ),
            (232, 247, 274)
        );
        assert!(
            parsed
                .gaps
                .iter()
                .any(|gap| gap.code == "prolog-sentence-not-analyzed")
        );
        assert!(
            parsed
                .gaps
                .iter()
                .any(|gap| gap.code == "prolog-body-semantics-partial")
        );
    }

    #[test]
    fn actual_new_syntax_capacity_and_cancellation_refuse_before_any_parse_output() {
        let source = include_bytes!("../../tests/fixtures/prolog_head/age.pl");
        let cancel = Cancellation::new();
        assert!(scan_source(source, 16, 128, &cancel).is_ok());
        assert!(matches!(scan_source(source, 15, 128, &cancel),
            Err(AdapterError::ProviderFailed { code }) if code.as_str() == "prolog-syntax-node-limit"));
        assert!(cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(matches!(
            scan_source(source, 16, 128, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn late_unproved_sentences_discard_every_previously_selected_goal() {
        let source = b"target(a). caller(X) :- target(X). :- dynamic target/1.\n";
        let parsed = scan_source(source, 64, 128, &Cancellation::new()).unwrap();
        assert_eq!(parsed.groups.len(), 2);
        assert!(parsed.calls.is_empty());
        assert!(
            parsed
                .gaps
                .iter()
                .any(|gap| gap.code == "prolog-sentence-not-analyzed")
        );
    }

    #[test]
    fn arithmetic_census_consumes_work_and_preserves_typed_stop_authority() {
        let cancel = Cancellation::new();
        let mut work = Work {
            remaining: 1,
            maximum_calls: 0,
            cancel: &cancel,
        };
        assert!(
            matches!(numeric_guard(b"LongVariable > OtherVariable", &mut work),
            Err(AdapterError::ProviderFailed { code }) if code.as_str() == "prolog-call-proof-work-limit")
        );
        let mut work = Work {
            remaining: 4096,
            maximum_calls: 0,
            cancel: &cancel,
        };
        assert!(numeric_guard(b"(Year-Birth)/12+(Day-First)/365 > Age", &mut work).unwrap());
        assert!(!numeric_guard(b"Year > callable(Age)", &mut work).unwrap());
        assert!(cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(matches!(
            numeric_guard(b"Year > Age", &mut work),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
