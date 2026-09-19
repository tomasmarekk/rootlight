//! Reconciles PROJECT declarations with decoded dir module records.
//! Both inventories retain every record and its outcome; metadata agreement is
//! not source-body identity, designer activation, or complete VBA name resolution.

use std::{collections::BTreeMap, fmt, ops::Range};

use rootlight_cancel::{Cancellation, Cancelled};

use crate::{
    compound::CompoundStream,
    vba_dir::{DirectoryError, DirectoryLimits, ModuleKind, ProjectDirectory},
    vba_project::{ProjectError, ProjectLimits, ProjectModuleKind, ProjectProperties},
    vba_source::{SourceError, VbaProject},
    vba_text::{DecodedText, TextError, TextLimits},
};

#[cfg(test)]
mod tests;

/// Independent limits for the two parsed inventories, not source module bodies.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct MembershipLimits {
    /// PROJECT stream, property line and item limits.
    pub project: ProjectLimits,
    /// Decoded dir stream, reference and module limits.
    pub directory: DirectoryLimits,
}

/// A whole-inventory failure; individual name/type failures remain on records.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MembershipError {
    /// The enclosing PROJECT stream cannot be read unambiguously.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// PROJECT framing is malformed or exceeds its own limits.
    #[error(transparent)]
    Project(#[from] ProjectError),
    /// The directory cannot be parsed within the requested limits.
    #[error(transparent)]
    Directory(#[from] DirectoryError),
    /// The caller cancelled before the complete inventory was produced.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

/// A declaration that cannot establish one compatible metadata counterpart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MembershipIssue {
    /// Empty, NUL-containing, or longer than 31 Unicode scalars.
    Name,
    /// No reviewed decoder exists for this project's codepage.
    UnsupportedCodePage,
    /// The name is not valid in the declared codepage.
    InvalidEncoding,
    /// An optional UTF-16 module-name copy disagrees with its MBCS field.
    UnicodeNameMismatch,
    /// No counterpart has the same name under the documented comparison.
    Missing,
    /// Multiple records on either side claim the same compared name.
    Ambiguous,
    /// The named counterpart has an independently invalid identity field.
    CounterpartInvalid,
    /// Procedural/object classification disagrees across the two streams.
    KindMismatch,
    /// Source-header spelling differs under exact Unicode plus ASCII-case comparison.
    SourceNameMismatch,
    /// The source header was interpreted with the wrong procedural/object mode.
    SourceKindMismatch,
}

/// One unique compatible pair under exact Unicode plus ASCII-case comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MembershipMatch {
    counterpart: usize,
    kind: ProjectModuleKind,
    non_ascii: bool,
}

impl MembershipMatch {
    /// Returns the ordinal in the other inventory, never a repository identity.
    #[must_use]
    pub const fn counterpart(&self) -> usize {
        self.counterpart
    }

    /// Returns the PROJECT subtype after checking dir's procedural/object bit.
    #[must_use]
    pub const fn kind(&self) -> ProjectModuleKind {
        self.kind
    }

    /// Whether full non-ASCII case equivalence still needs semantic validation.
    /// Exact character agreement is retained, not promoted to locale-aware identity.
    #[must_use]
    pub const fn needs_non_ascii_case_validation(&self) -> bool {
        self.non_ascii
    }
}

/// A declaration's decoded name, exact input ranges, and pairing or typed issue.
pub struct MembershipRecord {
    name: Option<String>,
    range: Range<usize>,
    name_range: Range<usize>,
    outcome: Result<MembershipMatch, MembershipIssue>,
}

impl fmt::Debug for MembershipRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MembershipRecord")
            .field("range", &self.range)
            .field("name_range", &self.name_range)
            .field("outcome", &self.outcome)
            .finish()
    }
}

impl MembershipRecord {
    /// Returns the decoded spelling, or `None` if decoding or name bounds failed.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the declaration range in its own uncompressed metadata stream.
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    /// Returns the original MBCS name range in its own metadata stream.
    #[must_use]
    pub fn name_range(&self) -> Range<usize> {
        self.name_range.clone()
    }

    /// Returns the pairing evidence or this declaration's failure.
    ///
    /// # Errors
    /// Returns a [`MembershipIssue`] without dropping the record or raw source.
    pub const fn outcome(&self) -> Result<MembershipMatch, MembershipIssue> {
        self.outcome
    }

