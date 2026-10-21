//! Private semantic coordinates for source-language helpers.
//! Admitted parse output stays physical; only this transient view is projected.
//! Evidence factories map back before records and payload identities are created.

use std::{borrow::Cow, mem::size_of};

use rootlight_adapter_sdk::{AdapterError, IncludedRange, ResourceKind, SinkError, SyntaxFact};
use rootlight_cancel::Cancellation;
use rootlight_ir::{SourceRef, SourceSpan};

use crate::source_view::NativeSourceView;

pub(super) struct SourceCoordinates<'context, 'source> {
    view: &'context NativeSourceView<'source>,
    facts: Cow<'context, [SyntaxFact]>,
    ranges: Cow<'context, [IncludedRange]>,
}

impl<'context, 'source> SourceCoordinates<'context, 'source> {
    pub(super) fn new(
        view: &'context NativeSourceView<'source>,
        facts: &'context [SyntaxFact],
        ranges: &'context [IncludedRange],
        maximum_owned_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        if view.is_identity() {
            return Ok(Self {
                view,
                facts: Cow::Borrowed(facts),
                ranges: Cow::Borrowed(ranges),
            });
        }
        let mut strings = 0;
        for fact in facts {
            cancellation.check()?;
            strings = add(strings, fact.syntax_kind().as_str().len())?;
        }
        for range in ranges {
            cancellation.check()?;
            strings = add(strings, range.language().as_str().len())?;
        }
        let charge = |fact_capacity: usize, range_capacity: usize| {
            add(
                add(
                    multiply(fact_capacity, size_of::<SyntaxFact>())?,
                    multiply(range_capacity, size_of::<IncludedRange>())?,
                )?,
                strings,
            )
        };
        require_owned(
            add(view.owned_bytes(), charge(facts.len(), ranges.len())?)?,
            maximum_owned_bytes,
        )?;
        let mut projected_facts = Vec::new();
        projected_facts
            .try_reserve_exact(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        require_owned(
            add(
                view.owned_bytes(),
                charge(projected_facts.capacity(), ranges.len())?,
            )?,
            maximum_owned_bytes,
        )?;
        let mut projected_ranges = Vec::new();
        projected_ranges
            .try_reserve_exact(ranges.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let owned_bytes = charge(projected_facts.capacity(), projected_ranges.capacity())?;
        require_owned(add(view.owned_bytes(), owned_bytes)?, maximum_owned_bytes)?;
        for fact in facts {
            cancellation.check()?;
            projected_facts.push(SyntaxFact::new(
                fact.local_id(),
                fact.parent(),
                fact.kind(),
                view.projected_span(fact.span())?,
                fact.depth(),
                fact.syntax_kind().clone(),
            ));
        }
        for range in ranges {
            cancellation.check()?;
            projected_ranges.push(IncludedRange::new(
                view.projected_span(range.span())?,
                range.language().clone(),
            ));
        }
        Ok(Self {
            view,
            facts: Cow::Owned(projected_facts),
            ranges: Cow::Owned(projected_ranges),
        })
    }

    pub(super) fn facts(&self) -> &[SyntaxFact] {
        &self.facts
    }
    pub(super) fn ranges(&self) -> &[IncludedRange] {
        &self.ranges
    }

    pub(super) fn physical(&self, span: SourceSpan) -> Result<SourceSpan, AdapterError> {
        self.view.physical_span(span)
    }

    pub(super) fn source(
        &self,
        full: &SourceRef,
        span: SourceSpan,
    ) -> Result<SourceRef, AdapterError> {
        Ok(super::source_for_span(full, self.physical(span)?))
    }

    pub(super) fn groovy_callable(
        &self,
        mut value: rootlight_ir::GroovyCallable,
    ) -> Result<rootlight_ir::GroovyCallable, AdapterError> {
        value.name = self.physical(value.name)?;
        value.parameters = self.physical(value.parameters)?;
        Ok(value)
    }

    pub(super) fn groovy_member(
        &self,
        mut value: rootlight_ir::GroovyMember,
    ) -> Result<rootlight_ir::GroovyMember, AdapterError> {
        value.owner = self.physical(value.owner)?;
        value.declaration = self.physical(value.declaration)?;
        value.name = self.physical(value.name)?;
        Ok(value)
    }

    pub(super) fn groovy_call(
        &self,
        mut value: rootlight_ir::GroovyCallLookup,
    ) -> Result<rootlight_ir::GroovyCallLookup, AdapterError> {
        value.name = self.physical(value.name)?;
        value.enclosing_type = value
            .enclosing_type
            .map(|span| self.physical(span))
            .transpose()?;
        value.enclosing_callable = value
            .enclosing_callable
            .map(|span| self.physical(span))
            .transpose()?;
        Ok(value)
    }

    pub(super) fn groovy_value(
        &self,
        mut value: rootlight_ir::GroovyValueLookup,
    ) -> Result<rootlight_ir::GroovyValueLookup, AdapterError> {
        value.enclosing_type = value
            .enclosing_type
            .map(|span| self.physical(span))
            .transpose()?;
        value.enclosing_callable = value
            .enclosing_callable
            .map(|span| self.physical(span))
            .transpose()?;
        Ok(value)
    }

    pub(super) fn groovy_static_import(
        &self,
        mut value: rootlight_ir::GroovyStaticImport,
    ) -> Result<rootlight_ir::GroovyStaticImport, AdapterError> {
        value.name = self.physical(value.name)?;
        value.alias = value.alias.map(|span| self.physical(span)).transpose()?;
        Ok(value)
    }

    pub(super) fn groovy_binding(
        &self,
        mut value: rootlight_ir::GroovyImportedType,
    ) -> Result<rootlight_ir::GroovyImportedType, AdapterError> {
        value.import = self.physical(value.import)?;
        Ok(value)
    }

    pub(super) fn groovy_type(
        &self,
        mut value: rootlight_ir::GroovyTypeLookup,
    ) -> Result<rootlight_ir::GroovyTypeLookup, AdapterError> {
        value.owner = self.physical(value.owner)?;
        value.fallback_import = value
            .fallback_import
            .map(|binding| self.groovy_binding(binding))
            .transpose()?;
        value.fallback_local = value
            .fallback_local
            .map(|span| self.physical(span))
            .transpose()?;
        Ok(value)
    }

    pub(super) fn vba_compilation(
        &self,
        mut value: rootlight_ir::VbaCompilation,
        cancellation: &Cancellation,
    ) -> Result<rootlight_ir::VbaCompilation, AdapterError> {
        value.scope = self.physical(value.scope)?;
        for branch in &mut value.branches {
            cancellation.check()?;
            branch.group = self.physical(branch.group)?;
            branch.region = self.physical(branch.region)?;
        }
        Ok(value)
    }
}

fn add(left: usize, right: usize) -> Result<usize, AdapterError> {
    left.checked_add(right)
        .ok_or_else(|| SinkError::AccountingOverflow.into())
}

fn multiply(left: usize, right: usize) -> Result<usize, AdapterError> {
    left.checked_mul(right)
        .ok_or_else(|| SinkError::AccountingOverflow.into())
}

fn require_owned(observed: usize, limit: usize) -> Result<(), AdapterError> {
    if observed > limit {
        Err(super::stream_limit(
            ResourceKind::ReportedMemoryBytes,
            observed,
            limit,
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod line_ending_tests {
    use rootlight_adapter_sdk::{LanguageId, SyntaxFactKind, SyntaxKindLabel};
    use rootlight_ids::FileId;

    use crate::source_view::LineEndingPolicy;

    use super::*;

    #[test]
    fn mapped_utf8_coordinates_project_physical_facts_and_ranges_before_helpers() {
        let raw = b"a\r\nb";
        let file = FileId::from_bytes([62; 20]);
        let written = SourceSpan::new(file, 3, 4).unwrap();
        let full = SourceSpan::new(file, 0, 4).unwrap();
        let facts = [SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Declaration,
            written,
            0,
            SyntaxKindLabel::new("identifier").unwrap(),
        )];
        let ranges = [IncludedRange::new(
            full,
            LanguageId::new("fixture").unwrap(),
        )];
        let cancellation = Cancellation::new();
        let view = NativeSourceView::new_with_policy(
            raw,
            "utf-8",
            LineEndingPolicy::CrlfFileText,
            usize::MAX,
            usize::MAX,
            &cancellation,
        )
        .unwrap();
        assert_eq!(view.encoding(), None);
        assert!(!view.is_identity());
        let coordinates =
            SourceCoordinates::new(&view, &facts, &ranges, usize::MAX, &cancellation).unwrap();
        let projected = SourceSpan::new(file, 2, 3).unwrap();
        assert_eq!(coordinates.facts()[0].span(), projected);
        assert_eq!(
            coordinates.ranges()[0].span(),
            SourceSpan::new(file, 0, 3).unwrap()
        );
        assert_eq!(coordinates.physical(projected).unwrap(), written);
        assert!(matches!(coordinates.facts, Cow::Owned(_)));
        assert!(
            SourceCoordinates::new(&view, &facts, &ranges, view.owned_bytes(), &cancellation)
                .is_err()
        );
        let preserve = NativeSourceView::new(raw, "utf-8", usize::MAX, 0, &cancellation).unwrap();
        let coordinates =
            SourceCoordinates::new(&preserve, &facts, &ranges, 0, &cancellation).unwrap();
        assert_eq!(coordinates.facts().as_ptr(), facts.as_ptr());
        assert_eq!(coordinates.ranges().as_ptr(), ranges.as_ptr());
        assert_eq!(coordinates.facts()[0].span(), written);
    }
}
