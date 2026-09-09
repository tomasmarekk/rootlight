//! Document-local named Markdown destinations over source-backed captures.
//! Comparison keys follow CommonMark without rewriting occurrence evidence.
//! Native container markers are excluded only while comparing multiline labels.

use std::collections::{BTreeMap, HashMap};

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SyntaxFact};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::SourceSpan;
use unicode_casefold::UnicodeCaseFold as _;

pub(super) const TARGET_UNAVAILABLE: &str = "markdown-reference-target-unavailable";

pub(super) fn is_reference(fact: &SyntaxFact) -> bool {
    matches!(
        fact.syntax_kind().as_str(),
        "markdown.reference_label.reference"
            | "markdown.shortcut_link.reference"
            | "markdown.collapsed_link.reference"
            | "markdown.shortcut_image.reference"
            | "markdown.collapsed_image.reference"
    )
}

pub(super) struct MarkdownBindings {
    continuations: Vec<SourceSpan>,
    definitions: BTreeMap<String, (u64, Option<SymbolId>)>,
    maximum_name_bytes: usize,
}

impl MarkdownBindings {
    pub(super) fn new(
        facts: &[SyntaxFact],
        source: &[u8],
        symbols: &HashMap<u64, SymbolId>,
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let mut continuations = Vec::new();
        for fact in facts {
            cancellation.check()?;
            if fact.syntax_kind().as_str() == "markdown.continuation.signature" {
                continuations
                    .try_reserve(1)
                    .map_err(|_| invalid_capture())?;
                continuations.push(fact.span());
            }
        }
        crate::runtime::sort_cancellable_by(&mut continuations, cancellation, |a, b| {
            (a.start_byte(), a.end_byte()).cmp(&(b.start_byte(), b.end_byte()))
        })?;
        for pair in continuations.windows(2) {
            cancellation.check()?;
            if pair[0].end_byte() > pair[1].start_byte() {
                return Err(invalid_capture());
            }
        }
        let mut result = Self {
            continuations,
            definitions: BTreeMap::new(),
            maximum_name_bytes,
        };
        for fact in facts {
            cancellation.check()?;
            if fact.syntax_kind().as_str() != "markdown.link_label.definition" {
                continue;
            }
            let Some(key) = result.key(fact, source, cancellation)? else {
                continue;
            };
            let binding = (
                fact.span().start_byte(),
                symbols.get(&fact.local_id()).copied(),
            );
            // Even an unavailable first identity shadows later definitions.
            result
                .definitions
                .entry(key)
                .and_modify(|previous| {
                    if binding.0 < previous.0 {
                        *previous = binding;
                    } else if binding.0 == previous.0 && binding.1 != previous.1 {
                        previous.1 = None;
                    }
                })
                .or_insert(binding);
        }
        Ok(result)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        if !is_reference(fact) {
            return Ok(None);
        }
        Ok(self
            .key(fact, source, cancellation)?
            .and_then(|key| self.definitions.get(&key).and_then(|(_, symbol)| *symbol)))
    }

    fn key(
        &self,
        fact: &SyntaxFact,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<String>, AdapterError> {
        let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
        let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
        let raw = std::str::from_utf8(source.get(start..end).ok_or_else(invalid_capture)?)
            .map_err(|_| invalid_capture())?;
        let mut label = raw;
        if matches!(
            fact.syntax_kind().as_str(),
            "markdown.shortcut_image.reference" | "markdown.collapsed_image.reference"
        ) {
            label = label.strip_prefix('!').ok_or_else(invalid_capture)?;
        }
        if matches!(
            fact.syntax_kind().as_str(),
            "markdown.collapsed_link.reference" | "markdown.collapsed_image.reference"
        ) {
            label = label.strip_suffix("[]").ok_or_else(invalid_capture)?;
        }
        let Some(inner) = label.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
            return Ok(None);
        };
        let prefix = usize::from(raw.starts_with('!'));
        let inner_start = start.checked_add(prefix + 1).ok_or_else(invalid_capture)?;
        let first = self
            .continuations
            .partition_point(|span| span.end_byte() <= fact.span().start_byte());
        let mut exclusions = self
            .continuations
            .get(first..)
            .ok_or_else(invalid_capture)?
            .iter()
            .peekable();
        let mut escaped = false;
        let mut count = 0usize;
        let mut whitespace = false;
        let mut key = String::new();
        for (offset, ch) in inner.char_indices() {
            cancellation.check()?;
            let absolute = u64::try_from(
                inner_start
                    .checked_add(offset)
                    .ok_or_else(invalid_capture)?,
            )
            .map_err(|_| invalid_capture())?;
            while exclusions
                .peek()
                .is_some_and(|span| span.end_byte() <= absolute)
            {
                exclusions.next();
            }
            if exclusions
                .peek()
                .is_some_and(|span| span.start_byte() <= absolute && absolute < span.end_byte())
            {
                continue;
            }
            count += 1;
            if count > 999 || (!escaped && matches!(ch, '[' | ']')) {
                return Ok(None);
            }
            escaped = ch == '\\' && !escaped;
            if matches!(ch, ' ' | '\t' | '\r' | '\n') {
                whitespace = !key.is_empty();
                continue;
            }
            for folded in std::iter::once(ch).case_fold() {
                let needed = folded.len_utf8() + usize::from(whitespace);
                if key.len().saturating_add(needed) > self.maximum_name_bytes {
                    return Ok(None);
                }
                key.try_reserve(needed).map_err(|_| invalid_capture())?;
                if whitespace {
                    key.push(' ');
                    whitespace = false;
                }
                key.push(folded);
            }
        }
        Ok((!key.is_empty() && !escaped).then_some(key))
    }
}