    /// Checks an effective source-header name against this paired declaration.
    ///
    /// This preserves the pairing's non-ASCII case-validation flag and is not
    /// proof that an arbitrary supplied body belongs to this container/generation.
    /// Callers must obtain the header from this record's own bound module stream.
    ///
    /// # Errors
    /// Returns the existing metadata issue, a source-kind mismatch or a spelling
    /// mismatch. Case comparison has the same restricted scope as metadata pairing.
    pub fn check_source_name(
        &self,
        header: &crate::vba_header::HeaderName<'_>,
    ) -> Result<MembershipMatch, MembershipIssue> {
        let matched = self.outcome?;
        let expected = match matched.kind {
            ProjectModuleKind::Procedural => ModuleKind::Procedural,
            ProjectModuleKind::Class
            | ProjectModuleKind::Document { .. }
            | ProjectModuleKind::Designer => ModuleKind::Object,
        };
        if header.kind() != expected {
            return Err(MembershipIssue::SourceKindMismatch);
        }
        if !self
            .name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(header.name()))
        {
            return Err(MembershipIssue::SourceNameMismatch);
        }
        Ok(matched)
    }
}

/// Paired inventories read from one selected VBA storage and its PROJECT sibling.
/// PROJECT bytes retain physical provenance; dir ranges address the owning project.
#[derive(Debug)]
pub struct ProjectMembership {
    project_stream: CompoundStream,
    directory_records: Vec<MembershipRecord>,
    project_records: Vec<MembershipRecord>,
}

