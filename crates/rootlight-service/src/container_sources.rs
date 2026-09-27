//! Prepares immutable derived sources before the generation manifest is sealed.
//! Physical discovery stays unchanged; virtual paths, parser inputs and container
//! dependencies belong to the prepared generation, never to filesystem discovery.

use super::*;
use rootlight_discovery::LanguageSignal;
use rootlight_ids::derive_file;
use rootlight_source::{
    compound::{CompoundFile, CompoundLimits},
    vba_dir::ModuleKind,
    vba_header::{HeaderLimits, HeaderName},
    vba_membership::{MembershipLimits, ProjectMembership},
    vba_project::ProjectModuleKind,
    vba_source::{ModuleReadLimits, ProjectReadLimits, root::RootedProject},
    vba_text::TextLimits,
};

pub(super) const REVISION: &[u8] = b"rootlight.container-sources/6";

mod references;

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct PreparedSources {
    pub inputs: Vec<ManifestInput>,
    pub fingerprints: Vec<InputFingerprint>,
    origins: Vec<(FileId, FileId)>,
    native_vba_modules: BTreeMap<FileId, rootlight_adapter_sdk::NativeVbaModuleKind>,
    incomplete_native_contexts: BTreeSet<FileId>,
    pub(super) native_projects: Vec<rootlight_resolve::NativeClassicProject>,
    issues: Vec<(FileId, String)>,
    references: Vec<references::DeclaredReference>,
}

struct Budget {
    work: usize,
    bytes: usize,
    files: usize,
}

impl Budget {
    // Failed reads spend the entire reservation: malformed containers cannot
    // repeatedly consume a fresh decompression allowance without accounting.
    fn reserve(&mut self) -> usize {
        let amount = self.work.min(16 * 1024 * 1024);
        self.work -= amount;
        amount
    }

    fn settle(&mut self, reserved: usize, used: usize) -> Result<(), FirstSliceError> {
        let unused = reserved.checked_sub(used).ok_or(FirstSliceError::Limits)?;
        self.work = self
            .work
            .checked_add(unused)
            .ok_or(FirstSliceError::Limits)?;
        Ok(())
    }
}

pub(super) fn prepare(
    inputs: &[ManifestInput],
    snapshots: &mut BTreeMap<FileId, SourceSnapshot>,
    repository: RepositoryId,
    remaining_files: usize,
    remaining_bytes: usize,
    cancellation: &Cancellation,
) -> Result<PreparedSources, FirstSliceError> {
    let mut prepared = PreparedSources::default();
    let mut budget = Budget {
        work: remaining_bytes,
        bytes: remaining_bytes,
        files: remaining_files,
    };
    for input in inputs {
        check_cancellation(cancellation)?;
        if detected_source_language(input) != Some("office-open-xml") {
            continue;
        }
        let snapshot = snapshots
            .get(&input.file)
            .ok_or(FirstSliceError::DiscoveryDrift)?;
        if snapshot.content_hash() != input.content_hash {
            return Err(FirstSliceError::DiscoveryDrift);
        }
        let mut expanded = Vec::new();
        prepare_office(
            input,
            snapshot,
            repository,
            &mut budget,
            &mut prepared,
            &mut expanded,
            cancellation,
        )?;
        for snapshot in expanded {
            if snapshots.insert(snapshot.file(), snapshot).is_some() {
                return Err(FirstSliceError::Identity);
            }
        }
    }
    Ok(prepared)
}

