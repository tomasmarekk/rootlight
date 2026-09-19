//! Binds declared Office VBA ownership to one immutable ZIP snapshot.
//! The retained evidence covers package/main relationships and content types;
//! it is not document-schema validation, signature authentication or index publication.

use crate::source_archive::{
    ExpandedZipSource, ZipExpansionError, ZipSourceArchive, ZipSourceLimits,
};
use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_source::opc::{
    ContentTypeDeclaration, ContentTypeSelector, ContentTypes, MetadataError, MetadataLimits,
    Relationship, Relationships, TargetMode,
    parts::{PartName, PartNameError},
};
use std::collections::{BTreeMap, BTreeSet};

const VBA_TYPE: &str = "application/vnd.ms-office.vbaProject";
const VBA_REL: &str = "http://schemas.microsoft.com/office/2006/relationships/vbaProject";
const OFFICE_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
const STRICT_OFFICE_REL: &str =
    "http://purl.oclc.org/ooxml/officeDocument/relationships/officeDocument";
const RELS_TYPE: &str = "application/vnd.openxmlformats-package.relationships+xml";

/// Resource limits shared across one Office ownership read.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct OfficePackageLimits {
    /// Non-refundable ZIP input, output, entry and read budgets.
    pub zip: ZipSourceLimits,
    /// Bounds applied separately to each of the three metadata sources.
    pub metadata: MetadataLimits,
}