impl ProjectMembership {
    /// Reads and reconciles both complete inventories without reading module bodies.
    ///
    /// Original order is preserved. Names use their declared codepage, have at most
    /// 31 Unicode scalars, and are not normalized or restricted to Latin identifiers.
    /// The optional dir UTF-16 name must be the exact encoding of its MBCS name.
    /// Pairing folds ASCII case only; non-ASCII matches carry an explicit remaining
    /// case-validation flag. No locale-dependent or source-body identity is claimed.
    /// All duplicate claimants are invalidated, regardless of their record order.
    ///
    /// # Errors
    /// Returns [`MembershipError`] for whole-stream framing/read/limit failures or
    /// cancellation. Per-record issues remain visible in both returned inventories.
    pub fn read(
        project: &VbaProject<'_, '_>,
        limits: MembershipLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, MembershipError> {
        let project_stream =
            project.read_project_stream(limits.project.input_bytes, cancellation)?;
        let properties =
            ProjectProperties::parse(project_stream.bytes(), limits.project, cancellation)?;
        let directory = ProjectDirectory::parse(
            project.decoded_directory().bytes(),
            limits.directory,
            cancellation,
        )?;
        let (directory_records, project_records) =
            reconcile(&directory, &properties, cancellation)?;
        Ok(Self {
            project_stream,
            directory_records,
            project_records,
        })
    }

    /// Returns all dir declarations in their original order, including failures.
    #[must_use]
    pub fn directory_records(&self) -> &[MembershipRecord] {
        &self.directory_records
    }

    /// Returns all PROJECT module declarations, including extra or invalid items.
    #[must_use]
    pub fn project_records(&self) -> &[MembershipRecord] {
        &self.project_records
    }

    /// Returns exact PROJECT bytes with the original CFB physical-stream mappings.
    #[must_use]
    pub fn project_stream(&self) -> &CompoundStream {
        &self.project_stream
    }
}

fn name(
    bytes: &[u8],
    page: u16,
    cancellation: &Cancellation,
) -> Result<Result<String, MembershipIssue>, Cancelled> {
    let limits = TextLimits {
        input_bytes: 124,
        output_bytes: 124,
        mapping_points: 1,
    };
    let text = match DecodedText::decode(bytes, page, limits, cancellation) {
        Ok(text) => text,
        Err(TextError::Cancelled(error)) => return Err(error),
        Err(TextError::Unsupported { .. }) => return Ok(Err(MembershipIssue::UnsupportedCodePage)),
        Err(TextError::Invalid { .. }) => return Ok(Err(MembershipIssue::InvalidEncoding)),
        Err(TextError::Limits | TextError::Limit { .. }) => return Ok(Err(MembershipIssue::Name)),
    };
    if text.text().is_empty() || text.text().contains('\0') || text.text().chars().count() > 31 {
        return Ok(Err(MembershipIssue::Name));
    }
    Ok(Ok(text.text().to_owned()))
}

fn record(
    name: Result<String, MembershipIssue>,
    range: Range<usize>,
    name_range: Range<usize>,
) -> MembershipRecord {
    match name {
        Ok(name) => MembershipRecord {
            name: Some(name),
            range,
            name_range,
            outcome: Err(MembershipIssue::Missing),
        },
        Err(issue) => MembershipRecord {
            name: None,
            range,
            name_range,
            outcome: Err(issue),
        },
    }
}

fn reconcile(
    directory: &ProjectDirectory<'_>,
    properties: &ProjectProperties<'_>,
    cancellation: &Cancellation,
) -> Result<(Vec<MembershipRecord>, Vec<MembershipRecord>), Cancelled> {
    let mut left = Vec::new();
    let mut right = Vec::new();
    for module in directory.modules() {
        cancellation.check()?;
        let mut item = record(
            name(module.name().bytes(), directory.code_page(), cancellation)?,
            module.range(),
            module.name().range(),
        );
        if let (Some(text), Some(wide)) = (&item.name, module.unicode_name()) {
            let (units, remainder) = wide.bytes().as_chunks::<2>();
            if !remainder.is_empty()
                || !text
                    .encode_utf16()
                    .eq(units.iter().map(|unit| u16::from_le_bytes(*unit)))
            {
                item.outcome = Err(MembershipIssue::UnicodeNameMismatch);
            }
        }
        left.push(item);
    }
    for module in properties.modules() {
        cancellation.check()?;
        right.push(record(
            name(module.name(), directory.code_page(), cancellation)?,
            module.range(),
            module.name_range(),
        ));
    }
    let mut groups: BTreeMap<String, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for (index, item) in left.iter().enumerate() {
        cancellation.check()?;
        if let Some(name) = &item.name {
            groups
                .entry(name.to_ascii_lowercase())
                .or_default()
                .0
                .push(index);
        }
    }
    for (index, item) in right.iter().enumerate() {
        cancellation.check()?;
        if let Some(name) = &item.name {
            groups
                .entry(name.to_ascii_lowercase())
                .or_default()
                .1
                .push(index);
        }
    }
    for (key, (a, b)) in groups {
        cancellation.check()?;
        if a.len() > 1 || b.len() > 1 {
            // No first-wins rule: every duplicate must remain visibly ambiguous.
            for index in a {
                if let Some(item) = left.get_mut(index) {
                    item.outcome = Err(MembershipIssue::Ambiguous);
                }
            }
            for index in b {
                if let Some(item) = right.get_mut(index) {
                    item.outcome = Err(MembershipIssue::Ambiguous);
                }
            }
            continue;
        }
        let ([a], [b]) = (a.as_slice(), b.as_slice()) else {
            continue;
        };
        let Some(l) = left.get_mut(*a) else {
            continue;
        };
        let Some(r) = right.get_mut(*b) else {
            continue;
        };
        if l.outcome != Err(MembershipIssue::Missing) || r.outcome != Err(MembershipIssue::Missing)
        {
            if l.outcome == Err(MembershipIssue::Missing) {
                l.outcome = Err(MembershipIssue::CounterpartInvalid);
            }
            if r.outcome == Err(MembershipIssue::Missing) {
                r.outcome = Err(MembershipIssue::CounterpartInvalid);
            }
            continue;
        }
        let Some(dir) = directory.modules().get(*a) else {
            continue;
        };
        let Some(property) = properties.modules().get(*b) else {
            continue;
        };
        let kind = property.kind();
        let expected = match kind {
            ProjectModuleKind::Procedural => ModuleKind::Procedural,
            ProjectModuleKind::Class
            | ProjectModuleKind::Document { .. }
            | ProjectModuleKind::Designer => ModuleKind::Object,
        };
        if dir.kind() != expected {
            l.outcome = Err(MembershipIssue::KindMismatch);
            r.outcome = Err(MembershipIssue::KindMismatch);
        } else {
            l.outcome = Ok(MembershipMatch {
                counterpart: *b,
                kind,
                non_ascii: !key.is_ascii(),
            });
            r.outcome = Ok(MembershipMatch {
                counterpart: *a,
                kind,
                non_ascii: !key.is_ascii(),
            });
        }
    }
    cancellation.check()?;
    Ok((left, right))
}
