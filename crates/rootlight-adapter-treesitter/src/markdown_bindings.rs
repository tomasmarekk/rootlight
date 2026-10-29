//! Document-local named Markdown destinations over source-backed captures.
//! Comparison keys follow CommonMark without rewriting occurrence evidence.
//! Native container markers are excluded only while comparing multiline labels.

use std::collections::{BTreeMap, HashMap};

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, DiagnosticCode, SyntaxFact, SyntaxFactKind,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::SourceSpan;
use rootlight_unicode::UnicodeCaseFold as _;

pub(super) const TARGET_UNAVAILABLE: &str = "markdown-reference-target-unavailable";
pub(super) const HOST_INLINE_COMPLETE: &str = "markdown.host_inline_complete";

pub(super) fn host_inline_complete(
    facts: &[SyntaxFact],
    diagnostics: &[AdapterDiagnostic],
    expected: SourceSpan,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut root = None;
    let mut module = None;
    let mut complete = None;
    let mut section = None;
    let mut inlines = Vec::new();
    for diagnostic in diagnostics {
        cancellation.check()?;
        if diagnostic.code().as_str() == "syntax-extraction-limit" {
            return Ok(false);
        }
    }
    for fact in facts {
        cancellation.check()?;
        let label = fact.syntax_kind().as_str();
        let slot = match label {
            "markdown.file.root" => Some((&mut root, SyntaxFactKind::Root)),
            "markdown.file.module" => Some((&mut module, SyntaxFactKind::Module)),
            "markdown.section.declaration" if fact.span() == expected => {
                Some((&mut section, SyntaxFactKind::Declaration))
            }
            "markdown.inline.signature"
            | "markdown.inline_limit.signature"
            | "markdown.inline_error.signature" => return Ok(false),
            _ if label.strip_suffix(".signature") == Some(HOST_INLINE_COMPLETE) => {
                Some((&mut complete, SyntaxFactKind::Signature))
            }
            "markdown.inline_parsed.signature" => {
                if fact.kind() != SyntaxFactKind::Signature
                    || fact.span().file() != expected.file()
                    || fact.span().start_byte() < expected.start_byte()
                    || fact.span().end_byte() > expected.end_byte()
                {
                    return Ok(false);
                }
                inlines.try_reserve(1).map_err(|_| invalid_capture())?;
                inlines.push(fact.span());
                None
            }
            _ => None,
        };
        if let Some((slot, kind)) = slot
            && (fact.kind() != kind || fact.span() != expected || slot.replace(fact).is_some())
        {
            return Ok(false);
        }
    }
    let (Some(root), Some(module), Some(complete)) = (root, module, complete) else {
        return Ok(false);
    };
    if root.local_id() == module.local_id()
        || root.local_id() == complete.local_id()
        || module.local_id() == complete.local_id()
        || root.parent().is_some()
        || root.depth() != 0
        || module.parent() != Some(root.local_id())
        || module.depth() != 1
    {
        return Ok(false);
    }
    // A leading section can own the full document span in ordinary normalization.
    // It may contain this metadata without changing the document's link census.
    let parent = if let Some(section) = section {
        if [root.local_id(), module.local_id(), complete.local_id()].contains(&section.local_id())
            || section.parent() != Some(module.local_id())
            || section.depth() != 2
        {
            return Ok(false);
        }
        section
    } else {
        module
    };
    if complete.parent() != Some(parent.local_id())
        || complete.depth() != parent.depth().saturating_add(1)
    {
        return Ok(false);
    }
    crate::runtime::sort_cancellable_by(&mut inlines, cancellation, |left, right| {
        (left.start_byte(), left.end_byte()).cmp(&(right.start_byte(), right.end_byte()))
    })?;
    for pair in inlines.windows(2) {
        cancellation.check()?;
        if pair[0].end_byte() > pair[1].start_byte() || pair[0] == pair[1] {
            return Ok(false);
        }
    }
    for fact in facts {
        cancellation.check()?;
        if !is_reference(fact) {
            continue;
        }
        if fact.kind() != SyntaxFactKind::Occurrence || fact.span().file() != expected.file() {
            return Ok(false);
        }
        let index = inlines.partition_point(|span| span.start_byte() <= fact.span().start_byte());
        if !index
            .checked_sub(1)
            .and_then(|index| inlines.get(index))
            .is_some_and(|span| fact.span().end_byte() <= span.end_byte())
        {
            return Ok(false);
        }
    }
    Ok(true)
}

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
    fn host_certificate_requires_unique_complete_source_owned_chain_and_unclipped_inlines() {
        let file = FileId::from_bytes([7; 20]);
        let expected = SourceSpan::new(file, 0, 18).unwrap();
        let make = |id, parent, kind, span, depth, label: &str| {
            SyntaxFact::new(
                id,
                parent,
                kind,
                span,
                depth,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        let marker = "markdown.host_inline_complete.signature";
        let baseline = vec![
            make(
                1,
                None,
                SyntaxFactKind::Root,
                expected,
                0,
                "markdown.file.root",
            ),
            make(
                2,
                Some(1),
                SyntaxFactKind::Module,
                expected,
                1,
                "markdown.file.module",
            ),
            make(3, Some(2), SyntaxFactKind::Signature, expected, 2, marker),
            make(
                4,
                Some(2),
                SyntaxFactKind::Signature,
                SourceSpan::new(file, 0, 3).unwrap(),
                2,
                "markdown.inline_parsed.signature",
            ),
            make(
                5,
                Some(2),
                SyntaxFactKind::Occurrence,
                SourceSpan::new(file, 0, 3).unwrap(),
                2,
                "markdown.shortcut_link.reference",
            ),
        ];
        let cancel = Cancellation::new();
        assert!(host_inline_complete(&baseline, &[], expected, &cancel).unwrap());
        let mut headed = baseline.clone();
        headed[2] = make(3, Some(6), SyntaxFactKind::Signature, expected, 3, marker);
        headed.push(make(
            6,
            Some(2),
            SyntaxFactKind::Declaration,
            expected,
            2,
            "markdown.section.declaration",
        ));
        assert!(host_inline_complete(&headed, &[], expected, &cancel).unwrap());
        for mode in 0..6 {
            let mut facts = headed.clone();
            match mode {
                0 => {
                    facts.pop();
                }
                1 => facts.push(make(
                    99,
                    Some(2),
                    SyntaxFactKind::Declaration,
                    expected,
                    2,
                    "markdown.section.declaration",
                )),
                2 => {
                    facts[5] = make(
                        6,
                        Some(1),
                        SyntaxFactKind::Declaration,
                        expected,
                        2,
                        "markdown.section.declaration",
                    )
                }
                3 => {
                    facts[5] = make(
                        6,
                        Some(2),
                        SyntaxFactKind::Declaration,
                        expected,
                        3,
                        "markdown.section.declaration",
                    )
                }
                4 => {
                    facts[5] = make(
                        3,
                        Some(2),
                        SyntaxFactKind::Declaration,
                        expected,
                        2,
                        "markdown.section.declaration",
                    )
                }
                5 => {
                    facts[5] = make(
                        6,
                        Some(2),
                        SyntaxFactKind::Declaration,
                        SourceSpan::new(file, 1, 18).unwrap(),
                        2,
                        "markdown.section.declaration",
                    )
                }
                _ => unreachable!(),
            }
            assert!(
                !host_inline_complete(&facts, &[], expected, &cancel).unwrap(),
                "headed mode{mode}"
            );
        }
        for index in 0..3 {
            for mode in 0..7 {
                let mut facts = baseline.clone();
                let witness = facts[index].clone();
                if mode == 0 {
                    facts.remove(index);
                } else if mode == 1 {
                    facts.push(make(
                        99,
                        witness.parent(),
                        witness.kind(),
                        witness.span(),
                        witness.depth(),
                        witness.syntax_kind().as_str(),
                    ));
                } else {
                    let changed = match mode {
                        2 => SourceSpan::new(file, 1, 18).unwrap(),
                        3 => SourceSpan::new(FileId::from_bytes([8; 20]), 0, 18).unwrap(),
                        _ => witness.span(),
                    };
                    facts[index] = make(
                        witness.local_id(),
                        if mode == 4 {
                            Some(99)
                        } else {
                            witness.parent()
                        },
                        if mode == 5 {
                            SyntaxFactKind::Occurrence
                        } else {
                            witness.kind()
                        },
                        changed,
                        witness.depth() + usize::from(mode == 6),
                        witness.syntax_kind().as_str(),
                    );
                }
                assert!(
                    !host_inline_complete(&facts, &[], expected, &cancel).unwrap(),
                    "witness{index} mode{mode}"
                );
            }
        }
        let mut aliases = baseline.clone();
        for (index, kind, parent, depth, label) in [
            (0, SyntaxFactKind::Root, None, 0, "markdown.file.root"),
            (
                1,
                SyntaxFactKind::Module,
                Some(42),
                1,
                "markdown.file.module",
            ),
            (2, SyntaxFactKind::Signature, Some(42), 2, marker),
        ] {
            aliases[index] = make(42, parent, kind, expected, depth, label);
        }
        assert!(!host_inline_complete(&aliases, &[], expected, &cancel).unwrap());
        for label in [
            "markdown.inline.signature",
            "markdown.inline_limit.signature",
            "markdown.inline_error.signature",
        ] {
            let mut facts = baseline.clone();
            facts.push(make(
                99,
                Some(2),
                SyntaxFactKind::Signature,
                expected,
                2,
                label,
            ));
            assert!(!host_inline_complete(&facts, &[], expected, &cancel).unwrap());
        }
        for duplicate in [false, true] {
            let mut facts = baseline.clone();
            if duplicate {
                facts.push(make(
                    99,
                    Some(2),
                    SyntaxFactKind::Signature,
                    baseline[3].span(),
                    2,
                    "markdown.inline_parsed.signature",
                ));
            } else {
                facts.remove(3);
            }
            assert!(!host_inline_complete(&facts, &[], expected, &cancel).unwrap());
        }
        let diagnostics = [AdapterDiagnostic::new(
            DiagnosticCode::new("syntax-extraction-limit").unwrap(),
            rootlight_ir::DiagnosticSeverity::Warning,
            None,
            rootlight_ir::CoverageStatus::Bounded,
        )];
        assert!(!host_inline_complete(&baseline, &diagnostics, expected, &cancel).unwrap());
        assert!(
            !host_inline_complete(
                &baseline,
                &[],
                SourceSpan::new(file, 0, 19).unwrap(),
                &cancel
            )
            .unwrap()
        );
        let mut embedded = baseline.clone();
        embedded.push(make(
            99,
            Some(2),
            SyntaxFactKind::Signature,
            SourceSpan::new(file, 0, 3).unwrap(),
            2,
            "markdown.inline_embedded.signature",
        ));
        assert!(host_inline_complete(&embedded, &[], expected, &cancel).unwrap());
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            host_inline_complete(&baseline, &[], expected, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
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
