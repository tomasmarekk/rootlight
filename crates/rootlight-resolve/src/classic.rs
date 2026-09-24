//! Project-owned classic BASIC types and members over immutable source inputs.
//! Declarative membership constrains native type occurrences; directory proximity
//! and matching spellings alone never establish a project or a target.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, SymbolId, content_hash};
use rootlight_ir::{
    Confidence, ContainerRef, CoverageRecord, CoverageScope, CoverageStatus, EntityKind,
    EntityRecord, EntityVisibility, FactDomain, FactRef, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, ProducerKind, SourceRef,
};

use crate::{
    CandidateExplanation, CompletenessAssumption, RESOLVER_PROVIDER_NAME,
    RESOLVER_PROVIDER_VERSION, ResolutionDecision, ResolutionError, ResolutionExplanation,
    ResolutionOutcome, ResolutionRule, ResolutionSignal,
    engine::{CandidateIndex, ResolutionWorkBudget},
};

const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_MANIFEST_ITEMS: usize = 8192;
const MAX_PATH_BYTES: usize = 4096;

/// Immutable bytes associated with one already indexed file.
///
/// The resolver verifies identity against the generation's file record before
/// reading any declaration or project membership. No filesystem access occurs.
#[derive(Debug, Clone, Copy)]
pub struct ClassicProjectSource<'a> {
    file: FileId,
    bytes: &'a [u8],
}

impl<'a> ClassicProjectSource<'a> {
    /// Associates retained bytes with a file; resolution checks their hash and length.
    #[must_use]
    pub const fn new(file: FileId, bytes: &'a [u8]) -> Self {
        Self { file, bytes }
    }
}

/// Host-supplied native project inventory bound to one immutable container.
///
/// The host must reconcile both metadata inventories and source headers from
/// this exact container. This is an analysis input, not an untrusted wire format.
/// Missing or unsupported declarations must remain represented, never filtered.
#[derive(Debug, Clone)]
pub struct NativeClassicProject {
    /// Indexed container that owns every member, independent of project spelling.
    pub container: FileId,
    /// Hash of the original bytes used to verify the inventory.
    pub content_hash: ContentHash,
    /// Reconciled project name, absent when either metadata identity is invalid.
    pub name: Option<String>,
    /// All directory declarations, including modules not admitted for analysis.
    pub members: Vec<NativeClassicMember>,
    /// Both metadata inventories and all retained source headers were reconciled.
    /// Missing member fields independently prevent resolution even when this is true.
    pub inventory_complete: bool,
}

/// One native declaration; absent fields are barriers to project-wide uniqueness.
#[derive(Debug, Clone)]
pub struct NativeClassicMember {
    /// Exact retained file identity, absent when extraction or admission failed.
    pub file: Option<FileId>,
    /// Decoded metadata spelling; the resolver still checks its syntax and entity.
    pub name: Option<String>,
    /// Source module category; extension categories do not supply host-defined members.
    pub kind: Option<NativeClassicMemberKind>,
    /// UTF-8 range of the effective True VB_PredeclaredId literal in verified source.
    /// None denotes the False default or an explicit False; only class members may set it.
    pub predeclared_id: Option<std::ops::Range<usize>>,
}

/// Native categories whose project lookup matches the classic BASIC resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeClassicMemberKind {
    /// An ordinary class, not a host document or designer object.
    Class,
    /// A standard procedural module.
    Procedural,
    /// Source declarations extending a host document class.
    Document,
    /// Source declarations extending a designer class, not its generated controls.
    Designer,
}

#[derive(Default)]
pub(crate) struct ClassicIndex<'a> {
    proofs: BTreeMap<FactId, Proof<'a>>,
}

pub(crate) struct Proof<'a> {
    pub(crate) target: &'a EntityRecord,
    pub(crate) context: Arc<ProjectEvidence>,
    pub(crate) receiver: Option<(&'a EntityRecord, SourceRef)>,
}

pub(crate) struct ProjectEvidence {
    pub(crate) manifest: FileId,
    pub(crate) native: bool,
    pub(crate) sources: Vec<SourceRef>,
    pub(crate) facts: Vec<FactRef>,
}

struct Member {
    name: String,
    location: MemberLocation,
    class: bool,
    predeclared_id: Option<std::ops::Range<usize>>,
}

struct ResolvedProject {
    types: BTreeMap<String, Option<SymbolId>>,
    modules: BTreeMap<String, Option<SymbolId>>,
    default_instances: BTreeMap<SymbolId, SourceRef>,
    value_names: BTreeSet<String>,
    evidence: Arc<ProjectEvidence>,
}

enum MemberLocation {
    Path(String),
    Indexed(Option<FileId>),
}

struct Project {
    name: String,
    members: Vec<Member>,
    complete: bool,
}

fn is_manifest(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("vbp"))
}

fn identifier(text: &str) -> Option<String> {
    let text = text
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(text);
    let mut bytes = text.bytes();
    if !bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || text.len() > MAX_PATH_BYTES
    {
        return None;
    }
    Some(text.to_ascii_lowercase())
}