impl PreparedSources {
    pub(super) fn analysis_context<'a>(
        &self,
        request: rootlight_adapter_sdk::AnalysisRequest<'a>,
    ) -> Result<rootlight_adapter_sdk::AnalysisRequest<'a>, FirstSliceError> {
        match self
            .native_vba_modules
            .get(&request.source().source_ref().span().file())
        {
            Some(kind) => request
                .with_native_vba_module(*kind)
                .map_err(|_| FirstSliceError::Adapter),
            None => Ok(request),
        }
    }

    fn issue(&mut self, file: FileId, message: String) -> Result<(), FirstSliceError> {
        self.issues
            .try_reserve(1)
            .map_err(|_| FirstSliceError::Limits)?;
        self.issues.push((file, message));
        Ok(())
    }

    pub(super) fn append_evidence(
        &self,
        document: &mut NormalizedIrDocument,
        limits: &IrLimits,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        if self.origins.is_empty() && self.issues.is_empty() && self.references.is_empty() {
            return Ok(());
        }
        let extra = self
            .origins
            .len()
            .checked_mul(2)
            .and_then(|count| count.checked_add(self.issues.len()))
            .ok_or(FirstSliceError::Limits)?;
        checked_combined_length(
            normalized_record_count(document)?,
            extra,
            limits.max_total_records,
        )?;
        reserve_records(
            &mut document.source_mappings,
            self.origins.len(),
            limits.max_source_mappings,
        )?;
        reserve_records(
            &mut document.diagnostics,
            self.issues.len(),
            limits.max_diagnostics,
        )?;
        reserve_records(
            &mut document.provenance,
            self.origins.len(),
            limits.max_provenance_records,
        )?;
        let files = document
            .files
            .iter()
            .map(|file| (file.id, file))
            .collect::<BTreeMap<_, _>>();
        let reference = |file: FileId| -> Result<SourceRef, FirstSliceError> {
            let file = files.get(&file).ok_or(FirstSliceError::Identity)?;
            Ok(SourceRef::new(
                document.repository,
                document.generation,
                SourceSpan::new(file.id, 0, file.byte_length)
                    .map_err(|_| FirstSliceError::Identity)?,
                file.content_hash,
                None,
            ))
        };
        for &(derived, container) in &self.origins {
            check_cancellation(cancellation)?;
            let origin = reference(container)?;
            let mut provenance = ProvenanceRecord {
                id: FactId::from_bytes([0; 20]),
                repository: document.repository,
                generation: document.generation,
                producer_kind: ProducerKind::Rule,
                producer: ProducerIdentity::new(
                    "rootlight-container-sources",
                    "1.0.0",
                    first_slice_build_context(),
                )
                .map_err(|_| FirstSliceError::Identity)?,
                binary_digest: content_hash(REVISION),
                frontend_version: Some("office-vba-1".to_owned()),
                language: "vba".to_owned(),
                tier: AnalysisTier::TierD,
                build_context: BuildContextIdentity::new(first_slice_build_context()),
                input_sources: vec![origin.clone()],
                evidence_sources: vec![reference(derived)?],
                derivation_parents: Vec::new(),
                rule: Some("declared-office-vba-source".to_owned()),
            };
            provenance.id =
                derive_provenance_record_id(&provenance).map_err(|_| FirstSliceError::Identity)?;
            let mut mapping = SourceMappingRecord {
                id: FactId::from_bytes([0; 20]),
                repository: document.repository,
                generation: document.generation,
                from: reference(derived)?,
                to: origin.clone(),
                kind: SourceMappingKind::ExtractedFromContainer,
                provenance: provenance.id,
                evidence: FactEvidence {
                    source: Some(origin),
                    derivation: Vec::new(),
                },
            };
            mapping.id = rootlight_ir::derive_source_mapping_record_id(&mapping)
                .map_err(|_| FirstSliceError::Identity)?;
            document.source_mappings.push(mapping);
            document.provenance.push(provenance);
        }
        for (file, message) in &self.issues {
            check_cancellation(cancellation)?;
            let source = reference(*file)?;
            let mut diagnostic = DiagnosticRecord {
                id: FactId::from_bytes([0; 20]),
                repository: document.repository,
                generation: document.generation,
                code: "container-source-incomplete".to_owned(),
                message: message.clone(),
                severity: DiagnosticSeverity::Warning,
                source: Some(source.clone()),
                coverage_effect: CoverageStatus::Unknown,
                provenance: files.get(file).ok_or(FirstSliceError::Identity)?.provenance,
                evidence: FactEvidence {
                    source: Some(source),
                    derivation: Vec::new(),
                },
            };
            diagnostic.id =
                derive_diagnostic_record_id(&diagnostic).map_err(|_| FirstSliceError::Identity)?;
            document.diagnostics.push(diagnostic);
        }
        if !self.origins.is_empty() {
            document.version = NormalizedIrVersion::V1_6;
        }
        let mut increments = references::append(&self.references, document, limits, cancellation)?;
        for &(derived, _) in &self.origins {
            increments.insert((derived, IrFactDomain::SourceMappings), 1);
            let count = increments
                .entry((derived, IrFactDomain::Provenance))
                .or_default();
            *count = count.checked_add(1).ok_or(FirstSliceError::Limits)?;
        }
        for &(file, _) in &self.issues {
            let count = increments
                .entry((file, IrFactDomain::Diagnostics))
                .or_default();
            *count = count.checked_add(1).ok_or(FirstSliceError::Limits)?;
        }
        for record in &mut document.coverage_records {
            check_cancellation(cancellation)?;
            let CoverageScope::File(file) = record.scope else {
                continue;
            };
            // File/source and explicit declaration inventories stay exact; the
            // additional host namespace has not been enumerated or resolved.
            if self.incomplete_native_contexts.contains(&file)
                && matches!(
                    record.domain,
                    IrFactDomain::Occurrences | IrFactDomain::Relations
                )
            {
                record.status = CoverageStatus::Unknown;
                record.id =
                    derive_coverage_record_id(record).map_err(|_| FirstSliceError::Identity)?;
            }
            if let Some(count) = increments.get(&(file, record.domain)) {
                record.discovered = record
                    .discovered
                    .checked_add(*count)
                    .ok_or(FirstSliceError::Limits)?;
                record.indexed = record
                    .indexed
                    .checked_add(*count)
                    .ok_or(FirstSliceError::Limits)?;
                record.id =
                    derive_coverage_record_id(record).map_err(|_| FirstSliceError::Identity)?;
            }
        }
        Ok(())
    }
}