fn invalid_capture() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("markdown-binding-capture").expect("static diagnostic is valid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::{SyntaxFactKind, SyntaxKindLabel};
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;

    fn fact(id: u64, start: u64, end: u64, syntax: &str) -> SyntaxFact {
        SyntaxFact::new(
            id,
            None,
            SyntaxFactKind::Occurrence,
            SourceSpan::new(FileId::from_bytes([7; 20]), start, end).unwrap(),
            0,
            SyntaxKindLabel::new(syntax).unwrap(),
        )
    }

    #[test]
    fn label_keys_keep_escapes_and_enforce_syntax_and_output_bounds() {
        let plan = MarkdownBindings {
            continuations: Vec::new(),
            definitions: BTreeMap::new(),
            maximum_name_bytes: 1024,
        };
        for (raw, expected) in [
            ("[  Straße\tNAME\r\n]", Some("strasse name")),
            ("[a\\[b]", Some("a\\[b")),
            ("[a[b]", None),
            ("[a\\]", None),
            ("[ \r\n\t]", None),
            ("[a&amp;b]", Some("a&amp;b")),
            ("[É]", Some("é")),
            ("[E\u{301}]", Some("e\u{301}")),
        ] {
            let capture = fact(
                1,
                0,
                u64::try_from(raw.len()).unwrap(),
                "markdown.reference_label.reference",
            );
            assert_eq!(
                plan.key(&capture, raw.as_bytes(), &Cancellation::new())
                    .unwrap()
                    .as_deref(),
                expected,
                "{raw}"
            );
        }
        for (count, valid) in [(999, true), (1000, false)] {
            let raw = format!("[{}]", "x".repeat(count));
            let capture = fact(
                1,
                0,
                u64::try_from(raw.len()).unwrap(),
                "markdown.reference_label.reference",
            );
            assert_eq!(
                plan.key(&capture, raw.as_bytes(), &Cancellation::new())
                    .unwrap()
                    .is_some(),
                valid
            );
        }
        let raw = "[ß]";
        let capture = fact(
            1,
            0,
            u64::try_from(raw.len()).unwrap(),
            "markdown.reference_label.reference",
        );
        let narrow = MarkdownBindings {
            maximum_name_bytes: 1,
            ..plan
        };
        assert!(
            narrow
                .key(&capture, raw.as_bytes(), &Cancellation::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn missing_first_identity_shadows_later_definitions_independent_of_fact_order() {
        let source = b"[x] [X] [x]";
        let later = SymbolId::from_bytes([8; 20]);
        let definitions = [
            fact(2, 4, 7, "markdown.link_label.definition"),
            fact(1, 0, 3, "markdown.link_label.definition"),
        ];
        let symbols = HashMap::from([(2, later)]);
        let plan = MarkdownBindings::new(&definitions, source, &symbols, 64, &Cancellation::new())
            .unwrap();
        let reference = fact(3, 8, 11, "markdown.shortcut_link.reference");
        assert_eq!(
            plan.resolve(&reference, source, &Cancellation::new())
                .unwrap(),
            None
        );
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            plan.resolve(&reference, source, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            MarkdownBindings::new(&[], b"", &HashMap::new(), 64, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