fn member_path(manifest: &str, value: &str) -> Option<String> {
    if value.len() > MAX_PATH_BYTES || value.contains([':', '\0']) {
        return None;
    }
    let value = value.replace('\\', "/");
    if value.starts_with('/') {
        return None;
    }
    let mut parts = manifest
        .rsplit_once('/')
        .map_or(Vec::new(), |(base, _)| base.split('/').collect());
    for part in value.split('/') {
        match part {
            "" => return None,
            "." => {}
            ".." => {
                parts.pop()?;
            }
            part if part.ends_with([' ', '.']) => return None,
            part => parts.push(part),
        }
    }
    let path = parts.join("/");
    (path.len() <= MAX_PATH_BYTES).then(|| path.to_ascii_lowercase())
}

fn unconditional_udt(
    ty: &EntityRecord,
    sources: &BTreeMap<FileId, &[u8]>,
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<bool, ResolutionError> {
    let source = ty
        .evidence
        .source
        .as_ref()
        .ok_or(ResolutionError::ClassicSourceIdentity)?;
    let span = source.span();
    let start = usize::try_from(span.start_byte()).map_err(|_| ResolutionError::CountOverflow)?;
    let end = usize::try_from(span.end_byte()).map_err(|_| ResolutionError::CountOverflow)?;
    let bytes = sources
        .get(&span.file())
        .and_then(|bytes| bytes.get(start..end))
        .ok_or(ResolutionError::ClassicSourceIdentity)?;
    // Without a configured preprocessor result, one conditional field makes
    // the entire UDT field environment unsafe to select across files.
    for line in bytes.split(|byte| matches!(byte, b'\r' | b'\n')) {
        cancellation.check()?;
        work.consume_many(line.len().div_ceil(64).max(1))?;
        if line.trim_ascii_start().starts_with(b"#") {
            return Ok(false);
        }
    }
    Ok(true)
}

fn parse_project(
    path: &str,
    bytes: &[u8],
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<Option<Project>, ResolutionError> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Ok(None);
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Ok(None);
    };
    let mut members = Vec::new();
    let mut names = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut project_name = None;
    let mut project_type = false;
    for (index, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        cancellation.check()?;
        work.consume()?;
        if index >= MAX_MANIFEST_ITEMS {
            return Ok(None);
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Ok(None);
        };
        let key = key.trim();
        let value = value.trim();
        if key.eq_ignore_ascii_case("Name") {
            let name = value
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .and_then(identifier);
            if project_name.is_some() || name.is_none() {
                return Ok(None);
            }
            project_name = name;
        } else if key.eq_ignore_ascii_case("Type") {
            if project_type
                || !["Exe", "OleDll", "OleExe", "Control"]
                    .iter()
                    .any(|kind| value.eq_ignore_ascii_case(kind))
            {
                return Ok(None);
            }
            project_type = true;
        } else if key.eq_ignore_ascii_case("Class") || key.eq_ignore_ascii_case("Module") {
            let Some((name, member)) = value.split_once(';') else {
                return Ok(None);
            };
            let Some(name) = identifier(name.trim()) else {
                return Ok(None);
            };
            let Some(path) = member_path(path, member.trim()) else {
                return Ok(None);
            };
            if !names.insert(name.clone()) || !paths.insert(path.clone()) {
                return Ok(None);
            }
            members.push(Member {
                name,
                location: MemberLocation::Path(path),
                class: key.eq_ignore_ascii_case("Class"),
                predeclared_id: None,
            });
        } else if [
            "Form",
            "UserControl",
            "UserDocument",
            "Designer",
            "RelatedDoc",
        ]
        .iter()
        .any(|kind| key.eq_ignore_ascii_case(kind))
        {
            // These entries can introduce competing native declarations. An
            // incomplete member inventory cannot justify unique type selection.
            return Ok(None);
        }
    }
    Ok(project_name
        .filter(|_| project_type && !members.is_empty())
        .map(|name| Project {
            name,
            members,
            complete: true,
        }))
}