fn prepare_office(
    input: &ManifestInput,
    snapshot: &SourceSnapshot,
    repository: RepositoryId,
    budget: &mut Budget,
    prepared: &mut PreparedSources,
    snapshots: &mut Vec<SourceSnapshot>,
    cancellation: &Cancellation,
) -> Result<(), FirstSliceError> {
    if budget.files == 0 || budget.bytes == 0 || budget.work == 0 {
        return prepared.issue(
            input.file,
            "container expansion admission exhausted".to_owned(),
        );
    }
    let reserved = budget.reserve();
    let mut limits = office_package::OfficePackageLimits::default();
    limits.zip.expanded_bytes = reserved;
    limits.zip.entry_bytes = reserved;
    let package = match office_package::read_vba_project(snapshot.content(), limits, cancellation) {
        Ok(Some(package)) => package,
        Ok(None) => {
            budget.settle(reserved, reserved)?;
            return Ok(());
        }
        Err(error) => {
            check_cancellation(cancellation)?;
            return prepared.issue(input.file, format!("office ownership: {error}"));
        }
    };
    let used = [
        package.types_source(),
        package.package_relationships_source(),
        package.main_source(),
        package.main_relationships_source(),
        package.project_source(),
    ]
    .iter()
    .try_fold(0usize, |total, source| {
        total
            .checked_add(source.bytes().len())
            .ok_or(FirstSliceError::Limits)
    })?;
    budget.settle(reserved, used)?;
    let compound = match CompoundFile::parse(
        package.project_source().bytes(),
        CompoundLimits::default(),
        cancellation,
    ) {
        Ok(compound) => compound,
        Err(error) => {
            check_cancellation(cancellation)?;
            return prepared.issue(input.file, format!("compound project: {error}"));
        }
    };
    let reserved = budget.reserve();
    let mut limits = ProjectReadLimits::default();
    limits.compression.input_bytes = reserved / 2;
    limits.compression.output_bytes = reserved / 2;
    let rooted = match RootedProject::open(&compound, 0, limits, cancellation) {
        Ok(project) => project,
        Err(error) => {
            check_cancellation(cancellation)?;
            return prepared.issue(input.file, format!("vba project root: {error}"));
        }
    };
    let project = rooted.project();
    budget.settle(
        reserved,
        project
            .encoded_directory()
            .bytes()
            .len()
            .checked_add(project.decoded_directory().bytes().len())
            .ok_or(FirstSliceError::Limits)?,
    )?;
    let reserved = budget.reserve();
    let mut limits = MembershipLimits::default();
    limits.project.input_bytes = reserved;
    let membership = match ProjectMembership::read(project, limits, cancellation) {
        Ok(membership) => {
            budget.settle(reserved, membership.project_stream().bytes().len())?;
            Some(membership)
        }
        Err(error) => {
            check_cancellation(cancellation)?;
            prepared.issue(input.file, format!("vba membership: {error}"))?;
            None
        }
    };
    if let Some(membership) = &membership {
        if let Err(error) = membership.identity().name() {
            prepared.issue(input.file, format!("vba project identity: {error:?}"))?;
        }
        for (ordinal, record) in membership.project_records().iter().enumerate() {
            if let Err(error) = record.outcome() {
                prepared.issue(
                    input.file,
                    format!("vba project declaration {ordinal}: {error:?}"),
                )?;
            }
        }
    }
    // Keep the entire directory inventory before admitting any module bytes.
    // An unreadable or unadmitted sibling can compete with every surviving type.
    let mut native_project = rootlight_resolve::NativeClassicProject {
        container: input.file,
        content_hash: snapshot.content_hash(),
        name: membership
            .as_ref()
            .and_then(|m| m.identity().name().ok())
            .map(str::to_owned),
        members: Vec::new(),
        inventory_complete: membership.as_ref().is_some_and(|m| {
            m.project_records().iter().all(|r| r.outcome().is_ok())
                && m.directory_records().len() == project.modules().len()
        }),
    };
    native_project
        .members
        .try_reserve_exact(project.modules().len())
        .map_err(|_| FirstSliceError::Limits)?;
    for ordinal in 0..project.modules().len() {
        check_cancellation(cancellation)?;
        let record = membership
            .as_ref()
            .and_then(|m| m.directory_records().get(ordinal));
        let kind = record
            .and_then(|r| r.outcome().ok())
            .map(|m| match m.kind() {
                ProjectModuleKind::Class => rootlight_resolve::NativeClassicMemberKind::Class,
                ProjectModuleKind::Procedural => {
                    rootlight_resolve::NativeClassicMemberKind::Procedural
                }
                ProjectModuleKind::Document { .. } => {
                    rootlight_resolve::NativeClassicMemberKind::Document
                }
                ProjectModuleKind::Designer => rootlight_resolve::NativeClassicMemberKind::Designer,
            });
        native_project
            .members
            .push(rootlight_resolve::NativeClassicMember {
                file: None,
                name: record.and_then(|r| r.name()).map(str::to_owned),
                kind,
                predeclared_id: None,
            });
    }
    for (ordinal, descriptor) in project.modules().iter().enumerate() {
        check_cancellation(cancellation)?;
        if budget.files == 0 || budget.bytes == 0 || budget.work == 0 {
            prepared.issue(input.file, format!("vba expansion admission exhausted before module {ordinal}; remaining declarations: {}", project.modules().len() - ordinal))?;
            break;
        }
        let reserved = budget.reserve();
        let mut limits = ModuleReadLimits::default();
        limits.stream_bytes = reserved / 2;
        limits.compression.input_bytes = reserved / 2;
        limits.compression.output_bytes = reserved / 2;
        let module = match project.read_module(ordinal, limits, cancellation) {
            Ok(module) => module,
            Err(error) => {
                check_cancellation(cancellation)?;
                prepared.issue(input.file, format!("vba module {ordinal}: {error}"))?;
                continue;
            }
        };
        budget.settle(
            reserved,
            module
                .encoded_stream()
                .bytes()
                .len()
                .checked_add(module.bytes().len())
                .ok_or(FirstSliceError::Limits)?,
        )?;
        let reserved = budget.reserve();
        let mut limits = TextLimits::default();
        limits.output_bytes = reserved.min(budget.bytes);
        let text = match module.decode_text(limits, cancellation) {
            Ok(text) => text,
            Err(error) => {
                check_cancellation(cancellation)?;
                prepared.issue(input.file, format!("vba module {ordinal} text: {error}"))?;
                continue;
            }
        };
        budget.settle(reserved, text.text().len())?;
        let record = membership
            .as_ref()
            .and_then(|membership| membership.directory_records().get(ordinal));
        let matched = record.and_then(|record| record.outcome().ok());
        let kind = matched.map_or(ModuleKind::Procedural, |matched| match matched.kind() {
            ProjectModuleKind::Procedural => ModuleKind::Procedural,
            ProjectModuleKind::Class
            | ProjectModuleKind::Document { .. }
            | ProjectModuleKind::Designer => ModuleKind::Object,
        });
        let header = HeaderName::parse(&text, kind, HeaderLimits::default(), cancellation).ok();
        let verified = record
            .and_then(|record| {
                header
                    .as_ref()
                    .and_then(|header| record.check_source_name(header).ok())
            })
            .is_some_and(|matched| !matched.needs_non_ascii_case_validation());
        check_cancellation(cancellation)?;
        if !verified {
            prepared.issue(input.file, format!("vba module {ordinal} identity is not fully verified; decoded source retained without structural analysis"))?;
        }
        let stream_id = descriptor
            .stream_id()
            .map_err(|_| FirstSliceError::Identity)?;
        let stream = compound
            .entries()
            .find(|entry| entry.id() == stream_id)
            .ok_or(FirstSliceError::Identity)?;
        let path = match derived_path(&input.path, stream.name(), kind) {
            Ok(path) => path,
            Err(_) => {
                prepared.issue(
                    input.file,
                    format!("vba module {ordinal} derived path exceeds canonical path limits"),
                )?;
                continue;
            }
        };
        let file = derive_file(rootlight_ids::FileIdentity {
            repository,
            path_identity: path.identity_bytes(),
        })
        .id();
        let bytes = text.text().as_bytes();
        let hash = content_hash(bytes);
        let length = bytes.len();
        let mut retained = Vec::new();
        retained
            .try_reserve_exact(length)
            .map_err(|_| FirstSliceError::Limits)?;
        retained.extend_from_slice(bytes);
        let snapshot =
            SourceSnapshot::from_persisted(repository, path.clone(), file, hash, retained)
                .map_err(|_| FirstSliceError::Identity)?;
        prepared
            .inputs
            .try_reserve(1)
            .map_err(|_| FirstSliceError::Limits)?;
        prepared
            .origins
            .try_reserve(1)
            .map_err(|_| FirstSliceError::Limits)?;
        prepared
            .fingerprints
            .try_reserve(2)
            .map_err(|_| FirstSliceError::Limits)?;
        snapshots
            .try_reserve(1)
            .map_err(|_| FirstSliceError::Limits)?;
        prepared.fingerprints.extend([
            InputFingerprint::new(InputKey::FileContent(file), hash),
            InputFingerprint::new(
                InputKey::FilePath(file),
                content_hash(path.identity_bytes()),
            ),
        ]);
        prepared.inputs.push(ManifestInput {
            file,
            path: path.as_str().to_owned(),
            content_hash: hash,
            bytes: u64::try_from(length).map_err(|_| FirstSliceError::Limits)?,
            encoding: "utf-8".to_owned(),
            class: input.class,
            language_signals: vec![LanguageSignal {
                language: if verified { "vba" } else { "vba-unverified" }.to_owned(),
                evidence: LanguageEvidence::Content,
            }],
            decisive_rule: None,
        });
        prepared.origins.push((file, input.file));
        let member = native_project
            .members
            .get_mut(ordinal)
            .ok_or(FirstSliceError::Identity)?;
        member.file = Some(file);
        if verified {
            member.predeclared_id = header
                .as_ref()
                .and_then(HeaderName::predeclared_id)
                .filter(|attribute| attribute.value())
                .map(|attribute| attribute.utf8_range());
        }
        native_project.inventory_complete &= verified;
        if verified
            && matched.is_some_and(|m| {
                matches!(
                    m.kind(),
                    ProjectModuleKind::Document { .. } | ProjectModuleKind::Designer
                )
            })
        {
            prepared.incomplete_native_contexts.insert(file);
            prepared.issue(file, "vba extension context incomplete: project resolution includes explicit source declarations; host-defined members and designer controls are not established".to_owned())?;
        }
        if verified {
            prepared.native_vba_modules.insert(
                file,
                match kind {
                    ModuleKind::Procedural => {
                        rootlight_adapter_sdk::NativeVbaModuleKind::Procedural
                    }
                    ModuleKind::Object => rootlight_adapter_sdk::NativeVbaModuleKind::Object,
                },
            );
        }
        snapshots.push(snapshot);
        budget.bytes = budget
            .bytes
            .checked_sub(length)
            .ok_or(FirstSliceError::Limits)?;
        budget.files -= 1;
    }
    prepared
        .native_projects
        .try_reserve(1)
        .map_err(|_| FirstSliceError::Limits)?;
    prepared.native_projects.push(native_project);
    references::prepare(
        input,
        project,
        repository,
        budget,
        prepared,
        snapshots,
        cancellation,
    )?;
    Ok(())
}

fn derived_path(
    container: &str,
    stream: &str,
    kind: ModuleKind,
) -> Result<RelativePath, FirstSliceError> {
    // A physical regular file cannot be a directory ancestor. Encoding every
    // byte makes traversal, reserved names and Unicode normalization irrelevant.
    let mut path = String::new();
    let length = stream
        .len()
        .checked_mul(2)
        .and_then(|n| n.checked_add(container.len()))
        .and_then(|n| n.checked_add(11))
        .ok_or(FirstSliceError::Limits)?;
    path.try_reserve_exact(length)
        .map_err(|_| FirstSliceError::Limits)?;
    path.push_str(container);
    path.push_str("/@vba/");
    use std::fmt::Write as _;
    for byte in stream.bytes() {
        write!(path, "{byte:02x}").map_err(|_| FirstSliceError::Limits)?;
    }
    path.push_str(if kind == ModuleKind::Procedural {
        ".bas"
    } else {
        ".cls"
    });
    RelativePath::parse(Path::new(&path)).map_err(|_| FirstSliceError::Identity)
}
