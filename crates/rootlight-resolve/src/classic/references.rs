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
        }
    }
    Ok(Some(ReferenceInventory {
        declarations: project.references.clone(),
        names,
        unknown_names,
    }))
}

impl ReferenceInventory {
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