impl<'a> ClassicIndex<'a> {
    pub(crate) fn build(
        index: &CandidateIndex<'a>,
        coverage: &[CoverageRecord],
        occurrences: &[OccurrenceRecord],
        inputs: &[ClassicProjectSource<'_>],
        native: &[NativeClassicProject],
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<Self, ResolutionError> {
        let files = &index.files;
        let entities = &index.entities;
        let provenance = &index.provenance;
        let mut result = Self::default();
        if native.is_empty() && !files.values().any(|file| is_manifest(&file.path)) {
            return Ok(result);
        }
        let mut sources = BTreeMap::new();
        for input in inputs {
            cancellation.check()?;
            work.consume()?;
            let Some(file) = files.get(&input.file) else {
                return Err(ResolutionError::ClassicSourceIdentity);
            };
            if !matches!(file.language.as_str(), "vba" | "visual-basic-classic")
                && !is_manifest(&file.path)
            {
                continue;
            }
            if u64::try_from(input.bytes.len()).ok() != Some(file.byte_length)
                || content_hash(input.bytes) != file.content_hash
                || sources.insert(input.file, input.bytes).is_some()
            {
                return Err(ResolutionError::ClassicSourceIdentity);
            }
        }
        let mut paths = BTreeMap::new();
        for file in files.values() {
            cancellation.check()?;
            work.consume()?;
            paths
                .entry(file.path.to_ascii_lowercase())
                .and_modify(|old| *old = None)
                .or_insert(Some(*file));
        }
        let mut domains = BTreeMap::new();
        for record in coverage {
            cancellation.check()?;
            work.consume()?;
            if let CoverageScope::File(file) = record.scope {
                domains
                    .entry((file, record.domain))
                    .and_modify(|old| *old = None)
                    .or_insert(Some(record));
            }
        }
        let mut declarations: BTreeMap<FileId, Vec<&EntityRecord>> = BTreeMap::new();
        let mut local_types = BTreeMap::new();
        for entity in entities.values() {
            cancellation.check()?;
            work.consume()?;
            if let Some(source) = &entity.evidence.source {
                declarations
                    .entry(source.span().file())
                    .or_default()
                    .push(entity);
                if matches!(entity.kind, EntityKind::Struct | EntityKind::Enum) {
                    local_types
                        .entry((
                            source.span().file(),
                            entity.canonical_name.to_ascii_lowercase(),
                        ))
                        .and_modify(|unique| *unique = false)
                        .or_insert(true);
                }
            }
        }
        let mut memberships = BTreeMap::new();
        let mut projects = Vec::new();
        let mut inventories = Vec::new();
        for manifest in files.values().filter(|file| is_manifest(&file.path)) {
            cancellation.check()?;
            let Some(bytes) = sources.get(&manifest.id) else {
                return Ok(result);
            };
            let Some(project) = parse_project(&manifest.path, bytes, cancellation, work)? else {
                return Ok(result);
            };
            inventories.push((*manifest, project, false));
        }
        let mut native_containers = BTreeSet::new();
        for project in native {
            cancellation.check()?;
            work.consume()?;
            if !native_containers.insert(project.container) {
                return Err(ResolutionError::ClassicSourceIdentity);
            }
            let manifest = files
                .get(&project.container)
                .filter(|file| {
                    file.content_hash == project.content_hash && file.language == "office-open-xml"
                })
                .ok_or(ResolutionError::ClassicSourceIdentity)?;
            if project.members.len() > MAX_MANIFEST_ITEMS {
                return Ok(result);
            }
            let name = project.name.as_deref().and_then(identifier);
            let mut complete =
                project.inventory_complete && name.is_some() && !project.members.is_empty();
            let mut members = Vec::new();
            for member in &project.members {
                cancellation.check()?;
                work.consume()?;
                let name = member.name.as_deref().and_then(identifier);
                complete &= name.is_some() && member.file.is_some() && member.kind.is_some();
                members.push(Member {
                    name: name.unwrap_or_default(),
                    location: MemberLocation::Indexed(member.file),
                    predeclared_id: member.predeclared_id.clone(),
                    // Extension source declarations belong to their named class
                    // (MS-VBAL 4.2.1/5.2.4). Host members are not inferred here.
                    class: matches!(
                        member.kind,
                        Some(
                            NativeClassicMemberKind::Class
                                | NativeClassicMemberKind::Document
                                | NativeClassicMemberKind::Designer
                        )
                    ),
                });
            }
            inventories.push((
                *manifest,
                Project {
                    name: name.unwrap_or_default(),
                    members,
                    complete,
                },
                true,
            ));
        }
        let member_file = |member: &Member| match &member.location {
            MemberLocation::Path(path) => paths.get(path).copied().flatten(),
            MemberLocation::Indexed(file) => file.and_then(|file| files.get(&file).copied()),
        };
        for (manifest, project, native) in inventories {
            cancellation.check()?;
            let members = project.members;
            // Project inventories can overlap. Admit their retained evidence
            // before allocation rather than relying on later ambiguity pruning.
            work.consume_many(4)?;
            let mut evidence = ProjectEvidence {
                manifest: manifest.id,
                native,
                sources: Vec::new(),
                facts: vec![FactRef::File(manifest.id)],
            };
            let Some(source) = &manifest.evidence.source else {
                continue;
            };
            evidence.sources.push(source.clone());
            let mut types = BTreeMap::from([(project.name.clone(), None)]);
            let mut modules = BTreeMap::from([(project.name, None)]);
            let mut default_instances = BTreeMap::new();
            let mut value_names = BTreeSet::new();
            work.consume_many(members.len())?;
            let member_files: Vec<_> = members
                .iter()
                .filter_map(|member| member_file(member).map(|file| file.id))
                .collect();
            let mut complete = project.complete;
            for member in members {
                cancellation.check()?;
                work.consume_many(5)?;
                let Some(file) = member_file(&member) else {
                    complete = false;
                    break;
                };
                if !matches!(file.language.as_str(), "vba" | "visual-basic-classic")
                    || !sources.contains_key(&file.id)
                    || !provenance.get(&file.provenance).is_some_and(|p| {
                        p.producer_kind == ProducerKind::Parser && p.language == file.language
                    })
                {
                    complete = false;
                    break;
                }
                let records = declarations
                    .get(&file.id)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                work.consume_many(
                    records
                        .len()
                        .checked_mul(2)
                        .ok_or(ResolutionError::CountOverflow)?,
                )?;
                for domain in [FactDomain::Files, FactDomain::Entities] {
                    let valid = domains
                        .get(&(file.id, domain))
                        .copied()
                        .flatten()
                        .filter(|r| {
                            r.status == CoverageStatus::Complete
                                && r.skipped == 0
                                && r.discovered == r.indexed
                                && r.provenance == file.provenance
                                && r.evidence.source == file.evidence.source
                        });
                    let Some(record) = valid else {
                        complete = false;
                        break;
                    };
                    let expected = if domain == FactDomain::Files {
                        1
                    } else {
                        u64::try_from(records.len()).map_err(|_| ResolutionError::CountOverflow)?
                    };
                    if record.indexed != expected {
                        complete = false;
                        break;
                    }
                    evidence.facts.push(FactRef::Fact(record.id));
                }
                if !complete {
                    break;
                }
                let named: Vec<_> = records
                    .iter()
                    .filter(|e| {
                        matches!(e.kind, EntityKind::Class | EntityKind::Module)
                            && e.canonical_name.eq_ignore_ascii_case(&member.name)
                    })
                    .collect();
                if named.len() != 1 || member.class != (named[0].kind == EntityKind::Class) {
                    complete = false;
                    break;
                }
                if let Some(range) = member.predeclared_id {
                    work.consume()?;
                    let literal = sources
                        .get(&file.id)
                        .and_then(|bytes| bytes.get(range.clone()));
                    if !member.class
                        || !literal.is_some_and(|text| text.eq_ignore_ascii_case(b"True"))
                    {
                        return Err(ResolutionError::ClassicSourceIdentity);
                    }
                    let source = file
                        .evidence
                        .source
                        .as_ref()
                        .ok_or(ResolutionError::ClassicSourceIdentity)?;
                    let span = rootlight_ir::SourceSpan::new(
                        file.id,
                        u64::try_from(range.start).map_err(|_| ResolutionError::CountOverflow)?,
                        u64::try_from(range.end).map_err(|_| ResolutionError::CountOverflow)?,
                    )
                    .map_err(ResolutionError::InvalidProducer)?;
                    default_instances.insert(
                        named[0].id,
                        SourceRef::new(
                            source.repository(),
                            source.generation(),
                            span,
                            source.content_hash(),
                            None,
                        ),
                    );
                }
                if !member.class {
                    for entity in records {
                        cancellation.check()?;
                        work.consume()?;
                        let namespace = if let Some(ContainerRef::Entity(owner)) = entity.container
                            && let Some(owner) =
                                entities.get(&owner).filter(|e| e.kind == EntityKind::Enum)
                        {
                            (owner.visibility != EntityVisibility::Private)
                                .then_some(owner.container)
                                .flatten()
                        } else {
                            entity.container
                        };
                        if namespace == Some(ContainerRef::Entity(named[0].id))
                            && entity.visibility != EntityVisibility::Private
                        {
                            value_names.insert(entity.canonical_name.to_ascii_lowercase());
                        }
                    }
                }
                // Public UDT/enum names cannot collide with any module or the
                // project name. Keep barriers sticky across manifest order.
                work.consume()?;
                modules
                    .entry(member.name.clone())
                    .and_modify(|prior| *prior = None)
                    .or_insert((!member.class).then_some(named[0].id));
                types
                    .entry(member.name)
                    .and_modify(|prior| *prior = None)
                    .or_insert(member.class.then_some(named[0].id));
                for entity in records
                    .iter()
                    .filter(|entity| matches!(entity.kind, EntityKind::Struct | EntityKind::Enum))
                {
                    if entity.visibility == EntityVisibility::Private {
                        continue;
                    }
                    // A private sibling module does not compete with an export,
                    // but duplicate types inside the exporting module do.
                    let name = entity.canonical_name.to_ascii_lowercase();
                    let target = (entity.visibility == EntityVisibility::Public
                        && entity.container == Some(ContainerRef::Entity(named[0].id))
                        && local_types.get(&(file.id, name.clone())) == Some(&true))
                    .then_some(entity.id);
                    work.consume()?;
                    modules
                        .entry(name.clone())
                        .and_modify(|prior| *prior = None)
                        .or_insert(None);
                    types
                        .entry(name)
                        .and_modify(|prior| *prior = None)
                        .or_insert(target);
                }
                evidence.facts.push(FactRef::File(file.id));
                if let Some(source) = &file.evidence.source {
                    evidence.sources.push(source.clone());
                }
            }
            // A source listed by multiple projects has multiple compilation
            // contexts. Do not silently pick the first project in path order.
            for file in &member_files {
                memberships
                    .entry(*file)
                    .and_modify(|old| *old = None)
                    .or_insert(Some(projects.len()));
            }
            evidence.sources.sort_unstable();
            evidence.sources.dedup();
            evidence.facts.sort_unstable();
            evidence.facts.dedup();
            projects.push(complete.then_some(ResolvedProject {
                types,
                modules,
                default_instances,
                value_names,
                evidence: Arc::new(evidence),
            }));
        }
        for occurrence in occurrences {
            cancellation.check()?;
            work.consume()?;
            if occurrence.role != OccurrenceRole::TypeUse
                || !matches!(
                    occurrence.syntax_kind.as_str(),
                    "vba.direct_type_identifier.reference"
                        | "vba.type_identifier.reference"
                        | "vba.automatic_type_identifier.reference"
                )
                || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            {
                continue;
            }
            // Generic type captures also occur on array parameters; only a
            // callable return declaration proves this scalar result type.
            if occurrence.syntax_kind == "vba.type_identifier.reference"
                && !occurrence
                    .enclosing
                    .and_then(|id| entities.get(&id))
                    .is_some_and(|owner| {
                        matches!(
                            owner.kind,
                            EntityKind::Function | EntityKind::Method | EntityKind::Property
                        )
                    })
            {
                continue;
            }
            let Some(project) = memberships
                .get(&occurrence.file)
                .copied()
                .flatten()
                .and_then(|i| projects.get(i))
                .and_then(Option::as_ref)
            else {
                continue;
            };
            let Some(bytes) = sources.get(&occurrence.file) else {
                continue;
            };
            let span = occurrence.source.span();
            let start =
                usize::try_from(span.start_byte()).map_err(|_| ResolutionError::CountOverflow)?;
            let end =
                usize::try_from(span.end_byte()).map_err(|_| ResolutionError::CountOverflow)?;
            let Some(name) = bytes
                .get(start..end)
                .and_then(|s| std::str::from_utf8(s).ok())
                .and_then(identifier)
            else {
                continue;
            };
            // Local UDT/enum type declarations have their own lookup tier, even
            // when native analysis could not safely select one of them.
            if local_types.contains_key(&(occurrence.file, name.clone())) {
                continue;
            }
            let Some(target) = project
                .types
                .get(&name)
                .copied()
                .flatten()
                .and_then(|id| entities.get(&id))
                .copied()
            else {
                continue;
            };
            // As New requires a named class (MS-VBAL 5.2.3.1.4), not a
            // UDT/enum that happens to occupy the same type lookup tier.
            if occurrence.syntax_kind == "vba.automatic_type_identifier.reference"
                && target.kind != EntityKind::Class
            {
                continue;
            }
            result.proofs.insert(
                occurrence.id,
                Proof {
                    target,
                    context: Arc::clone(&project.evidence),
                    receiver: None,
                },
            );
        }
        // A project-proven UDT type owns its declared fields, while a property
        // getter's result type can establish the next receiver in a chain.
        // Enum values do not establish an instance member environment.
        let mut value_types = BTreeMap::new();
        let mut receivers = BTreeMap::new();
        let mut members = BTreeMap::new();
        let mut parameterized_properties = BTreeSet::new();
        let mut field_definitions = BTreeMap::new();
        let mut unconditional_udts = BTreeMap::new();
        for entity in entities.values() {
            cancellation.check()?;
            work.consume()?;
            if entity.kind == EntityKind::Parameter
                && let Some(ContainerRef::Entity(owner)) = entity.container
                && entities
                    .get(&owner)
                    .is_some_and(|owner| owner.kind == EntityKind::Property)
            {
                parameterized_properties.insert(owner);
            }
            if let Some(ContainerRef::Entity(owner)) = entity.container {
                members
                    .entry((owner, entity.canonical_name.to_ascii_lowercase()))
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(*entity));
                // Enum constants also occupy the enclosing module namespace
                // (MS-VBAL 5.6.10), including private or ambiguous local enums.
                if let Some(enumeration) =
                    entities.get(&owner).filter(|e| e.kind == EntityKind::Enum)
                    && let Some(ContainerRef::Entity(module)) = enumeration.container
                {
                    work.consume()?;
                    members
                        .entry((module, entity.canonical_name.to_ascii_lowercase()))
                        .and_modify(|prior| *prior = None)
                        .or_insert(Some(*entity));
                }
            }
        }
        for occurrence in occurrences {
            cancellation.check()?;
            work.consume()?;
            if occurrence.role == OccurrenceRole::Definition
                && let OccurrenceTarget::Resolved { symbol } = occurrence.target
                && entities
                    .get(&symbol)
                    .is_some_and(|entity| entity.kind == EntityKind::Field)
            {
                work.consume()?;
                field_definitions
                    .entry(symbol)
                    .and_modify(|source: &mut Option<SourceRef>| {
                        if source.as_ref() != Some(&occurrence.source) {
                            *source = None;
                        }
                    })
                    .or_insert_with(|| Some(occurrence.source.clone()));
            }
            if let Some(owner) = occurrence
                .enclosing
                .and_then(|id| entities.get(&id))
                .copied()
                && matches!(
                    owner.kind,
                    EntityKind::Parameter
                        | EntityKind::Variable
                        | EntityKind::Field
                        | EntityKind::Property
                )
            {
                let typed = result
                    .proof(occurrence)
                    .map(|proof| (proof.target, Arc::clone(&proof.context)))
                    .or_else(|| {
                        // A same-module UDT type can already be resolved by
                        // the native pass. VBA requires its declaration to
                        // precede the field; cross-file member chaining also
                        // requires the unique complete project declaration.
                        let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
                            return None;
                        };
                        let target = entities.get(&symbol).copied()?;
                        let project = memberships
                            .get(&occurrence.file)
                            .copied()
                            .flatten()
                            .and_then(|i| projects.get(i))
                            .and_then(Option::as_ref)?;
                        (owner.kind == EntityKind::Field
                            && occurrence.role == OccurrenceRole::TypeUse
                            && occurrence.syntax_kind == "vba.direct_type_identifier.reference"
                            && target.kind == EntityKind::Struct
                            && owner
                                .evidence
                                .source
                                .as_ref()
                                .zip(target.evidence.source.as_ref())
                                .is_some_and(|(field, declaration)| {
                                    field.span().file() == occurrence.file
                                        && declaration.span().file() == occurrence.file
                                        && declaration.span().end_byte()
                                            <= field.span().start_byte()
                                })
                            && project
                                .types
                                .get(&target.canonical_name.to_ascii_lowercase())
                                == Some(&Some(target.id)))
                        .then(|| (target, Arc::clone(&project.evidence)))
                    });
                if let Some((target, context)) = typed.filter(|(target, _)| {
                    matches!(target.kind, EntityKind::Class | EntityKind::Struct)
                }) {
                    let safe = if target.kind == EntityKind::Struct {
                        if let Some(&safe) = unconditional_udts.get(&target.id) {
                            safe
                        } else {
                            let safe = unconditional_udt(target, &sources, cancellation, work)?;
                            unconditional_udts.insert(target.id, safe);
                            safe
                        }
                    } else {
                        true
                    };
                    if safe {
                        value_types
                            .entry(owner.id)
                            .and_modify(|prior| *prior = None)
                            .or_insert(Some((
                                owner,
                                target.id,
                                context,
                                occurrence.source.clone(),
                            )));
                    }
                }
            }
            if occurrence.syntax_kind == "vba.value_identifier.reference"
                && occurrence.role == OccurrenceRole::Reference
                && let OccurrenceTarget::Resolved { symbol } = occurrence.target
            {
                receivers
                    .entry(occurrence.source.span())
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(symbol));
            }
        }
        let mut member_occurrences = Vec::new();
        for occurrence in occurrences {
            cancellation.check()?;
            if matches!(
                occurrence.syntax_kind.as_str(),
                "vba.member.reference" | "vba.invoked_member.reference"
            ) && occurrence.role == OccurrenceRole::Reference
                && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            {
                member_occurrences.push(occurrence);
            } else {
                // Keep the original per-occurrence work charge even when a
                // record cannot participate in project member resolution.
                work.consume()?;
            }
        }
        // Inner member expressions have shorter source spans than their
        // enclosing chains, independent of the normalized record order.
        member_occurrences.sort_unstable_by_key(|occurrence| {
            let span = occurrence.source.span();
            (span.end_byte() - span.start_byte(), span)
        });
        let mut resolved_members = BTreeMap::new();
        for occurrence in member_occurrences {
            cancellation.check()?;
            work.consume()?;
            let span = occurrence.source.span();
            let start =
                usize::try_from(span.start_byte()).map_err(|_| ResolutionError::CountOverflow)?;
            let end =
                usize::try_from(span.end_byte()).map_err(|_| ResolutionError::CountOverflow)?;
            let Some(text) = sources
                .get(&occurrence.file)
                .and_then(|bytes| bytes.get(start..end))
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
            else {
                continue;
            };
            let Some((receiver, member)) = text.rsplit_once('.') else {
                continue;
            };
            let Some(name) = identifier(member) else {
                continue;
            };
            let receiver_end = span
                .start_byte()
                .checked_add(
                    u64::try_from(receiver.len()).map_err(|_| ResolutionError::CountOverflow)?,
                )
                .ok_or(ResolutionError::CountOverflow)?;
            let receiver_span =
                rootlight_ir::SourceSpan::new(occurrence.file, span.start_byte(), receiver_end)
                    .map_err(ResolutionError::InvalidProducer)?;
            let binding = if let Some(value) = receivers.get(&receiver_span) {
                value
                    .and_then(|value| value_types.get(&value))
                    .and_then(Option::as_ref)
                    .map(|(owner, class, context, source)| {
                        (*owner, *class, Arc::clone(context), source.clone())
                    })
            } else if receiver.contains('.') {
                // A qualified receiver requires the shorter exact expression:
                // a declared UDT field or a project-typed property result.
                resolved_members
                    .get(&receiver_span)
                    .and_then(|id: &Option<FactId>| *id)
                    .and_then(|id| result.proofs.get(&id))
                    .filter(|proof| {
                        matches!(proof.target.kind, EntityKind::Field | EntityKind::Property)
                    })
                    .and_then(|proof| value_types.get(&proof.target.id))
                    .and_then(Option::as_ref)
                    .filter(|(owner, class, _, _)| {
                        entities.get(class).is_some_and(|entity| {
                            matches!(
                                (owner.kind, entity.kind),
                                (EntityKind::Field, EntityKind::Struct)
                                    | (
                                        EntityKind::Property,
                                        EntityKind::Class | EntityKind::Struct
                                    )
                            )
                        })
                    })
                    .map(|(owner, class, context, source)| {
                        (*owner, *class, Arc::clone(context), source.clone())
                    })
            } else {
                let Some(receiver_name) = identifier(receiver) else {
                    continue;
                };
                let Some(project) = memberships
                    .get(&occurrence.file)
                    .copied()
                    .flatten()
                    .and_then(|i| projects.get(i))
                    .and_then(Option::as_ref)
                else {
                    continue;
                };
                // Explicit lexical or exported values outrank both named default
                // instances and project module qualification. An uncertain local
                // binding must not expose an unrelated project-wide spelling.
                if project.value_names.contains(&receiver_name) {
                    continue;
                }
                let mut scope = occurrence.enclosing;
                let mut unshadowed = false;
                for _ in 0..entities.len() {
                    cancellation.check()?;
                    work.consume()?;
                    let Some(owner) = scope.and_then(|id| entities.get(&id)) else {
                        break;
                    };
                    if members.contains_key(&(owner.id, receiver_name.clone())) {
                        break;
                    }
                    match owner.container {
                        Some(ContainerRef::Entity(parent)) => scope = Some(parent),
                        Some(ContainerRef::File(file)) if file == occurrence.file => {
                            unshadowed = true;
                            break;
                        }
                        _ => break,
                    }
                }
                if !unshadowed {
                    continue;
                }
                project
                    .types
                    .get(&receiver_name)
                    .copied()
                    .flatten()
                    .and_then(|id| {
                        project
                            .default_instances
                            .get(&id)
                            .map(|source| (id, source))
                    })
                    .and_then(|(id, source)| {
                        entities.get(&id).map(|owner| {
                            (*owner, id, Arc::clone(&project.evidence), source.clone())
                        })
                    })
                    .or_else(|| {
                        // Standard modules have no instance. Their verified
                        // project declaration itself owns the qualified member.
                        project
                            .modules
                            .get(&receiver_name)
                            .copied()
                            .flatten()
                            .and_then(|id| entities.get(&id).copied())
                            .and_then(|owner| {
                                owner.evidence.source.as_ref().map(|source| {
                                    (
                                        owner,
                                        owner.id,
                                        Arc::clone(&project.evidence),
                                        source.clone(),
                                    )
                                })
                            })
                    })
            };
            let Some((owner, class, context, type_source)) = binding else {
                continue;
            };
            let Some(target) = members.get(&(class, name)).copied().flatten() else {
                continue;
            };
            let Some(receiver_type) = entities.get(&class).copied() else {
                continue;
            };
            let accessible = if receiver_type.kind == EntityKind::Struct {
                // Type fields have no independent access modifier in VBA. The
                // project-proven public UDT and exact field owner supply access.
                target.kind == EntityKind::Field
                    && target.container == Some(ContainerRef::Entity(class))
                    && target
                        .evidence
                        .source
                        .as_ref()
                        .zip(receiver_type.evidence.source.as_ref())
                        .is_some_and(|(field, ty)| field.span().file() == ty.span().file())
                    && field_definitions
                        .get(&target.id)
                        .and_then(Option::as_ref)
                        .is_some_and(|definition| {
                            target.evidence.source.as_ref().is_some_and(|field| {
                                definition.span().file() == field.span().file()
                                    && definition.span().start_byte() >= field.span().start_byte()
                                    && definition.span().end_byte() <= field.span().end_byte()
                            })
                        })
            } else {
                matches!(
                    target.visibility,
                    EntityVisibility::Public | EntityVisibility::Restricted
                ) && match owner.kind {
                    EntityKind::Module => matches!(
                        target.kind,
                        EntityKind::Function
                            | EntityKind::Variable
                            | EntityKind::Field
                            | EntityKind::Constant
                    ),
                    _ => {
                        matches!(target.kind, EntityKind::Method | EntityKind::Field)
                            || (target.kind == EntityKind::Property
                                && value_types
                                    .get(&target.id)
                                    .and_then(Option::as_ref)
                                    .is_some())
                    }
                }
            };
            if !accessible {
                continue;
            }
            result.proofs.insert(
                occurrence.id,
                Proof {
                    target,
                    context,
                    receiver: Some((owner, type_source)),
                },
            );
            // Without arity evidence, a bare parameterized getter cannot be
            // used as the receiver for another member.
            if occurrence.syntax_kind == "vba.member.reference"
                && (target.kind == EntityKind::Field
                    || (target.kind == EntityKind::Property
                        && !parameterized_properties.contains(&target.id)))
            {
                resolved_members
                    .entry(span)
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(occurrence.id));
            }
        }
        Ok(result)
    }

    pub(crate) fn proof(&self, occurrence: &OccurrenceRecord) -> Option<&Proof<'a>> {
        self.proofs.get(&occurrence.id)
    }

    pub(crate) fn is_call(&self, occurrence: &OccurrenceRecord) -> bool {
        occurrence.syntax_kind == "vba.invoked_member.reference"
            && self.proof(occurrence).is_some_and(|proof| {
                matches!(proof.target.kind, EntityKind::Method | EntityKind::Function)
            })
    }

    pub(crate) fn resolve(&self, occurrence: &OccurrenceRecord) -> Option<ResolutionDecision> {
        let proof = self.proof(occurrence)?;
        let confidence = Confidence::new(1000).expect("exact source-bound confidence is valid");
        Some(ResolutionDecision {
            occurrence: occurrence.id,
            outcome: ResolutionOutcome::Resolved {
                symbol: proof.target.id,
                confidence,
            },
            explanation: ResolutionExplanation {
                rule: if proof.receiver.is_some() {
                    ResolutionRule::ClassicProjectMember
                } else {
                    ResolutionRule::ClassicProjectType
                },
                provider_name: RESOLVER_PROVIDER_NAME,
                provider_version: RESOLVER_PROVIDER_VERSION,
                candidates: vec![CandidateExplanation {
                    symbol: proof.target.id,
                    score: confidence,
                    positive_signals: vec![ResolutionSignal::NativeProjectMembership],
                    penalties: Vec::new(),
                }],
                rejected_candidates: Vec::new(),
                rejected_total: 0,
                completeness_assumptions: vec![
                    CompletenessAssumption::ValidatedNormalizedDocument,
                    CompletenessAssumption::SingleGeneration,
                    CompletenessAssumption::NoRepositoryExecution,
                    CompletenessAssumption::IndexedPackageSnapshot,
                ],
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(
        path: &str,
        bytes: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Project>, ResolutionError> {
        parse_project(
            path,
            bytes,
            cancellation,
            &mut ResolutionWorkBudget::new(crate::MAX_RESOLUTION_WORK_LIMIT),
        )
    }

    #[test]
    fn project_membership_preserves_relative_source_paths() {
        let cancel = Cancellation::default();
        let project = parse("app/project.vbp", b"Type=OleDll\r\nName=\"Primary\"\r\nClass=Worker; ..\\shared\\Worker.cls\r\nModule=Client; Client.bas\r\n", &cancel).unwrap().unwrap();
        assert_eq!(project.name, "primary");
        assert_eq!(project.members.len(), 2);
        assert!(matches!(&project.members[0].location,
            MemberLocation::Path(path) if path == "shared/worker.cls"));
        assert!(project.members[0].class);
        assert_eq!(project.members[1].name, "client");
    }

    #[test]
    fn project_manifest_lines_share_the_preparation_budget() {
        let source = b"Type=OleDll\nName=\"Primary\"\nClass=Worker; Worker.cls\n";
        let mut exact = ResolutionWorkBudget::new(3);
        assert!(
            parse_project("project.vbp", source, &Cancellation::new(), &mut exact)
                .unwrap()
                .is_some()
        );
        assert_eq!(exact.consumed(), 3);
        let mut insufficient = ResolutionWorkBudget::new(2);
        assert!(matches!(
            parse_project(
                "project.vbp",
                source,
                &Cancellation::new(),
                &mut insufficient
            ),
            Err(ResolutionError::WorkLimit { maximum: 2 })
        ));
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut unused = ResolutionWorkBudget::new(3);
        assert!(matches!(
            parse_project("project.vbp", source, &cancel, &mut unused),
            Err(ResolutionError::Cancelled(_))
        ));
        assert_eq!(unused.consumed(), 0);
    }

    #[test]
    fn project_membership_rejects_ambiguous_or_escaping_inputs() {
        let cancel = Cancellation::default();
        for entry in [
            "Class=Worker; Worker.cls\nClass=worker; Other.cls",
            "Class=Worker; Worker.cls\nModule=Other; WORKER.cls",
            "Class=Worker; ../../Worker.cls",
            "Class=Worker; C:\\Worker.cls",
            "Class=Worker; \\\\host\\Worker.cls",
            "Class=Worker; Worker.cls\nForm=Window.frm",
            "Class=Worker; Worker.cls\nName=\"Duplicate\"",
        ] {
            let source = format!("Type=OleDll\nName=\"Primary\"\n{entry}\n");
            assert!(
                parse("app/project.vbp", source.as_bytes(), &cancel)
                    .unwrap()
                    .is_none(),
                "{entry}"
            );
        }
        assert!(parse("app/project.vbp", &[255], &cancel).unwrap().is_none());
        assert!(
            parse(
                "app/project.vbp",
                &vec![b' '; MAX_MANIFEST_BYTES + 1],
                &cancel
            )
            .unwrap()
            .is_none()
        );
    }
}
