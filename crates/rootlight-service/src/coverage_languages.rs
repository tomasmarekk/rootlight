//! Source-range attribution for coverage gaps inside multilingual files.
//! Validated module evidence distinguishes embedded analysis from host syntax.

use std::{cmp::Reverse, collections::BTreeMap};

use rootlight_cancel::Cancellation;
use rootlight_ids::FileId;
use rootlight_ir::{EntityKind, SourceSpan};
use rootlight_storage::GenerationSnapshot;

use super::{FirstSliceError, check_cancellation};

#[derive(Clone, Copy)]
struct LanguageScope<'a> {
    span: SourceSpan,
    language: &'a str,
    maximum_end: u64,
}

/// Attribution of a source selection to its most specific module evidence.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SourceLanguage<'a> {
    Host,
    Embedded(&'a str),
    Ambiguous,
}

/// A per-read range index containing only modules foreign to their host file.
#[derive(Default)]
pub(super) struct SourceLanguageScopes<'a> {
    files: BTreeMap<FileId, Vec<LanguageScope<'a>>>,
}

impl<'a> SourceLanguageScopes<'a> {
    /// Indexes source-backed embedded modules from a validated generation.
    ///
    /// # Errors
    /// Returns cancellation, allocation or invalid file-ownership failures.
    pub(super) fn from_snapshot(
        snapshot: &'a GenerationSnapshot,
        cancellation: &Cancellation,
    ) -> Result<Self, FirstSliceError> {
        let mut index = Self::default();
        for entity in &snapshot.document().entities {
            check_cancellation(cancellation)?;
            if entity.kind != EntityKind::Module {
                continue;
            }
            let Some(source) = &entity.evidence.source else {
                continue;
            };
            let file = snapshot
                .find_file(source.span().file())
                .ok_or(FirstSliceError::CatalogCorrupt)?;
            if entity.language != file.language {
                index.insert(source.span(), &entity.language)?;
            }
        }
        index.finish(cancellation)?;
        Ok(index)
    }

    fn insert(&mut self, span: SourceSpan, language: &'a str) -> Result<(), FirstSliceError> {
        let scopes = self.files.entry(span.file()).or_default();
        scopes.try_reserve(1).map_err(|_| FirstSliceError::Limits)?;
        scopes.push(LanguageScope {
            span,
            language,
            maximum_end: 0,
        });
        Ok(())
    }

    fn finish(&mut self, cancellation: &Cancellation) -> Result<(), FirstSliceError> {
        for scopes in self.files.values_mut() {
            check_cancellation(cancellation)?;
            scopes.sort_unstable_by_key(|scope| {
                (
                    scope.span.start_byte(),
                    Reverse(scope.span.end_byte()),
                    scope.language,
                )
            });
            let mut maximum_end = 0;
            for scope in scopes {
                check_cancellation(cancellation)?;
                maximum_end = maximum_end.max(scope.span.end_byte());
                scope.maximum_end = maximum_end;
            }
        }
        Ok(())
    }

    /// Selects a containing embedded scope without conflating neighboring blocks.
    ///
    /// Equal ranges with conflicting languages remain repository-wide uncertainty.
    /// # Errors
    /// Returns cancellation while checking overlapping scope evidence.
    pub(super) fn resolve(
        &self,
        span: SourceSpan,
        cancellation: &Cancellation,
    ) -> Result<SourceLanguage<'a>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let Some(scopes) = self.files.get(&span.file()) else {
            return Ok(SourceLanguage::Host);
        };
        let mut position =
            scopes.partition_point(|scope| scope.span.start_byte() <= span.start_byte());
        let mut selected: Option<LanguageScope<'_>> = None;
        while let Some(index) = position.checked_sub(1) {
            check_cancellation(cancellation)?;
            let scope = scopes.get(index).ok_or(FirstSliceError::CatalogCorrupt)?;
            position = index;
            // Prefix maxima stop a miss before walking unrelated earlier examples.
            if scope.maximum_end < span.end_byte() {
                break;
            }
            if let Some(selected) = selected {
                if scope.span != selected.span {
                    break;
                }
                if scope.language != selected.language {
                    return Ok(SourceLanguage::Ambiguous);
                }
            } else if scope.span.end_byte() >= span.end_byte() {
                selected = Some(*scope);
            }
        }
        Ok(selected.map_or(SourceLanguage::Host, |scope| {
            SourceLanguage::Embedded(scope.language)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_ranges_select_nested_languages_without_crossing_siblings() {
        let file = FileId::from_bytes([1; 20]);
        let span = |start, end| SourceSpan::new(file, start, end).unwrap();
        let cancellation = Cancellation::new();
        let mut scopes = SourceLanguageScopes::default();
        for (start, end, language) in [
            (10, 90, "html"),
            (20, 40, "javascript"),
            (50, 70, "css"),
            (100, 120, "objective-c"),
        ] {
            scopes.insert(span(start, end), language).unwrap();
        }
        scopes.finish(&cancellation).unwrap();
        for (start, end, expected) in [
            (0, 10, SourceLanguage::Host),
            (10, 90, SourceLanguage::Embedded("html")),
            (25, 35, SourceLanguage::Embedded("javascript")),
            (35, 55, SourceLanguage::Embedded("html")),
            (55, 65, SourceLanguage::Embedded("css")),
            (110, 115, SourceLanguage::Embedded("objective-c")),
            (80, 110, SourceLanguage::Host),
            (125, 130, SourceLanguage::Host),
        ] {
            assert_eq!(
                scopes.resolve(span(start, end), &cancellation).unwrap(),
                expected
            );
        }
        assert_eq!(
            scopes
                .resolve(
                    SourceSpan::new(FileId::from_bytes([2; 20]), 25, 35).unwrap(),
                    &cancellation
                )
                .unwrap(),
            SourceLanguage::Host
        );
    }

    #[test]
    fn conflicting_ranges_and_cancelled_reads_do_not_invent_a_language() {
        let span = SourceSpan::new(FileId::from_bytes([1; 20]), 10, 20).unwrap();
        let cancellation = Cancellation::new();
        let mut scopes = SourceLanguageScopes::default();
        scopes.insert(span, "html").unwrap();
        scopes.insert(span, "javascript").unwrap();
        scopes.finish(&cancellation).unwrap();
        assert_eq!(
            scopes.resolve(span, &cancellation).unwrap(),
            SourceLanguage::Ambiguous
        );
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            scopes.resolve(span, &cancellation),
            Err(FirstSliceError::Cancelled(_))
        ));
    }
}
