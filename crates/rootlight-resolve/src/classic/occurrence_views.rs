//! Ordered input-position views for repeated classic occurrence passes.
//! Classify during retention so negative category tests do not repeatedly scan
//! every record. Positions preserve ambiguity witnesses, including duplicates.

use super::{is_invoked_member, is_member};
use crate::{ResolutionError, engine::ResolutionWorkBudget};
use rootlight_cancel::Cancellation;
use rootlight_ir::{OccurrenceRecord, OccurrenceRole, OccurrenceTarget};

#[cfg(test)]
mod tests;

const VIEW_COUNT: usize = 8;

#[derive(Debug, Clone, Copy)]
pub(super) enum OccurrenceView {
    Pointers,
    TypeLookup,
    TypeAssociation,
    ConstructedTypes,
    Constructions,
    Calls,
    Members,
    MemberApplications,
}

impl OccurrenceView {
    const ALL: [Self; VIEW_COUNT] = [
        Self::Pointers,
        Self::TypeLookup,
        Self::TypeAssociation,
        Self::ConstructedTypes,
        Self::Constructions,
        Self::Calls,
        Self::Members,
        Self::MemberApplications,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Pointers => 0,
            Self::TypeLookup => 1,
            Self::TypeAssociation => 2,
            Self::ConstructedTypes => 3,
            Self::Constructions => 4,
            Self::Calls => 5,
            Self::Members => 6,
            Self::MemberApplications => 7,
        }
    }

    fn includes(self, site: &OccurrenceRecord) -> bool {
        let unresolved = matches!(site.target, OccurrenceTarget::Unresolved { .. });
        let definition = site.role == OccurrenceRole::Definition
            && matches!(site.target, OccurrenceTarget::Resolved { .. });
        match self {
            Self::Pointers => {
                site.role == OccurrenceRole::Reference
                    && unresolved
                    && matches!(
                        site.syntax_kind.as_str(),
                        "vba.procedure_pointer.reference"
                            | "vba.qualified_procedure_pointer.reference"
                    )
            }
            Self::TypeLookup => {
                (site.role == OccurrenceRole::TypeUse
                    || (site.role == OccurrenceRole::ImplementationUse
                        && site.syntax_kind == "vba.implemented_type.reference"))
                    && unresolved
                    && matches!(
                        site.syntax_kind.as_str(),
                        "vba.direct_type_identifier.reference"
                            | "vba.implemented_type.reference"
                            | "vba.type_identifier.reference"
                            | "vba.automatic_type_identifier.reference"
                            | "vba.constructed_type_identifier.reference"
                            | "vba.tested_type_identifier.reference"
                    )
            }
            Self::TypeAssociation => {
                // The same pass also supplies native value and call receivers;
                // those independent tails are not declaration-type captures.
                definition
                    || matches!(
                        site.syntax_kind.as_str(),
                        "vba.direct_type_identifier.reference"
                            | "vba.automatic_type_identifier.reference"
                            | "vba.type_identifier.reference"
                    )
                    || (site.role == OccurrenceRole::Reference
                        && matches!(site.target, OccurrenceTarget::Resolved { .. })
                        && matches!(
                            site.syntax_kind.as_str(),
                            "vba.value_identifier.reference" | "vba.identifier.reference"
                        ))
                    || (site.syntax_kind == "vba.application.reference"
                        && matches!(site.target, OccurrenceTarget::Resolved { .. }))
            }
            Self::ConstructedTypes => {
                site.role == OccurrenceRole::TypeUse
                    && site.syntax_kind == "vba.constructed_type_identifier.reference"
            }
            Self::Constructions => {
                site.role == OccurrenceRole::Reference
                    && unresolved
                    && site.syntax_kind == "vba.construction.reference"
            }
            Self::Calls => {
                definition
                    || (site.syntax_kind == "vba.nonvalue_identifier.reference"
                        && site.role == OccurrenceRole::Reference
                        && unresolved)
                    || site.syntax_kind == "vba.application.reference"
                    || is_invoked_member(&site.syntax_kind)
                    || matches!(
                        site.syntax_kind.as_str(),
                        "vba.invoked_get_identifier.reference"
                            | "vba.invoked_let_identifier.reference"
                            | "vba.invoked_set_identifier.reference"
                            | "vba.invoked_identifier.reference"
                            | "vba.member.reference"
                    )
            }
            Self::Members => is_member(&site.syntax_kind),
            Self::MemberApplications => {
                (site.syntax_kind == "vba.application.reference" && unresolved)
                    || (site.syntax_kind == "vba.value_identifier.reference"
                        && site.role == OccurrenceRole::Reference
                        && unresolved)
                    || (is_member(&site.syntax_kind) && site.role == OccurrenceRole::Reference)
            }
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct OccurrenceViews {
    positions: [Vec<usize>; VIEW_COUNT],
}

impl OccurrenceViews {
    /// Records one retained input position without sorting or identity deduplication.
    ///
    /// Fixed classification and selected entries consume the transaction budget
    /// before mutation; cancellation or insufficient work aborts construction.
    ///
    /// # Errors
    /// Returns the existing cancellation or work-limit error without appending entries.
    pub(super) fn record(
        &mut self,
        position: usize,
        occurrence: &OccurrenceRecord,
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<(), ResolutionError> {
        cancellation.check()?;
        work.consume()?;
        let selected = OccurrenceView::ALL.map(|view| view.includes(occurrence));
        cancellation.check()?;
        work.consume_many(selected.iter().filter(|selected| **selected).count())?;
        cancellation.check()?;
        for (view, selected) in OccurrenceView::ALL.into_iter().zip(selected) {
            if selected {
                self.positions[view.index()].push(position);
            }
        }
        Ok(())
    }

    /// Borrows a category from the unchanged retained slice used during recording.
    ///
    /// # Panics
    /// Panics if the caller replaces that slice with one lacking a recorded position.
    pub(super) fn iter<'a>(
        &'a self,
        view: OccurrenceView,
        retained: &'a [OccurrenceRecord],
    ) -> impl Iterator<Item = &'a OccurrenceRecord> {
        self.positions[view.index()].iter().map(move |position| {
            retained
                .get(*position)
                .expect("view positions belong to the unchanged retained occurrence slice")
        })
    }
}
