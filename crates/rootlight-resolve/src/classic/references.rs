//! Ordered native reference declarations crossing the service/resolver boundary.
//! Retained bytes prove declarations and labels, not library contents, host
//! precedence, or the absence of competing external modules and members.

use super::*;
use rootlight_ir::{FileRecord, SourceSpan};

/// One complete reference record in physical metadata order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeClassicReference {
    /// Complete retained record, absent when metadata admission failed.
    /// Missing evidence is not an empty referenced namespace.
    pub source: Option<SourceSpan>,
    /// Fields in declaration order; control fields share this record's slot.
    /// Absent control sentinels are not references. An empty list does not prove
    /// that the record contributes no external names.
    pub fields: Vec<NativeClassicReferenceField>,
}

/// A source-declared library/project identity whose namespace remains unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeClassicReferenceField {
    /// Retained field and any associated name, within the complete record.
    pub source: SourceSpan,
    /// Exactly decoded declaration label; absent or invalid labels stay unknown.
    pub name: Option<String>,
    /// Interpreted library/project identifier, never a resolved namespace.
    /// None preserves a malformed or unsupported identifier without guessing.
    pub identity: Option<String>,
    /// Decoded project-reference fields, absent for libraries or opaque records.
    /// These are locators, not evidence that an indexed target has been found.
    pub project: Option<NativeClassicProjectReference>,
}

/// The project form and path syntax declared by both ProjectReference fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeClassicProjectReferenceKind {
    /// Standalone project using Windows path syntax (A).
    WindowsStandalone,
    /// Standalone project using Macintosh path syntax (B).
    MacintoshStandalone,
    /// Host-embedded project using Windows path syntax (C).
    WindowsEmbedded,
    /// Host-embedded project using Macintosh path syntax (D).
    MacintoshEmbedded,
}

impl NativeClassicProjectReferenceKind {
    fn tag(self) -> u8 {
        match self {
            Self::WindowsStandalone => b'A',
            Self::MacintoshStandalone => b'B',
            Self::WindowsEmbedded => b'C',
            Self::MacintoshEmbedded => b'D',
        }
    }
}

/// One host-decoded locator, without normalization or filesystem authority.
#[derive(Clone, PartialEq, Eq)]
pub struct NativeClassicProjectLocator {
    /// Complete original ProjectReference identifier, including its three-byte prefix.
    pub source: SourceSpan,
    /// Exact losslessly decoded path, excluding the prefix; empty paths stay empty.
    pub path: String,
}

impl std::fmt::Debug for NativeClassicProjectLocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeClassicProjectLocator")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// One source-declared project dependency with both locators and expected version.
/// Host decoding establishes text/kind agreement. Resolver preparation checks
/// the retained coordinates, kind tags and version bytes, not target identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeClassicProjectReference {
    /// Shared declared path syntax and standalone/embedded form.
    pub kind: NativeClassicProjectReferenceKind,
    /// Locator from LibidAbsolute; its path has not been validated as absolute.
    pub absolute: NativeClassicProjectLocator,
    /// Locator from LibidRelative; its path has not been validated as relative.
    pub relative: NativeClassicProjectLocator,
    /// Expected major version, not a compatibility guarantee.
    pub major_version: u32,
    /// Expected minor version, not a compatibility guarantee.
    pub minor_version: u16,
    /// Exact six-byte little-endian major/minor payload in the same record.
    pub version_source: SourceSpan,
}

#[derive(Default)]
pub(super) struct ReferenceInventory {
    declarations: Vec<NativeClassicReference>,
    names: BTreeMap<String, Vec<(usize, usize)>>,
    unknown_names: bool,
}

