//! Recheck written ENV keys against physical instruction boundaries before naming.
//! Values are scanned only for token boundaries; expansion and execution stay unknown.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ir::{SourceRef, SourceSpan};

#[derive(Default)]
pub(super) struct Definitions {
    admitted: HashSet<u64>,
}

struct Pair {
    instruction: Range<usize>,
    declaration: Range<usize>,
    name: Range<usize>,
}

type EnvSourceCensus = (Vec<Pair>, Vec<Range<usize>>);

fn space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t')
}

fn first(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn rest(byte: u8) -> bool {
    first(byte) || byte.is_ascii_digit()
}

fn pairs(
    source: &[u8],
    maximum: usize,
    cancel: &Cancellation,
) -> Result<Option<EnvSourceCensus>, AdapterError> {
    let mut result = Vec::new();
    let mut from = Vec::new();
    let mut at = 0;
    let mut continued = false;
    while at < source.len() {
        cancel.check()?;
        let start = at;
        while at < source.len() && source[at] != b'\n' {
            if at.is_multiple_of(64) {
                cancel.check()?;
            }
            if source[at] == 0 || source.get(at..at + 2) == Some(b"<<") {
                return Ok(None);
            }
            at += 1;
        }
        let mut end = at;
        at += usize::from(at < source.len());
        if end > start && source[end - 1] == b'\r' {
            end -= 1;
        }
        while end > start && space(source[end - 1]) {
            cancel.check()?;
            end -= 1;
        }
        let mut word = if start == 0 && source.starts_with(b"\xef\xbb\xbf") {
            3
        } else {
            start
        };
        while word < end && space(source[word]) {
            cancel.check()?;
            word += 1;
        }
        if source.get(word) == Some(&b'#') {
            let mut directive = word + 1;
            while directive < end && space(source[directive]) {
                cancel.check()?;
                directive += 1;
            }
            if source
                .get(directive..directive.saturating_add(6))
                .is_some_and(|bytes| bytes.eq_ignore_ascii_case(b"escape"))
            {
                return Ok(None);
            }
            continue;
        }
        if word == end {
            continue;
        }
        let next_continued = source[end - 1] == b'\\';
        let blocked = continued || next_continued;
        continued = next_continued;
        if blocked {
            continue;
        }
        let mut body = word;
        while body < end && !space(source[body]) {
            if body.is_multiple_of(64) {
                cancel.check()?;
            }
            body += 1;
        }
        let keyword = &source[word..body];
        if keyword.eq_ignore_ascii_case(b"FROM") {
            if from.len() == maximum {
                return Ok(None);
            }
            from.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            from.push(word..end);
        }
        if !keyword.eq_ignore_ascii_case(b"ENV") || body == end {
            continue;
        }
        let checkpoint = result.len();
        while body < end {
            while body < end && space(source[body]) {
                cancel.check()?;
                body += 1;
            }
            if body == end {
                break;
            }
            let key = body;
            if !first(source[body]) {
                result.truncate(checkpoint);
                break;
            }
            body += 1;
            while body < end && rest(source[body]) {
                cancel.check()?;
                body += 1;
            }
            let name_end = body;
            if source.get(body) != Some(&b'=') {
                result.truncate(checkpoint);
                break;
            }
            body += 1;
            let mut quote = None;
            while body < end {
                cancel.check()?;
                let byte = source[body];
                if byte == b'\\' {
                    if body + 1 == end {
                        result.truncate(checkpoint);
                        return Ok(None);
                    }
                    body += 2;
                } else if quote == Some(byte) {
                    quote = None;
                    body += 1;
                } else if quote.is_none() && matches!(byte, b'\'' | b'"') {
                    quote = Some(byte);
                    body += 1;
                } else if quote.is_none() && space(byte) {
                    break;
                } else {
                    body += 1;
                }
            }
            if quote.is_some() || result.len() == maximum {
                return Ok(None);
            }
            result
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.push(Pair {
                instruction: word..end,
                declaration: key..body,
                name: key..name_end,
            });
        }
    }
    Ok(Some((result, from)))
}

impl Definitions {
    pub(super) fn new(
        facts: &[SyntaxFact],
        source: &SourceRef,
        bytes: &[u8],
        maximum: usize,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let Some((pairs, from)) = pairs(bytes, maximum, cancel)? else {
            return Ok(Self::default());
        };
        let mut ids = HashMap::new();
        let mut spans = HashMap::<SourceSpan, Vec<&SyntaxFact>>::new();
        ids.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        spans
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut root = None;
        let mut roots = 0;
        let mut recovered = false;
        for fact in facts {
            cancel.check()?;
            recovered |= fact.syntax_kind().as_str() == "dockerfile.recovered_region.signature";
            ids.entry(fact.local_id())
                .and_modify(|entry| *entry = None)
                .or_insert(Some(fact));
            let rows = spans.entry(fact.span()).or_default();
            rows.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            rows.push(fact);
            if fact.kind() == SyntaxFactKind::Root {
                roots += 1;
                if fact.syntax_kind().as_str() == "dockerfile.source_file.root"
                    && fact.parent().is_none()
                    && fact.span().start_byte() == 0
                    && fact.span().end_byte() == bytes.len() as u64
                    && physical(fact.span())? == source.span()
                {
                    root = Some(fact);
                }
            }
        }
        let Some(root) = root.filter(|root| {
            !recovered && roots == 1 && ids.get(&root.local_id()) == Some(&Some(*root))
        }) else {
            return Ok(Self::default());
        };
        let span = |range: &Range<usize>| {
            SourceSpan::new(source.span().file(), range.start as u64, range.end as u64)
                .map_err(|_| SinkError::InvalidIr)
        };
        let unique = |at: SourceSpan, kind, label: &str| {
            let rows = spans.get(&at)?;
            if rows.len() > 2 {
                return None;
            }
            let mut selected = rows
                .iter()
                .filter(|fact| fact.kind() == kind && fact.syntax_kind().as_str() == label);
            let fact = *selected.next()?;
            (selected.next().is_none() && ids.get(&fact.local_id()) == Some(&Some(fact)))
                .then_some(fact)
        };
        let mut admitted = HashSet::new();
        admitted
            .try_reserve(pairs.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for pair in pairs {
            cancel.check()?;
            let declaration_span = span(&pair.declaration)?;
            let definition_span = span(&pair.name)?;
            let instruction_span = span(&pair.instruction)?;
            let Some(declaration) = unique(
                declaration_span,
                SyntaxFactKind::Declaration,
                "dockerfile.env.declaration",
            ) else {
                continue;
            };
            let Some(definition) = unique(
                definition_span,
                SyntaxFactKind::Occurrence,
                "dockerfile.env_name.definition",
            ) else {
                continue;
            };
            let Some(marker) = unique(
                declaration_span,
                SyntaxFactKind::Signature,
                "dockerfile.env_pair_context.signature",
            ) else {
                continue;
            };
            let Some(instruction) = unique(
                instruction_span,
                SyntaxFactKind::Signature,
                "dockerfile.env_instruction.signature",
            ) else {
                continue;
            };
            if spans.get(&declaration_span).map(Vec::len) != Some(2)
                || spans.get(&definition_span).map(Vec::len) != Some(1)
                || spans.get(&instruction_span).map(Vec::len) != Some(1)
                || definition.parent() != Some(declaration.local_id())
                || marker.parent() != Some(declaration.local_id())
                || declaration.parent() != instruction.parent()
            {
                continue;
            }
            let Some(parent) = declaration
                .parent()
                .and_then(|id| ids.get(&id))
                .copied()
                .flatten()
            else {
                continue;
            };
            let mut low = 0;
            let mut high = from.len();
            while low < high {
                cancel.check()?;
                let middle = low + (high - low) / 2;
                if from
                    .get(middle)
                    .is_some_and(|header| header.start < pair.instruction.start)
                {
                    low = middle + 1;
                } else {
                    high = middle;
                }
            }
            let previous = low.checked_sub(1).and_then(|index| from.get(index));
            let next = from.get(low).map_or(bytes.len(), |header| header.start);
            let header_for = |header: &Range<usize>| {
                let at = span(header).ok()?;
                (spans.get(&at)?.len() == 1)
                    .then(|| {
                        unique(
                            at,
                            SyntaxFactKind::Signature,
                            "dockerfile.stage_header.signature",
                        )
                    })
                    .flatten()
            };
            let owner = if parent.local_id() == root.local_id() {
                previous.is_some_and(|header| {
                    header_for(header)
                        .is_some_and(|header| header.parent() == Some(root.local_id()))
                })
            } else if parent.kind() == SyntaxFactKind::Declaration
                && parent.syntax_kind().as_str() == "dockerfile.stage.declaration"
                && parent.parent() == Some(root.local_id())
                && parent.span().end_byte() == next as u64
                && spans.get(&parent.span()).is_some_and(|rows| {
                    let expected = 1 + usize::from(parent.span() == root.span());
                    rows.len() == expected
                        && rows.iter().all(|fact| {
                            fact.local_id() == parent.local_id()
                                || fact.local_id() == root.local_id()
                        })
                })
            {
                previous.is_some_and(|header| {
                    parent.span().start_byte() == header.start as u64
                        && header_for(header)
                            .is_some_and(|header| header.parent() == Some(parent.local_id()))
                })
            } else {
                false
            };
            if owner
                && Some(definition.depth()) == declaration.depth().checked_add(1)
                && Some(marker.depth()) == declaration.depth().checked_add(1)
                && Some(instruction.depth()) == parent.depth().checked_add(1)
                && Some(declaration.depth()) == parent.depth().checked_add(1)
            {
                admitted.insert(declaration.local_id());
            }
        }
        Ok(Self { admitted })
    }

    pub(super) fn contains(&self, fact: &SyntaxFact) -> bool {
        self.admitted.contains(&fact.local_id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_census_masks_values_and_continued_shell_lines_without_evaluating_them() {
        let source=b"FROM scratch\nENV Alpha=\"quoted hidden=no\" Beta=one\\ two\nRUN echo \\\n\n# continued command\nENV hidden=unproved\nENV actual=$VALUE\n";
        let (pairs, headers) = pairs(source, 64, &Cancellation::new()).unwrap().unwrap();
        assert_eq!(headers, vec![0..12]);
        assert_eq!(
            pairs
                .iter()
                .map(|pair| &source[pair.name.clone()])
                .collect::<Vec<_>>(),
            vec![
                b"Alpha".as_slice(),
                b"Beta".as_slice(),
                b"actual".as_slice()
            ]
        );
        assert_eq!(&source[pairs[1].declaration.clone()], b"Beta=one\\ two");
        assert_eq!(&source[pairs[2].declaration.clone()], b"actual=$VALUE");
    }

    #[test]
    fn source_census_keeps_legacy_heredoc_custom_escape_capacity_and_cancel_unproved() {
        let source = b"FROM scratch\nENV Alpha=one Beta=two\nENV legacy old\n";
        assert_eq!(
            pairs(source, 64, &Cancellation::new())
                .unwrap()
                .unwrap()
                .0
                .len(),
            2
        );
        assert!(pairs(source, 1, &Cancellation::new()).unwrap().is_none());
        for source in [
            b"FROM scratch\nRUN <<END\nENV hidden=not_a_directive\nEND\n".as_slice(),
            b"# escape=`\nFROM scratch\nENV Alpha=one\n".as_slice(),
            b"FROM scratch\nENV Alpha=bad\0value\n".as_slice(),
        ] {
            assert!(pairs(source, 64, &Cancellation::new()).unwrap().is_none());
        }
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            pairs(source, 64, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