/// A source-free failure to establish declared package ownership.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OfficePackageError {
    /// ZIP metadata, decoding or admission failed.
    #[error(transparent)]
    Archive(#[from] ZipExpansionError),
    /// OPC XML metadata failed validation.
    #[error(transparent)]
    Metadata(#[from] MetadataError),
    /// Logical part identity or relative resolution failed.
    #[error(transparent)]
    PartName(#[from] PartNameError),
    /// An invariant is missing, ambiguous or contradictory.
    #[error("invalid office package: {reason}")]
    Invalid {
        /// Static invariant label, never source text or archive names.
        reason: &'static str,
    },
    /// A format feature is explicit but not implemented by this reader.
    #[error("unsupported office package feature: {feature}")]
    Unsupported {
        /// Static feature label.
        feature: &'static str,
    },
    /// A bounded package metadata inventory was exceeded.
    #[error("office package logical name budget exceeded")]
    NameLimit,
    /// The caller cancelled or its deadline expired.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

/// Source-backed declared ownership of one Office VBA project part.
/// All five retained entry sources belong to the same original archive hash.
#[derive(Debug)]
pub struct OfficeVbaProject {
    main_name: PartName,
    project_name: PartName,
    types_source: ExpandedZipSource,
    package_relationships_source: ExpandedZipSource,
    main_source: ExpandedZipSource,
    main_relationships_source: ExpandedZipSource,
    project_source: ExpandedZipSource,
    main_declaration: ContentTypeDeclaration,
    project_declaration: ContentTypeDeclaration,
    main_relationship: Relationship,
    project_relationship: Relationship,
    non_part_entries: Vec<usize>,
}

impl OfficeVbaProject {
    /// Returns the canonical name of the package's declared main document part.
    #[must_use]
    pub const fn main_name(&self) -> &PartName {
        &self.main_name
    }
    /// Returns the canonical name of the uniquely declared VBA project.
    #[must_use]
    pub const fn project_name(&self) -> &PartName {
        &self.project_name
    }
    /// Returns the content-type XML and its original archive dependencies.
    #[must_use]
    pub const fn types_source(&self) -> &ExpandedZipSource {
        &self.types_source
    }
    /// Returns the package relationship XML and its archive dependencies.
    #[must_use]
    pub const fn package_relationships_source(&self) -> &ExpandedZipSource {
        &self.package_relationships_source
    }
    /// Returns the main document bytes; their application schema is not validated here.
    #[must_use]
    pub const fn main_source(&self) -> &ExpandedZipSource {
        &self.main_source
    }
    /// Returns the main document's relationship XML and archive dependencies.
    #[must_use]
    pub const fn main_relationships_source(&self) -> &ExpandedZipSource {
        &self.main_relationships_source
    }
    /// Returns the selected project bytes for subsequent compound/VBA validation.
    #[must_use]
    pub const fn project_source(&self) -> &ExpandedZipSource {
        &self.project_source
    }
    /// Returns the main content-type declaration, bound to types_source.
    #[must_use]
    pub const fn main_declaration(&self) -> &ContentTypeDeclaration {
        &self.main_declaration
    }
    /// Returns the project content-type declaration, bound to types_source.
    #[must_use]
    pub const fn project_declaration(&self) -> &ContentTypeDeclaration {
        &self.project_declaration
    }
    /// Returns the selected officeDocument relationship, bound to package_relationships_source.
    #[must_use]
    pub const fn main_relationship(&self) -> &Relationship {
        &self.main_relationship
    }
    /// Returns the selected vbaProject relationship, bound to main_relationships_source.
    #[must_use]
    pub const fn project_relationship(&self) -> &Relationship {
        &self.project_relationship
    }
    /// Returns physical ordinals not mapped to parts, such as directories or trash items.
    /// They remain present in the original ZIP; this reader never silently publishes them.
    #[must_use]
    pub fn non_part_entries(&self) -> &[usize] {
        &self.non_part_entries
    }
}

/// Reads the unique declared VBA project through package/main relationship evidence.
///
/// None means no logical part declares the VBA media type; it is not a statement
/// that arbitrary embedded bytes contain no macros. Non-part ZIP items are not
/// promoted into parts. The reader never touches a filesystem or activates content.
///
/// # Errors
/// Rejects name collisions, ambiguous/orphaned projects, external or fragment-only
/// ownership targets, missing parts/types, invalid metadata, unsupported interleaved
/// streams, exceeded bounds or cancellation. A successful result establishes declared
/// ownership, not application schema validity, authenticity or generation identity.
pub fn read_vba_project(
    input: &[u8],
    limits: OfficePackageLimits,
    cancellation: &Cancellation,
) -> Result<Option<OfficeVbaProject>, OfficePackageError> {
    let mut archive = ZipSourceArchive::open(input, limits.zip, cancellation)?;
    let inventory = Inventory::read(&archive, cancellation)?;
    let types_source = read_metadata(&mut archive, inventory.types, limits.metadata, cancellation)?;
    let types = ContentTypes::parse(types_source.bytes(), limits.metadata, cancellation)?;
    for declaration in types.declarations() {
        cancellation.check()?;
        if declaration
            .media_type()
            .value()
            .eq_ignore_ascii_case(VBA_TYPE)
            && let ContentTypeSelector::PartName(name) = declaration.selector()
        {
            inventory.require(&PartName::parse(name.value())?)?;
        }
    }
    let mut project = None;
    for (name, ordinal) in &inventory.parts {
        cancellation.check()?;
        if let Some(declaration) = types.for_part(name.as_str())
            && declaration
                .media_type()
                .value()
                .eq_ignore_ascii_case(VBA_TYPE)
        {
            if name.is_relationships() || project.is_some() {
                return Err(invalid("ambiguous vba project content type"));
            }
            project = Some((name.clone(), *ordinal, declaration.clone()));
        }
    }
    let Some((project_name, project_ordinal, project_declaration)) = project else {
        return Ok(None);
    };
    let root_name = PartName::relationships_for(None)?;
    let root_ordinal = inventory.require(&root_name)?;
    check_relationship_type(&types, &root_name)?;
    let package_relationships_source =
        read_metadata(&mut archive, root_ordinal, limits.metadata, cancellation)?;
    let root = Relationships::parse(
        package_relationships_source.bytes(),
        limits.metadata,
        cancellation,
    )?;
    let main_relationship = unique_relation(&root, &[OFFICE_REL, STRICT_OFFICE_REL])?.clone();
    let main_name = ownership_target(None, &main_relationship)?;
    if main_name == project_name {
        return Err(invalid("main document aliases vba project"));
    }
    let main_ordinal = inventory.require(&main_name)?;
    let main_declaration = types
        .for_part(main_name.as_str())
        .ok_or_else(|| invalid("main document has no declared content type"))?
        .clone();
    let main_rels_name = PartName::relationships_for(Some(&main_name))?;
    let main_rels_ordinal = inventory.require(&main_rels_name)?;
    check_relationship_type(&types, &main_rels_name)?;
    let main_relationships_source = read_metadata(
        &mut archive,
        main_rels_ordinal,
        limits.metadata,
        cancellation,
    )?;
    let main_rels = Relationships::parse(
        main_relationships_source.bytes(),
        limits.metadata,
        cancellation,
    )?;
    let project_relationship = unique_relation(&main_rels, &[VBA_REL])?.clone();
    if ownership_target(Some(&main_name), &project_relationship)? != project_name {
        return Err(invalid("vba relationship and content type disagree"));
    }
    let main_source = archive.read_entry(main_ordinal, cancellation)?;
    let project_source = archive.read_entry(project_ordinal, cancellation)?;
    cancellation.check()?;
    Ok(Some(OfficeVbaProject {
        main_name,
        project_name,
        types_source,
        package_relationships_source,
        main_source,
        main_relationships_source,
        project_source,
        main_declaration,
        project_declaration,
        main_relationship,
        project_relationship,
        non_part_entries: inventory.non_parts,
    }))
}

struct Inventory {
    types: usize,
    parts: BTreeMap<PartName, usize>,
    non_parts: Vec<usize>,
}

impl Inventory {
    fn read(
        archive: &ZipSourceArchive<'_>,
        cancellation: &Cancellation,
    ) -> Result<Self, OfficePackageError> {
        let mut parts = BTreeMap::new();
        let mut names = BTreeSet::new();
        let mut non_parts = Vec::new();
        let mut types = None;
        let mut bytes = 0usize;
        for (ordinal, entry) in archive.entries().iter().enumerate() {
            cancellation.check()?;
            bytes = bytes
                .checked_add(entry.name().len())
                .filter(|total| *total <= 1024 * 1024)
                .ok_or(OfficePackageError::NameLimit)?;
            let name = std::str::from_utf8(entry.name())
                .ok()
                .filter(|name| name.is_ascii())
                .ok_or(PartNameError::ZipEncoding)?;
            if !names.insert(name.to_ascii_lowercase()) {
                return Err(invalid("equivalent zip item names"));
            }
            if name.eq_ignore_ascii_case("[Content_Types].xml") {
                types = Some(ordinal);
                continue;
            }
            if is_piece(name) {
                return Err(OfficePackageError::Unsupported {
                    feature: "interleaved opc streams",
                });
            }
            match PartName::from_zip_name(entry.name()) {
                Ok(name) => {
                    if parts.insert(name, ordinal).is_some() {
                        return Err(invalid("equivalent logical part names"));
                    }
                }
                Err(PartNameError::Invalid) => non_parts.push(ordinal),
                Err(error) => return Err(error.into()),
            }
        }
        let logical_names: BTreeSet<_> = parts.keys().map(PartName::as_str).collect();
        for name in &logical_names {
            cancellation.check()?;
            // Search once per part, not once per path segment: reparsing every
            // prefix makes a bounded but deeply nested name quadratic work.
            let prefix = format!("{name}/");
            if logical_names
                .range::<str, _>((
                    std::ops::Bound::Included(prefix.as_str()),
                    std::ops::Bound::Unbounded,
                ))
                .next()
                .is_some_and(|candidate| candidate.starts_with(&prefix))
            {
                return Err(invalid("part name derives from another part"));
            }
        }
        Ok(Self {
            types: types.ok_or_else(|| invalid("missing content types stream"))?,
            parts,
            non_parts,
        })
    }
    fn require(&self, name: &PartName) -> Result<usize, OfficePackageError> {
        self.parts
            .get(name)
            .copied()
            .ok_or_else(|| invalid("relationship part is missing"))
    }
}

fn invalid(reason: &'static str) -> OfficePackageError {
    OfficePackageError::Invalid { reason }
}

fn is_piece(name: &str) -> bool {
    let Some((_, leaf)) = name.rsplit_once('/') else {
        return false;
    };
    let lower = leaf.to_ascii_lowercase();
    let Some(number) = lower.strip_prefix('[').and_then(|s| {
        s.strip_suffix("].piece")
            .or_else(|| s.strip_suffix("].last.piece"))
    }) else {
        return false;
    };
    number == "0"
        || (!number.starts_with('0')
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit()))
}

fn read_metadata(
    archive: &mut ZipSourceArchive<'_>,
    ordinal: usize,
    limits: MetadataLimits,
    cancellation: &Cancellation,
) -> Result<ExpandedZipSource, OfficePackageError> {
    let entry = archive
        .entries()
        .get(ordinal)
        .ok_or_else(|| invalid("metadata entry is missing"))?;
    if entry.expanded_bytes() > u64::try_from(limits.bytes).map_err(|_| MetadataError::Limits)? {
        return Err(MetadataError::Limit {
            resource: "xml bytes",
        }
        .into());
    }
    Ok(archive.read_entry(ordinal, cancellation)?)
}

fn unique_relation<'a>(
    document: &'a Relationships,
    kinds: &[&str],
) -> Result<&'a Relationship, OfficePackageError> {
    let mut matches = document
        .declarations()
        .iter()
        .filter(|record| kinds.contains(&record.kind().value()));
    let selected = matches
        .next()
        .ok_or_else(|| invalid("ownership relationship is missing"))?;
    if matches.next().is_some() {
        return Err(invalid("ownership relationship is ambiguous"));
    }
    Ok(selected)
}

fn ownership_target(
    source: Option<&PartName>,
    record: &Relationship,
) -> Result<PartName, OfficePackageError> {
    if record.mode() != TargetMode::Internal {
        return Err(invalid("external ownership relationship"));
    }
    let resolved = PartName::resolve(source, record.target().value())?;
    if resolved
        .fragment()
        .is_some_and(|fragment| !fragment.is_empty())
        || resolved.part().is_relationships()
    {
        return Err(invalid("ownership target is not a complete document part"));
    }
    Ok(resolved.part().clone())
}

fn check_relationship_type(
    types: &ContentTypes,
    name: &PartName,
) -> Result<(), OfficePackageError> {
    // The OPC relationship media type is intrinsic; a contradictory explicit
    // declaration must not be used as authority for an ownership chain.
    if types
        .for_part(name.as_str())
        .is_some_and(|record| !record.media_type().value().eq_ignore_ascii_case(RELS_TYPE))
    {
        return Err(invalid("contradictory relationships content type"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