pub(super) fn prepare(
    project: &NativeClassicProject,
    container: &FileRecord,
    files: &BTreeMap<FileId, &FileRecord>,
    sources: &BTreeMap<FileId, &[u8]>,
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<Option<ReferenceInventory>, ResolutionError> {
    let path = format!("{}/@vba/references.bin", container.path);
    let mut previous_end = 0;
    let mut field_count = 0usize;
    let mut names = BTreeMap::<String, Vec<(usize, usize)>>::new();
    let mut unknown_names = false;
    for (ordinal, reference) in project.references.iter().enumerate() {
        cancellation.check()?;
        work.consume()?;
        field_count = field_count
            .checked_add(reference.fields.len())
            .ok_or(ResolutionError::ClassicSourceIdentity)?;
        if field_count > MAX_MANIFEST_ITEMS {
            return Ok(None);
        }
        let Some(span) = reference.source else {
            if !reference.fields.is_empty() {
                return Err(ResolutionError::ClassicSourceIdentity);
            }
            unknown_names = true;
            continue;
        };
        unknown_names |= reference.fields.is_empty();
        let file = files
            .get(&span.file())
            .filter(|file| {
                file.path == path
                    && file.encoding == "binary"
                    && file.language == "vba"
                    && file.repository == container.repository
                    && file.generation == container.generation
            })
            .ok_or(ResolutionError::ClassicSourceIdentity)?;
        if !sources.contains_key(&file.id)
            || span.start_byte() < previous_end
            || span.start_byte() >= span.end_byte()
            || span.end_byte() > file.byte_length
        {
            return Err(ResolutionError::ClassicSourceIdentity);
        }
        previous_end = span.end_byte();
        for (field_index, field) in reference.fields.iter().enumerate() {
            cancellation.check()?;
            work.consume()?;
            if field.source.file() != span.file()
                || field.source.start_byte() < span.start_byte()
                || field.source.end_byte() > span.end_byte()
                || field.source.start_byte() >= field.source.end_byte()
            {
                return Err(ResolutionError::ClassicSourceIdentity);
            }
            if let Some(name) = field.name.as_deref().and_then(identifier) {
                names.entry(name).or_default().push((ordinal, field_index));
            } else {
                unknown_names = true;
            }
            if let Some(project) = &field.project {
                if field.identity.is_none() {
                    return Err(ResolutionError::ClassicSourceIdentity);
                }
                validate_project(project, field.source, sources, cancellation, work)?;
            }
        }
    }
    Ok(Some(ReferenceInventory {
        declarations: project.references.clone(),
        names,
        unknown_names,
    }))
}

fn validate_project(
    project: &NativeClassicProjectReference,
    owner: SourceSpan,
    sources: &BTreeMap<FileId, &[u8]>,
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<(), ResolutionError> {
    let mut previous_end = owner.start_byte();
    for (span, prefix) in [
        (project.absolute.source, Some(project.kind.tag())),
        (project.relative.source, Some(project.kind.tag())),
        (project.version_source, None),
    ] {
        cancellation.check()?;
        work.consume()?;
        if span.file() != owner.file()
            || span.start_byte() < previous_end
            || span.start_byte() >= span.end_byte()
            || span.end_byte() > owner.end_byte()
        {
            return Err(ResolutionError::ClassicSourceIdentity);
        }
        let start = usize::try_from(span.start_byte())
            .map_err(|_| ResolutionError::ClassicSourceIdentity)?;
        let end =
            usize::try_from(span.end_byte()).map_err(|_| ResolutionError::ClassicSourceIdentity)?;
        let bytes = sources
            .get(&span.file())
            .and_then(|bytes| bytes.get(start..end))
            .ok_or(ResolutionError::ClassicSourceIdentity)?;
        if let Some(tag) = prefix {
            if !bytes.starts_with(&[b'*', b'\\', tag]) {
                return Err(ResolutionError::ClassicSourceIdentity);
            }
        } else {
            let mut expected = [0; 6];
            expected[..4].copy_from_slice(&project.major_version.to_le_bytes());
            expected[4..].copy_from_slice(&project.minor_version.to_le_bytes());
            if bytes != expected {
                return Err(ResolutionError::ClassicSourceIdentity);
            }
        }
        previous_end = span.end_byte();
    }
    Ok(())
}

impl ReferenceInventory {
    pub(super) fn may_shadow_standard_name(&self, name: &str) -> bool {
        // Labels cover the project-name tier, not the contents or precedence
        // of referenced namespaces. Even a differently named library can
        // expose a competing module, type, or member before the standard one.
        self.may_declare_name(name) || !self.declarations.is_empty()
    }

    pub(super) fn may_declare_name(&self, name: &str) -> bool {
        // An unavailable label can occupy the project-name tier before a
        // standard member/type. Known labels still prove no library contents.
        self.unknown_names || self.declared_name(name).is_some()
    }

    pub(super) fn declared_name(&self, name: &str) -> Option<&NativeClassicReferenceField> {
        // Any declared label is a barrier; selecting this witness does not
        // select a winning library or discard its other declaration slots.
        let (ordinal, field) = self.names.get(name)?.first()?;
        self.declarations.get(*ordinal)?.fields.get(*field)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{GenerationId, RepositoryId};
    use rootlight_ir::FactEvidence;

    fn fixture() -> (FileRecord, FileRecord, NativeClassicProject) {
        let container = FileRecord {
            id: FileId::from_bytes([1; 20]),
            repository: RepositoryId::from_bytes([2; 16]),
            generation: GenerationId::from_bytes([3; 20]),
            path: "book.xlsm".to_owned(),
            path_locator: None,
            content_hash: content_hash(b"container"),
            byte_length: 9,
            language: "office-open-xml".to_owned(),
            encoding: "binary".to_owned(),
            generated: false,
            provenance: FactId::from_bytes([4; 20]),
            evidence: FactEvidence {
                source: None,
                derivation: Vec::new(),
            },
        };
        let mut metadata = container.clone();
        metadata.id = FileId::from_bytes([5; 20]);
        metadata.path = "book.xlsm/@vba/references.bin".to_owned();
        metadata.language = "vba".to_owned();
        metadata.content_hash = content_hash(&[0; 128]);
        metadata.byte_length = 128;
        let references = (0..3)
            .map(|index| {
                let span = SourceSpan::new(metadata.id, index * 32, (index + 1) * 32).unwrap();
                NativeClassicReference {
                    source: Some(span),
                    fields: vec![NativeClassicReferenceField {
                        source: span,
                        name: (index != 1)
                            .then(|| if index == 0 { "Alias" } else { "ALIAS" }.to_owned()),
                        identity: None,
                        project: None,
                    }],
                }
            })
            .chain(std::iter::once(NativeClassicReference {
                source: None,
                fields: Vec::new(),
            }))
            .collect();
        let project = NativeClassicProject {
            container: container.id,
            content_hash: container.content_hash,
            name: Some("Project".to_owned()),
            members: Vec::new(),
            references,
            inventory_complete: true,
        };
        (container, metadata, project)
    }

    fn run(
        project: &NativeClassicProject,
        container: &FileRecord,
        metadata: &FileRecord,
    ) -> Result<Option<ReferenceInventory>, ResolutionError> {
        prepare(
            project,
            container,
            &BTreeMap::from([(metadata.id, metadata)]),
            &BTreeMap::from([(metadata.id, [0; 128].as_slice())]),
            &Cancellation::new(),
            &mut ResolutionWorkBudget::new(100),
        )
    }

    #[test]
    fn project_reference_locators_retain_kind_version_and_namespace_barriers() {
        for kind in [
            NativeClassicProjectReferenceKind::WindowsStandalone,
            NativeClassicProjectReferenceKind::MacintoshStandalone,
            NativeClassicProjectReferenceKind::WindowsEmbedded,
            NativeClassicProjectReferenceKind::MacintoshEmbedded,
        ] {
            for empty in [false, true] {
                let (container, mut metadata, mut project) = fixture();
                project.references.truncate(1);
                let mut bytes = [0; 128];
                let length = if empty { 3 } else { 4 };
                bytes[..4].copy_from_slice(&[b'*', b'\\', kind.tag(), b'x']);
                bytes[8..12].copy_from_slice(&[b'*', b'\\', kind.tag(), b'y']);
                bytes[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
                bytes[20..22].copy_from_slice(&u16::MAX.to_le_bytes());
                metadata.content_hash = content_hash(&bytes);
                let field = &mut project.references[0].fields[0];
                field.identity = Some("project declaration".to_owned());
                field.project = Some(NativeClassicProjectReference {
                    kind,
                    absolute: NativeClassicProjectLocator {
                        source: SourceSpan::new(metadata.id, 0, length).unwrap(),
                        path: if empty { "" } else { "x" }.to_owned(),
                    },
                    relative: NativeClassicProjectLocator {
                        source: SourceSpan::new(metadata.id, 8, 8 + length).unwrap(),
                        path: if empty { "" } else { "y" }.to_owned(),
                    },
                    major_version: u32::MAX,
                    minor_version: u16::MAX,
                    version_source: SourceSpan::new(metadata.id, 16, 22).unwrap(),
                });
                let files = BTreeMap::from([(metadata.id, &metadata)]);
                let sources = BTreeMap::from([(metadata.id, bytes.as_slice())]);
                let mut work = ResolutionWorkBudget::new(5);
                let inventory = prepare(
                    &project,
                    &container,
                    &files,
                    &sources,
                    &Cancellation::new(),
                    &mut work,
                )
                .unwrap()
                .unwrap();
                assert_eq!(work.consumed(), 5);
                assert_eq!(inventory.declarations, project.references);
                assert!(inventory.may_shadow_standard_name("collection"));
                assert!(!inventory.may_declare_name("collection"));
                for budget in 2..5 {
                    assert!(matches!(
                        prepare(
                            &project,
                            &container,
                            &files,
                            &sources,
                            &Cancellation::new(),
                            &mut ResolutionWorkBudget::new(budget)
                        ),
                        Err(ResolutionError::WorkLimit { .. })
                    ));
                }
                for case in 0..9 {
                    let mut invalid = project.clone();
                    let field = &mut invalid.references[0].fields[0];
                    let descriptor = field.project.as_mut().unwrap();
                    match case {
                        0 => {
                            descriptor.kind =
                                if kind == NativeClassicProjectReferenceKind::WindowsEmbedded {
                                    NativeClassicProjectReferenceKind::MacintoshEmbedded
                                } else {
                                    NativeClassicProjectReferenceKind::WindowsEmbedded
                                }
                        }
                        1 => descriptor.major_version = 0,
                        2 => descriptor.minor_version = 0,
                        3 => descriptor.relative.source = descriptor.absolute.source,
                        4 => {
                            descriptor.absolute.source =
                                SourceSpan::new(container.id, 0, length).unwrap()
                        }
                        5 => {
                            descriptor.version_source =
                                SourceSpan::new(metadata.id, 16, 33).unwrap()
                        }
                        6 => {
                            descriptor.version_source =
                                SourceSpan::new(metadata.id, 16, 21).unwrap()
                        }
                        7 => {
                            descriptor.absolute.source = SourceSpan::new(metadata.id, 0, 2).unwrap()
                        }
                        8 => field.identity = None,
                        _ => unreachable!(),
                    }
                    assert!(
                        matches!(
                            prepare(
                                &invalid,
                                &container,
                                &files,
                                &sources,
                                &Cancellation::new(),
                                &mut ResolutionWorkBudget::new(100)
                            ),
                            Err(ResolutionError::ClassicSourceIdentity)
                        ),
                        "case {case}"
                    );
                }
                assert!(matches!(
                    prepare(
                        &project,
                        &container,
                        &files,
                        &BTreeMap::from([(metadata.id, &bytes[..21])]),
                        &Cancellation::new(),
                        &mut ResolutionWorkBudget::new(100)
                    ),
                    Err(ResolutionError::ClassicSourceIdentity)
                ));
            }
        }
    }

    #[test]
    fn reference_inventory_retains_unknown_and_repeated_declarations() {
        let (container, metadata, project) = fixture();
        let inventory = run(&project, &container, &metadata).unwrap().unwrap();
        assert_eq!(inventory.declarations, project.references);
        assert_eq!(inventory.names["alias"], [(0, 0), (2, 0)]);
        assert_eq!(
            inventory.declared_name("alias"),
            Some(&project.references[0].fields[0])
        );
        assert!(inventory.declared_name("missing").is_none());
        assert!(inventory.may_declare_name("missing"));
        let mut named = project.clone();
        named.references.retain(|reference| {
            reference.source.is_some() && reference.fields.iter().all(|field| field.name.is_some())
        });
        let inventory = run(&named, &container, &metadata).unwrap().unwrap();
        assert!(inventory.may_declare_name("alias"));
        assert!(!inventory.may_declare_name("missing"));
        assert!(inventory.may_shadow_standard_name("missing"));
        named.references.clear();
        let empty = run(&named, &container, &metadata).unwrap().unwrap();
        assert!(!empty.may_shadow_standard_name("alias"));
        assert!(!empty.may_shadow_standard_name("missing"));
        let mut work = ResolutionWorkBudget::new(100);
        prepare(
            &project,
            &container,
            &BTreeMap::from([(metadata.id, &metadata)]),
            &BTreeMap::from([(metadata.id, [0; 128].as_slice())]),
            &Cancellation::new(),
            &mut work,
        )
        .unwrap();
        assert_eq!(work.consumed(), 7);
    }

    #[test]
    fn reference_inventory_rejects_reordered_or_unbound_source_slots() {
        let (container, metadata, original) = fixture();
        for case in 0..7 {
            let mut project = original.clone();
            let mut metadata = metadata.clone();
            match case {
                0 => project.references.swap(0, 1),
                1 => project.references[1].source = project.references[0].source,
                2 => {
                    project.references[0].fields[0].source =
                        SourceSpan::new(metadata.id, 0, 33).unwrap()
                }
                3 => project.references[0].source = None,
                4 => metadata.path = "other.xlsm/@vba/references.bin".to_owned(),
                5 => metadata.generation = GenerationId::from_bytes([9; 20]),
                6 => {
                    project.references[0].fields[0].source =
                        SourceSpan::new(container.id, 0, 9).unwrap()
                }
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    run(&project, &container, &metadata),
                    Err(ResolutionError::ClassicSourceIdentity)
                ),
                "case {case}"
            );
        }
    }

    #[test]
    fn reference_inventory_checks_missing_bytes_cancellation_and_work() {
        let (container, metadata, project) = fixture();
        let files = BTreeMap::from([(metadata.id, &metadata)]);
        let bytes = [0; 128];
        let sources = BTreeMap::from([(metadata.id, bytes.as_slice())]);
        assert!(matches!(
            prepare(
                &project,
                &container,
                &files,
                &BTreeMap::new(),
                &Cancellation::new(),
                &mut ResolutionWorkBudget::new(100)
            ),
            Err(ResolutionError::ClassicSourceIdentity)
        ));
        assert!(matches!(
            prepare(
                &project,
                &container,
                &files,
                &sources,
                &Cancellation::new(),
                &mut ResolutionWorkBudget::new(1)
            ),
            Err(ResolutionError::WorkLimit { .. })
        ));
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            prepare(
                &project,
                &container,
                &files,
                &sources,
                &cancellation,
                &mut ResolutionWorkBudget::new(100)
            ),
            Err(ResolutionError::Cancelled(_))
        ));
    }
}
