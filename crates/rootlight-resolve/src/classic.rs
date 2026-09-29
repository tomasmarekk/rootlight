//! Project-owned classic BASIC types and members over immutable source inputs.
//! Declarative membership constrains native type occurrences; directory proximity
//! and matching spellings alone never establish a project or a target.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

mod calls;
mod default_members;
mod dependencies;
pub(crate) mod standard;
mod with_members;

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
    application_parameters: BTreeMap<FactId, Arc<Vec<&'a EntityRecord>>>,
    receiver_sources: BTreeMap<FactId, Arc<Vec<SourceRef>>>,
    initializers: BTreeMap<FactId, &'a EntityRecord>,
    pub(crate) automatic_initializers: BTreeMap<FactId, AutomaticInitialization<'a>>,
}

pub(crate) struct AutomaticInitialization<'a> {
    pub(crate) variable: &'a EntityRecord,
    pub(crate) class: &'a EntityRecord,
    pub(crate) initializer: &'a EntityRecord,
    pub(crate) context: Arc<ProjectEvidence>,
    pub(crate) type_source: SourceRef,
    pub(crate) read_sources: Vec<SourceRef>,
}

pub(crate) struct Proof<'a> {
    pub(crate) target: &'a EntityRecord,
    pub(crate) context: Arc<ProjectEvidence>,
    pub(crate) receiver: Option<(&'a EntityRecord, SourceRef)>,
    rule: ResolutionRule,
    property_call: bool,
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
    instantiable: bool,
    predeclared_id: Option<std::ops::Range<usize>>,
}

#[derive(Default)]
struct PropertyAccessors<'a> {
    get: Option<&'a EntityRecord>,
    let_value: Option<&'a EntityRecord>,
    set_value: Option<&'a EntityRecord>,
    invalid: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PropertyAccessorKind {
    Get,
    Let,
    Set,
}

fn is_member(syntax: &str) -> bool {
    matches!(
        syntax,
        "vba.member.reference"
            | "vba.invoked_member.reference"
            | "vba.get_member.reference"
            | "vba.let_member.reference"
            | "vba.set_member.reference"
            | "vba.invoked_get_member.reference"
            | "vba.invoked_let_member.reference"
            | "vba.invoked_set_member.reference"
    )
}

fn is_invoked_member(syntax: &str) -> bool {
    matches!(
        syntax,
        "vba.invoked_member.reference"
            | "vba.invoked_get_member.reference"
            | "vba.invoked_let_member.reference"
            | "vba.invoked_set_member.reference"
    )
}

fn member_property_access(syntax: &str) -> Option<PropertyAccessorKind> {
    match syntax {
        "vba.get_member.reference" | "vba.invoked_get_member.reference" => {
            Some(PropertyAccessorKind::Get)
        }
        "vba.let_member.reference" | "vba.invoked_let_member.reference" => {
            Some(PropertyAccessorKind::Let)
        }
        "vba.set_member.reference" | "vba.invoked_set_member.reference" => {
            Some(PropertyAccessorKind::Set)
        }
        _ => None,
    }
}

fn procedure_kind(
    entity: &EntityRecord,
    sources: &BTreeMap<FileId, &[u8]>,
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<Option<rootlight_vba::ProcedureKind>, ResolutionError> {
    // Category and spelling must agree with the immutable declaration evidence;
    // a shared property name alone cannot identify the corresponding accessor.
    let declaration = entity.evidence.source.as_ref().and_then(|source| {
        let span = source.span();
        sources
            .get(&span.file())?
            .get(usize::try_from(span.start_byte()).ok()?..usize::try_from(span.end_byte()).ok()?)
    });
    let Some(declaration) = declaration else {
        return Ok(None);
    };
    let header = rootlight_vba::procedure_header(declaration, &mut || {
        cancellation.check()?;
        work.consume()
    })?;
    Ok(header
        .filter(|header| {
            header
                .name()
                .eq_ignore_ascii_case(entity.canonical_name.as_bytes())
        })
        .map(rootlight_vba::ProcedureHeader::kind))
}

fn scalar_procedure_return_matches(
    entity: &EntityRecord,
    occurrence: &OccurrenceRecord,
    sources: &BTreeMap<FileId, &[u8]>,
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<bool, ResolutionError> {
    if !matches!(
        procedure_kind(entity, sources, cancellation, work)?,
        Some(rootlight_vba::ProcedureKind::Function | rootlight_vba::ProcedureKind::PropertyGet)
    ) {
        return Ok(false);
    }
    let Some(source) = entity.evidence.source.as_ref() else {
        return Ok(false);
    };
    let span = source.span();
    let Some(bytes) = sources.get(&span.file()).and_then(|bytes| {
        bytes.get(usize::try_from(span.start_byte()).ok()?..usize::try_from(span.end_byte()).ok()?)
    }) else {
        return Ok(false);
    };
    let Some(ty) = rootlight_vba::scalar_return_type(bytes, &mut || {
        cancellation.check()?;
        work.consume()
    })?
    else {
        return Ok(false);
    };
    Ok(occurrence.file == span.file()
        && u64::try_from(ty.start)
            .ok()
            .and_then(|offset| span.start_byte().checked_add(offset))
            == Some(occurrence.source.span().start_byte())
        && u64::try_from(ty.end)
            .ok()
            .and_then(|offset| span.start_byte().checked_add(offset))
            == Some(occurrence.source.span().end_byte()))
}

fn property_accessor_kind(
    entity: &EntityRecord,
    sources: &BTreeMap<FileId, &[u8]>,
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<Option<PropertyAccessorKind>, ResolutionError> {
    Ok(match procedure_kind(entity, sources, cancellation, work)? {
        Some(rootlight_vba::ProcedureKind::PropertyGet) => Some(PropertyAccessorKind::Get),
        Some(rootlight_vba::ProcedureKind::PropertyLet) => Some(PropertyAccessorKind::Let),
        Some(rootlight_vba::ProcedureKind::PropertySet) => Some(PropertyAccessorKind::Set),
        Some(
            rootlight_vba::ProcedureKind::Sub
            | rootlight_vba::ProcedureKind::Function
            | rootlight_vba::ProcedureKind::ExternalSub
            | rootlight_vba::ProcedureKind::ExternalFunction,
        )
        | None => None,
    })
}

struct ResolvedProject {
    name: String,
    name_shadowed: bool,
    types: BTreeMap<String, Option<SymbolId>>,
    modules: BTreeMap<String, Option<SymbolId>>,
    default_instances: BTreeMap<SymbolId, SourceRef>,
    instantiable: BTreeSet<SymbolId>,
    value_names: BTreeSet<String>,
    member_modules: BTreeMap<FileId, SymbolId>,
    pointer_names: BTreeMap<String, Option<FileId>>,
    evidence: Arc<ProjectEvidence>,
}

// Procedure-pointer lookup excludes variables and setter-only properties
// (MS-VBAL 5.6.10). DLL declarations occupy the tier but cannot be AddressOf
// targets; retain them as barriers instead of falling through.
fn pointer_callable(
    entity: &EntityRecord,
    sources: &BTreeMap<FileId, &[u8]>,
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<Option<bool>, ResolutionError> {
    if entity.kind == EntityKind::Property {
        return Ok(
            match property_accessor_kind(entity, sources, cancellation, work)? {
                Some(PropertyAccessorKind::Get) => Some(true),
                Some(PropertyAccessorKind::Let | PropertyAccessorKind::Set) => None,
                None => Some(false),
            },
        );
    }
    if !matches!(entity.kind, EntityKind::Function | EntityKind::Method) {
        return Ok(None);
    }
    Ok(Some(matches!(
        procedure_kind(entity, sources, cancellation, work)?,
        Some(rootlight_vba::ProcedureKind::Sub | rootlight_vba::ProcedureKind::Function)
    )))
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

fn member_access_parts<'a>(
    text: &'a [u8],
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<Option<(&'a str, &'a str)>, ResolutionError> {
    cancellation.check()?;
    work.consume_many(text.len().div_ceil(64).max(1))?;
    let Some((receiver_end, member)) = rootlight_vba::member_access_receiver(text, &mut || {
        cancellation.check()?;
        work.consume()
    })?
    else {
        return Ok(None);
    };
    let Some(receiver) = text.get(..receiver_end) else {
        return Ok(None);
    };
    Ok(std::str::from_utf8(receiver)
        .ok()
        .zip(std::str::from_utf8(member).ok()))
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
                instantiable: key.eq_ignore_ascii_case("Class"),
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
                    instantiable: member.kind == Some(NativeClassicMemberKind::Class),
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
            let mut pointer_names = BTreeMap::from([(project.name.clone(), None)]);
            let mut member_modules = BTreeMap::new();
            let mut modules = BTreeMap::from([(project.name.clone(), None)]);
            let mut default_instances = BTreeMap::new();
            let mut instantiable = BTreeSet::new();
            let mut value_names = BTreeSet::new();
            let mut name_shadowed = false;
            work.consume_many(members.len())?;
            let member_files: Vec<_> = members
                .iter()
                .filter_map(|member| member_file(member).map(|file| file.id))
                .collect();
            let mut complete = project.complete;
            for member in members {
                cancellation.check()?;
                work.consume_many(5)?;
                name_shadowed |= member.name == project.name;
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
                work.consume()?;
                member_modules.insert(file.id, named[0].id);
                if member.instantiable {
                    instantiable.insert(named[0].id);
                }
                if !member.class {
                    work.consume()?;
                    pointer_names
                        .entry(member.name.clone())
                        .and_modify(|prior| *prior = None)
                        .or_insert(Some(file.id));
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
                            && !matches!(entity.kind, EntityKind::Struct | EntityKind::Enum)
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
                    name_shadowed |= name == project.name;
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
                name: project.name,
                name_shadowed,
                types,
                modules,
                default_instances,
                instantiable,
                value_names,
                member_modules,
                pointer_names,
                evidence: Arc::new(evidence),
            }));
        }
        let mut pointer_sites = BTreeMap::<usize, Vec<&OccurrenceRecord>>::new();
        for occurrence in occurrences {
            cancellation.check()?;
            work.consume()?;
            if matches!(
                occurrence.syntax_kind.as_str(),
                "vba.procedure_pointer.reference" | "vba.qualified_procedure_pointer.reference"
            ) && occurrence.role == OccurrenceRole::Reference
                && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
                && let Some(project) = memberships.get(&occurrence.file).copied().flatten()
            {
                work.consume()?;
                pointer_sites.entry(project).or_default().push(occurrence);
            }
        }
        for (project_id, sites) in pointer_sites {
            let Some(project) = projects.get(project_id).and_then(Option::as_ref) else {
                continue;
            };
            let mut bindings = BTreeMap::<(FileId, String), (Option<SymbolId>, bool)>::new();
            let mut complete = true;
            let mut source_files = BTreeSet::new();
            let mut blocked_sources = BTreeSet::new();
            for site in &sites {
                cancellation.check()?;
                work.consume()?;
                source_files.insert(site.file);
            }
            for (file, module) in &project.member_modules {
                cancellation.check()?;
                work.consume()?;
                let procedural = entities
                    .get(module)
                    .is_some_and(|owner| owner.kind == EntityKind::Module);
                // Class members participate only in their own lexical tier;
                // an unrelated class cannot add competing project callbacks.
                if !procedural && !source_files.contains(file) {
                    continue;
                }
                let mut source_complete = memberships.get(file) == Some(&Some(project_id));
                let Some(bytes) = sources.get(file) else {
                    complete &= !procedural;
                    work.consume()?;
                    blocked_sources.insert(*file);
                    continue;
                };
                // Branch directives can change local and project-wide uniqueness.
                // A #Const alone only defines a separate compiler-constant name;
                // it cannot remove or introduce runtime declarations (MS-VBAL 3.4).
                let mut remaining = *bytes;
                while let Some(line) = remaining
                    .split_inclusive(|byte| matches!(byte, b'\r' | b'\n'))
                    .next()
                {
                    cancellation.check()?;
                    work.consume_many(line.len().div_ceil(64).max(1))?;
                    if line.trim_ascii_start().starts_with(b"#")
                        && rootlight_vba::compilation_constant_header(remaining, &mut || {
                            cancellation.check()?;
                            work.consume()
                        })?
                        .is_none()
                    {
                        source_complete = false;
                    }
                    remaining = remaining
                        .get(line.len()..)
                        .ok_or(ResolutionError::ClassicSourceIdentity)?;
                }
                for entity in declarations.get(file).into_iter().flatten() {
                    cancellation.check()?;
                    work.consume()?;
                    if entity.container != Some(ContainerRef::Entity(*module)) {
                        continue;
                    }
                    let Some(eligible) = pointer_callable(entity, &sources, cancellation, work)?
                    else {
                        continue;
                    };
                    let Some(name) = identifier(&entity.canonical_name) else {
                        source_complete = false;
                        continue;
                    };
                    let exported = procedural && entity.visibility != EntityVisibility::Private;
                    let target = (eligible
                        && matches!(
                            entity.visibility,
                            EntityVisibility::Public | EntityVisibility::Restricted
                        ))
                    .then_some(entity.id);
                    work.consume()?;
                    bindings
                        .entry((*file, name))
                        .and_modify(|(prior, public)| {
                            *prior = None;
                            *public |= exported;
                        })
                        .or_insert((target, exported));
                }
                if !source_complete {
                    complete &= !procedural;
                    work.consume()?;
                    blocked_sources.insert(*file);
                }
            }
            if !complete {
                continue;
            }
            let mut exports = BTreeMap::new();
            for ((_, name), (target, exported)) in &bindings {
                cancellation.check()?;
                work.consume()?;
                if *exported {
                    exports
                        .entry(name.as_str())
                        .and_modify(|prior| *prior = None)
                        .or_insert(*target);
                }
            }
            for occurrence in sites {
                cancellation.check()?;
                work.consume()?;
                if !project.member_modules.contains_key(&occurrence.file)
                    || blocked_sources.contains(&occurrence.file)
                {
                    continue;
                }
                let span = occurrence.source.span();
                let start = usize::try_from(span.start_byte())
                    .map_err(|_| ResolutionError::CountOverflow)?;
                let end =
                    usize::try_from(span.end_byte()).map_err(|_| ResolutionError::CountOverflow)?;
                let Some(text) = sources
                    .get(&occurrence.file)
                    .and_then(|bytes| bytes.get(start..end))
                else {
                    continue;
                };
                work.consume_many(text.len().div_ceil(64).max(1))?;
                let Some(path) = rootlight_vba::procedure_pointer_path(text, &mut || {
                    cancellation.check()?;
                    work.consume()
                })?
                else {
                    continue;
                };
                let qualified =
                    occurrence.syntax_kind == "vba.qualified_procedure_pointer.reference";
                if path.end() != text.len() || path.components() != if qualified { 2 } else { 1 } {
                    continue;
                }
                let Some(name) = std::str::from_utf8(path.last()).ok().and_then(identifier) else {
                    continue;
                };
                let (symbol, receiver) = if qualified {
                    let Some(qualifier) =
                        std::str::from_utf8(path.first()).ok().and_then(identifier)
                    else {
                        continue;
                    };
                    // A procedure/getter outranks a module in pointer context,
                    // but ordinary values and types cannot shadow that module.
                    if bindings.contains_key(&(occurrence.file, qualifier.clone())) {
                        continue;
                    }
                    let Some(file) = project.pointer_names.get(&qualifier).copied().flatten()
                    else {
                        continue;
                    };
                    let Some(module) = project
                        .member_modules
                        .get(&file)
                        .and_then(|symbol| entities.get(symbol))
                        .copied()
                    else {
                        continue;
                    };
                    let Some(source) = module.evidence.source.as_ref() else {
                        continue;
                    };
                    let symbol = bindings.get(&(file, name)).and_then(|(target, _)| *target);
                    (symbol, Some((module, source.clone())))
                } else {
                    // A local procedure or a project/module name occupies an earlier
                    // tier even when it cannot supply a valid procedure address.
                    if bindings.contains_key(&(occurrence.file, name.clone()))
                        || project.pointer_names.contains_key(&name)
                    {
                        continue;
                    }
                    (exports.get(name.as_str()).copied().flatten(), None)
                };
                let Some(target) = symbol.and_then(|symbol| entities.get(&symbol)).copied() else {
                    continue;
                };
                // MS-VBAL 5.6.12 permits qualified Sub/Function addresses, not
                // Property Get addresses admitted by simple-name pointer lookup.
                if qualified && target.kind != EntityKind::Function {
                    continue;
                }
                work.consume()?;
                result.proofs.insert(
                    occurrence.id,
                    Proof {
                        target,
                        context: Arc::clone(&project.evidence),
                        receiver,
                        rule: ResolutionRule::ClassicProjectPointer,
                        property_call: false,
                    },
                );
            }
        }
        for occurrence in occurrences {
            cancellation.check()?;
            work.consume()?;
            let implementation = occurrence.role == OccurrenceRole::ImplementationUse
                && occurrence.syntax_kind == "vba.implemented_type.reference";
            if (!implementation && occurrence.role != OccurrenceRole::TypeUse)
                || !matches!(
                    occurrence.syntax_kind.as_str(),
                    "vba.direct_type_identifier.reference"
                        | "vba.implemented_type.reference"
                        | "vba.type_identifier.reference"
                        | "vba.automatic_type_identifier.reference"
                        | "vba.constructed_type_identifier.reference"
                        | "vba.tested_type_identifier.reference"
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
            if implementation
                && !occurrence.enclosing.is_some_and(|id| {
                    project.member_modules.get(&occurrence.file) == Some(&id)
                        && entities
                            .get(&id)
                            .is_some_and(|owner| owner.kind == EntityKind::Class)
                })
            {
                continue;
            }
            let span = occurrence.source.span();
            let start =
                usize::try_from(span.start_byte()).map_err(|_| ResolutionError::CountOverflow)?;
            let end =
                usize::try_from(span.end_byte()).map_err(|_| ResolutionError::CountOverflow)?;
            let Some(text) = bytes.get(start..end) else {
                continue;
            };
            let (name, qualified) = if implementation || text.contains(&b'.') {
                let Some(path) = rootlight_vba::procedure_pointer_path(text, &mut || {
                    cancellation.check()?;
                    work.consume()
                })?
                else {
                    continue;
                };
                if path.end() != text.len() || path.components() > 2 {
                    continue;
                }
                let qualified = path.components() == 2;
                if qualified
                    && (project.name_shadowed
                        || !path.first().eq_ignore_ascii_case(project.name.as_bytes())
                        || local_types.contains_key(&(occurrence.file, project.name.clone())))
                {
                    continue;
                }
                (
                    std::str::from_utf8(path.last()).ok().and_then(identifier),
                    qualified,
                )
            } else {
                (std::str::from_utf8(text).ok().and_then(identifier), false)
            };
            let Some(name) = name else { continue };
            // Local UDT/enum type declarations have their own lookup tier, even
            // when native analysis could not safely select one of them.
            if !qualified && local_types.contains_key(&(occurrence.file, name.clone())) {
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
            // Construction and Implements require classes, not UDTs/enums
            // that happen to occupy the same type lookup tier.
            if matches!(
                occurrence.syntax_kind.as_str(),
                "vba.automatic_type_identifier.reference"
                    | "vba.constructed_type_identifier.reference"
                    | "vba.implemented_type.reference"
            ) && target.kind != EntityKind::Class
            {
                continue;
            }
            if implementation && occurrence.enclosing == Some(target.id) {
                continue;
            }
            result.proofs.insert(
                occurrence.id,
                Proof {
                    target,
                    context: Arc::clone(&project.evidence),
                    receiver: None,
                    rule: ResolutionRule::ClassicProjectType,
                    property_call: false,
                },
            );
        }
        // A project-proven UDT type owns its declared fields, while a property
        // getter's result type can establish the next receiver in a chain.
        // Enum values do not establish an instance member environment.
        let mut value_types = BTreeMap::new();
        let mut automatic_types = BTreeSet::new();
        let mut receivers = BTreeMap::new();
        let mut native_results = BTreeMap::new();
        let mut members = BTreeMap::new();
        let mut property_accessors = BTreeMap::new();
        let mut field_definitions = BTreeMap::new();
        let mut unconditional_udts = BTreeMap::new();
        for entity in entities.values() {
            cancellation.check()?;
            work.consume()?;
            // Compiler constants are module-private but never runtime members,
            // including when their spelling matches a module qualifier.
            if entity.kind == EntityKind::Constant
                && entity.evidence.source.as_ref().is_some_and(|source| {
                    let span = source.span();
                    usize::try_from(span.start_byte())
                        .ok()
                        .and_then(|start| sources.get(&span.file())?.get(start))
                        == Some(&b'#')
                })
            {
                continue;
            }
            if let Some(ContainerRef::Entity(owner)) = entity.container {
                let key = (owner, entity.canonical_name.to_ascii_lowercase());
                if entity.kind == EntityKind::Property {
                    let prior_member = members.contains_key(&key);
                    let accessors = property_accessors.entry(key.clone()).or_insert_with(|| {
                        PropertyAccessors {
                            invalid: prior_member,
                            ..PropertyAccessors::default()
                        }
                    });
                    match property_accessor_kind(entity, &sources, cancellation, work)? {
                        Some(PropertyAccessorKind::Get) if accessors.get.is_none() => {
                            accessors.get = Some(*entity);
                        }
                        Some(PropertyAccessorKind::Let) if accessors.let_value.is_none() => {
                            accessors.let_value = Some(*entity);
                        }
                        Some(PropertyAccessorKind::Set) if accessors.set_value.is_none() => {
                            accessors.set_value = Some(*entity);
                        }
                        _ => accessors.invalid = true,
                    }
                } else if let Some(accessors) = property_accessors.get_mut(&key) {
                    accessors.invalid = true;
                }
                members
                    .entry(key)
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
                        | EntityKind::Function
                        | EntityKind::Method
                )
                // Expression-side type uses do not declare a procedure's
                // return type or an assignment receiver's type.
                && matches!(occurrence.syntax_kind.as_str(),
                    "vba.direct_type_identifier.reference"
                    | "vba.automatic_type_identifier.reference"
                    | "vba.type_identifier.reference")
            {
                if matches!(
                    owner.kind,
                    EntityKind::Function | EntityKind::Method | EntityKind::Property
                ) && !scalar_procedure_return_matches(
                    owner,
                    occurrence,
                    &sources,
                    cancellation,
                    work,
                )? {
                    continue;
                }
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
                        if occurrence.syntax_kind == "vba.automatic_type_identifier.reference"
                            && matches!(owner.kind, EntityKind::Variable | EntityKind::Field)
                        {
                            automatic_types.insert((owner.id, occurrence.source.clone()));
                        }
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
            if matches!(
                occurrence.syntax_kind.as_str(),
                "vba.value_identifier.reference" | "vba.identifier.reference"
            ) && occurrence.role == OccurrenceRole::Reference
                && let OccurrenceTarget::Resolved { symbol } = occurrence.target
            {
                receivers
                    .entry(occurrence.source.span())
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(symbol));
            }
            if occurrence.syntax_kind == "vba.application.reference"
                && let OccurrenceTarget::Resolved { symbol } = occurrence.target
                && let Some(target) = entities.get(&symbol).copied()
                && matches!(
                    target.kind,
                    EntityKind::Function | EntityKind::Method | EntityKind::Property
                )
                && target
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == occurrence.file)
                && matches!(
                    procedure_kind(target, &sources, cancellation, work)?,
                    Some(
                        rootlight_vba::ProcedureKind::Function
                            | rootlight_vba::ProcedureKind::PropertyGet
                    )
                )
            {
                // Native analysis already checked this entire same-file call.
                // Its result may acquire a project-owned type in this pass;
                // array applications cannot supply this callable return proof.
                receivers
                    .entry(occurrence.source.span())
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(symbol));
                native_results.insert(occurrence.source.span(), occurrence.source.clone());
            }
        }
        let project_classes = projects
            .iter()
            .flatten()
            .flat_map(|project| project.member_modules.values())
            .copied()
            .filter(|id| {
                entities
                    .get(id)
                    .is_some_and(|entity| entity.kind == EntityKind::Class)
            })
            .collect();
        let default_members =
            default_members::inventory(entities, &project_classes, &sources, cancellation, work)?;
        let mut module_qualifiers = BTreeMap::new();
        let mut member_occurrences = Vec::new();
        let mut read_receivers: BTreeMap<_, Vec<SourceRef>> = BTreeMap::new();
        let mut automatic_reads = BTreeMap::new();
        let mut calls = calls::CallIndex::build(index, occurrences, cancellation, work)?;
        let mut constructed_types = BTreeMap::new();
        let mut constructed_classes = BTreeSet::new();
        for (owner, source) in &automatic_types {
            cancellation.check()?;
            work.consume()?;
            if let Some((_, class, _, type_source)) =
                value_types.get(owner).and_then(Option::as_ref)
                && source == type_source
            {
                constructed_classes.insert(*class);
            }
        }
        for site in occurrences {
            cancellation.check()?;
            work.consume()?;
            if site.role == OccurrenceRole::TypeUse
                && site.syntax_kind == "vba.constructed_type_identifier.reference"
            {
                constructed_types
                    .entry(site.source.span())
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(site));
                if let Some(proof) = result.proof(site) {
                    constructed_classes.insert(proof.target.id);
                }
            }
        }
        // Type lookup alone cannot establish construction: host documents are
        // not ordinary creatable classes, and lifecycle signatures must agree
        // with both the source inventory and complete project-owned member facts.
        let mut initializers = BTreeMap::new();
        for class in projects
            .iter()
            .flatten()
            .flat_map(|project| &project.instantiable)
        {
            cancellation.check()?;
            work.consume()?;
            if !constructed_classes.contains(class) {
                continue;
            }
            let class_source = entities
                .get(class)
                .and_then(|entity| entity.evidence.source.as_ref());
            let declaration = class_source.and_then(|source| sources.get(&source.span().file()));
            let inventory = if let Some(bytes) = declaration {
                rootlight_vba::class_initializer(bytes, &mut || {
                    cancellation.check()?;
                    work.consume()
                })?
            } else {
                rootlight_vba::ClassInitializer::Unproven
            };
            let initializer = match (
                inventory,
                members.get(&(*class, "class_initialize".to_owned())),
            ) {
                (rootlight_vba::ClassInitializer::Absent, None) => Some(None),
                (rootlight_vba::ClassInitializer::Header(header), Some(Some(entity)))
                    if matches!(entity.kind, EntityKind::Function | EntityKind::Method) =>
                {
                    let declaration = entity.evidence.source.as_ref().and_then(|source| {
                        let span = source.span();
                        sources.get(&span.file())?.get(
                            usize::try_from(span.start_byte()).ok()?
                                ..usize::try_from(span.end_byte()).ok()?,
                        )
                    });
                    let matches_header = entity.evidence.source.as_ref().is_some_and(|source| {
                        class_source.is_some_and(|class_source| {
                            source.span().file() == class_source.span().file()
                        }) && Some(source.span().start_byte()) == u64::try_from(header.start).ok()
                            && u64::try_from(header.end)
                                .ok()
                                .is_some_and(|end| end <= source.span().end_byte())
                    });
                    let valid = if let Some(declaration) = declaration.filter(|_| matches_header) {
                        rootlight_vba::parameterless_sub(declaration, &mut || {
                            cancellation.check()?;
                            work.consume()
                        })? && procedure_kind(entity, &sources, cancellation, work)?
                            == Some(rootlight_vba::ProcedureKind::Sub)
                    } else {
                        false
                    };
                    valid.then_some(Some(*entity))
                }
                _ => None,
            };
            initializers.insert(*class, initializer);
        }
        let mut constructed_receivers = BTreeMap::new();
        for site in occurrences {
            cancellation.check()?;
            work.consume()?;
            if site.role != OccurrenceRole::Reference
                || site.syntax_kind != "vba.construction.reference"
                || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
            {
                continue;
            }
            let span = site.source.span();
            let Some(bytes) = sources.get(&site.file).and_then(|bytes| {
                bytes.get(
                    usize::try_from(span.start_byte()).ok()?
                        ..usize::try_from(span.end_byte()).ok()?,
                )
            }) else {
                continue;
            };
            let Some(range) = rootlight_vba::constructed_type_range(bytes, &mut || {
                cancellation.check()?;
                work.consume()
            })?
            else {
                continue;
            };
            if range.end != bytes.len() {
                continue;
            }
            let type_span = rootlight_ir::SourceSpan::new(
                site.file,
                span.start_byte()
                    .checked_add(
                        u64::try_from(range.start).map_err(|_| ResolutionError::CountOverflow)?,
                    )
                    .ok_or(ResolutionError::CountOverflow)?,
                span.end_byte(),
            )
            .map_err(ResolutionError::InvalidProducer)?;
            let Some(type_site) = constructed_types.get(&type_span).and_then(|site| *site) else {
                continue;
            };
            let Some(proof) = result.proof(type_site) else {
                continue;
            };
            let Some(initializer) = initializers.get(&proof.target.id).copied().flatten() else {
                continue;
            };
            let proof = Proof {
                target: proof.target,
                context: Arc::clone(&proof.context),
                receiver: Some((proof.target, type_site.source.clone())),
                rule: ResolutionRule::ClassicProjectConstruction,
                property_call: false,
            };
            result.proofs.insert(site.id, proof);
            if let Some(initializer) = initializer {
                result.initializers.insert(site.id, initializer);
            }
            constructed_receivers
                .entry(span)
                .and_modify(|prior| *prior = None)
                .or_insert(Some(site));
        }
        let mut parsed_members =
            with_members::inventory(occurrences, &sources, cancellation, work)?;
        for occurrence in occurrences {
            cancellation.check()?;
            if occurrence.syntax_kind == "vba.application.reference"
                && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            {
                if let Some(receiver) = calls.returned_receiver(occurrence, cancellation, work)? {
                    read_receivers
                        .entry(receiver.source.span())
                        .or_default()
                        .push(occurrence.source.clone());
                }
                member_occurrences.push((occurrence, None));
                continue;
            }
            if occurrence.syntax_kind == "vba.value_identifier.reference"
                && occurrence.role == OccurrenceRole::Reference
                && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            {
                module_qualifiers
                    .entry(occurrence.source.span())
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(occurrence));
            }
            let member =
                is_member(&occurrence.syntax_kind) && occurrence.role == OccurrenceRole::Reference;
            if member && let Some(member) = parsed_members.remove(&occurrence.id) {
                // Only the exact receiver span proves a getter read. The
                // logical separator must not become part of that identity.
                read_receivers
                    .entry(member.span)
                    .or_default()
                    .push(occurrence.source.clone());
                if !automatic_types.is_empty() {
                    let mut evidence = vec![occurrence.source.clone()];
                    evidence.extend(member.boundaries.iter().cloned());
                    // Multiple With-body reads share one evaluated receiver. Select
                    // its witness by source order, not generation-dependent fact IDs.
                    automatic_reads
                        .entry(member.span)
                        .and_modify(|prior: &mut Vec<SourceRef>| {
                            if evidence < *prior {
                                *prior = evidence.clone();
                            }
                        })
                        .or_insert(evidence);
                }
                if matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }) {
                    member_occurrences.push((occurrence, Some(member)));
                    continue;
                }
            }
            // Keep the per-occurrence charge even for records that cannot
            // participate; lexical scans have their own bounded work charges.
            work.consume()?;
        }
        let order = dependencies::order(&member_occurrences, &calls, &sources, cancellation, work)?;
        let mut resolved_members = BTreeMap::new();
        for i in order {
            let Some((occurrence, member)) = member_occurrences.get(i) else {
                continue;
            };
            cancellation.check()?;
            work.consume()?;
            let span = occurrence.source.span();
            let Some(member) = member else {
                calls.resolve(&mut result, occurrence, &sources, cancellation, work)?;
                let callee = if result.proof(occurrence).is_none() {
                    match calls.callee(occurrence, &sources, cancellation, work)? {
                        Some((callee, false)) => Some(callee),
                        None if read_receivers.contains_key(&span) => {
                            calls.returned_receiver(occurrence, cancellation, work)?
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(callee) = callee
                    && let Some(access) = match callee.syntax_kind.as_str() {
                        "vba.application.reference"
                        | "vba.invoked_get_identifier.reference"
                        | "vba.invoked_get_member.reference" => Some(PropertyAccessorKind::Get),
                        "vba.invoked_let_identifier.reference"
                        | "vba.invoked_let_member.reference" => Some(PropertyAccessorKind::Let),
                        "vba.invoked_set_identifier.reference"
                        | "vba.invoked_set_member.reference" => Some(PropertyAccessorKind::Set),
                        // Native GLR statement syntax also admits a call whose
                        // argument is a comparison. Only a typed storage receiver
                        // and an argument-validated default setter prove assignment.
                        "vba.invoked_identifier.reference"
                        | "vba.invoked_member.reference"
                        | "vba.member.reference" => Some(PropertyAccessorKind::Let),
                        _ => None,
                    }
                    && let Some(symbol) = match callee.target {
                        OccurrenceTarget::Resolved { symbol } => Some(symbol),
                        _ => result.proof(callee).map(|proof| proof.target.id),
                    }
                    && let Some((owner, class, context, type_source)) =
                        value_types.get(&symbol).and_then(Option::as_ref)
                    && (matches!(
                        owner.kind,
                        EntityKind::Variable | EntityKind::Parameter | EntityKind::Field
                    ) || (!matches!(
                        callee.syntax_kind.as_str(),
                        "vba.invoked_identifier.reference"
                            | "vba.invoked_member.reference"
                            | "vba.member.reference"
                    ) && matches!(
                        procedure_kind(owner, &sources, cancellation, work)?,
                        Some(
                            rootlight_vba::ProcedureKind::Function
                                | rootlight_vba::ProcedureKind::PropertyGet
                        )
                    ) && (callee.syntax_kind == "vba.application.reference"
                        || calls.has_empty_parameter_list(owner, &sources, cancellation, work)?)))
                    && let Some(default) = default_members.get(class).and_then(Option::as_ref)
                {
                    let key = (*class, default.name.clone());
                    let target = if let Some(accessors) = property_accessors.get(&key) {
                        (!accessors.invalid)
                            .then_some(match access {
                                PropertyAccessorKind::Get => accessors.get,
                                PropertyAccessorKind::Let => accessors.let_value,
                                PropertyAccessorKind::Set => accessors.set_value,
                            })
                            .flatten()
                    } else {
                        members.get(&key).copied().flatten()
                    };
                    if let Some(target) =
                        target.filter(|target| target.visibility == EntityVisibility::Public)
                        && matches!(
                            (
                                access,
                                procedure_kind(target, &sources, cancellation, work)?
                            ),
                            (
                                PropertyAccessorKind::Get,
                                Some(
                                    rootlight_vba::ProcedureKind::Function
                                        | rootlight_vba::ProcedureKind::PropertyGet
                                )
                            ) | (
                                PropertyAccessorKind::Let,
                                Some(rootlight_vba::ProcedureKind::PropertyLet)
                            ) | (
                                PropertyAccessorKind::Set,
                                Some(rootlight_vba::ProcedureKind::PropertySet)
                            )
                        )
                    {
                        let mut evidence = default.sources.clone();
                        evidence.extend(owner.evidence.source.iter().cloned());
                        if callee.syntax_kind == "vba.application.reference" {
                            evidence
                                .extend(read_receivers.get(&span).into_iter().flatten().cloned());
                        }
                        if let Some(proof) = result.proof(callee)
                            && let Some((_, source)) = &proof.receiver
                        {
                            evidence.push(source.clone());
                        }
                        calls.resolve_bound(
                            &mut result,
                            occurrence,
                            calls::BoundCall {
                                callee,
                                explicit: false,
                                proof: Proof {
                                    target,
                                    context: Arc::clone(context),
                                    receiver: Some((*owner, type_source.clone())),
                                    rule: ResolutionRule::ClassicProjectDefaultMember,
                                    property_call: true,
                                },
                                sources: &evidence,
                            },
                            &sources,
                            cancellation,
                            work,
                        )?;
                    }
                }
                if result.proof(occurrence).is_some() {
                    resolved_members
                        .entry(span)
                        .and_modify(|prior| *prior = None)
                        .or_insert(Some(occurrence.id));
                }
                continue;
            };
            let receiver = member.receiver;
            let Some(name) = identifier(member.name) else {
                continue;
            };
            let receiver_span = member.span;
            let binding = if let Some(site) = constructed_receivers.get(&receiver_span) {
                site.and_then(|site| {
                    result.proof(site).map(|proof| {
                        (
                            proof.target,
                            proof.target.id,
                            Arc::clone(&proof.context),
                            site.source.clone(),
                        )
                    })
                })
            } else if let Some(value) = receivers.get(&receiver_span) {
                value
                    .and_then(|value| value_types.get(&value))
                    .and_then(Option::as_ref)
                    .map(|(owner, class, context, source)| {
                        (*owner, *class, Arc::clone(context), source.clone())
                    })
            } else if receiver.contains('.') || resolved_members.contains_key(&receiver_span) {
                // A composed receiver requires the shorter exact expression:
                // a field, property result or argument-validated function result.
                resolved_members
                    .get(&receiver_span)
                    .and_then(|id: &Option<FactId>| *id)
                    .and_then(|id| result.proofs.get(&id))
                    .filter(|proof| {
                        matches!(
                            proof.target.kind,
                            EntityKind::Field
                                | EntityKind::Property
                                | EntityKind::Function
                                | EntityKind::Method
                        )
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
                                    | (
                                        EntityKind::Function | EntityKind::Method,
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
                // An uncertain local type still outranks a project export.
                if local_types.contains_key(&(occurrence.file, receiver_name.clone())) {
                    continue;
                }
                let default_instance = project
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
                    });
                default_instance
                    .or_else(|| {
                        // Standard modules have no instance. Their verified
                        // project declaration itself owns the qualified member.
                        let module = project
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
                            });
                        if let Some((owner, _, context, _)) = &module
                            && let Some(qualifier) =
                                module_qualifiers.get(&receiver_span).and_then(|site| *site)
                        {
                            result.proofs.entry(qualifier.id).or_insert_with(|| Proof {
                                target: owner,
                                context: Arc::clone(context),
                                receiver: None,
                                rule: ResolutionRule::ClassicProjectModule,
                                property_call: false,
                            });
                        }
                        module
                    })
                    .or_else(|| {
                        // Enum qualification names a static type in a standard
                        // module, not an instance of a project class.
                        let enumeration = project
                            .types
                            .get(&receiver_name)
                            .copied()
                            .flatten()
                            .and_then(|id| entities.get(&id).copied())
                            .filter(|owner| {
                                owner.kind == EntityKind::Enum
                                    && owner
                                        .container
                                        .and_then(|container| match container {
                                            ContainerRef::Entity(id) => entities.get(&id),
                                            _ => None,
                                        })
                                        .is_some_and(|module| module.kind == EntityKind::Module)
                            })?;
                        let source = enumeration.evidence.source.as_ref()?.clone();
                        Some((
                            enumeration,
                            enumeration.id,
                            Arc::clone(&project.evidence),
                            source,
                        ))
                    })
            };
            let Some((owner, class, context, type_source)) = binding else {
                continue;
            };
            let key = (class, name);
            let access = member_property_access(&occurrence.syntax_kind).or_else(|| {
                read_receivers
                    .contains_key(&span)
                    .then_some(PropertyAccessorKind::Get)
            });
            // Accessor identity is independent of the returned value's type.
            // Unknown access and conflicting declarations cannot select a getter
            // merely because it is the only accessor with a known return type.
            let target = if let Some(accessors) = property_accessors.get(&key) {
                if accessors.invalid {
                    None
                } else {
                    match access {
                        Some(PropertyAccessorKind::Get) => accessors.get,
                        Some(PropertyAccessorKind::Let) => accessors.let_value,
                        Some(PropertyAccessorKind::Set) => accessors.set_value,
                        None => None,
                    }
                }
            } else {
                members.get(&key).copied().flatten()
            };
            let Some(target) = target else {
                continue;
            };
            let Some(receiver_type) = entities.get(&class).copied() else {
                continue;
            };
            if member.implicit_root
                && !matches!(receiver_type.kind, EntityKind::Class | EntityKind::Struct)
            {
                continue;
            }
            let accessible = if receiver_type.kind == EntityKind::Enum {
                target.kind == EntityKind::Constant
                    && target.container == Some(ContainerRef::Entity(class))
                    && target.visibility != EntityVisibility::Private
            } else if receiver_type.kind == EntityKind::Struct {
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
                            | EntityKind::Property
                    ),
                    _ => {
                        matches!(
                            target.kind,
                            EntityKind::Method | EntityKind::Field | EntityKind::Property
                        )
                    }
                }
            };
            if !accessible {
                continue;
            }
            let mut evidence = BTreeSet::new();
            evidence.extend(native_results.get(&receiver_span).cloned());
            if let Some(id) = resolved_members.get(&receiver_span).and_then(|id| *id) {
                if let Some(sources) = result.receiver_sources.get(&id) {
                    for source in sources.iter() {
                        work.consume()?;
                        evidence.insert(source.clone());
                    }
                }
                if let Some(proof) = result.proofs.get(&id) {
                    evidence.extend(proof.target.evidence.source.iter().cloned());
                    if let Some((_, source)) = &proof.receiver {
                        evidence.insert(source.clone());
                    }
                }
            }
            evidence.extend(member.boundaries.iter().cloned());
            if !evidence.is_empty() {
                result
                    .receiver_sources
                    .insert(occurrence.id, Arc::new(evidence.into_iter().collect()));
            }
            if let Some(parameters) = resolved_members
                .get(&receiver_span)
                .and_then(|id| *id)
                .and_then(|id| result.application_parameters.get(&id))
                .cloned()
            {
                result
                    .application_parameters
                    .insert(occurrence.id, parameters);
            }
            if receiver_type.kind == EntityKind::Enum
                && let Some(qualifier) =
                    module_qualifiers.get(&receiver_span).and_then(|site| *site)
            {
                result.proofs.entry(qualifier.id).or_insert_with(|| Proof {
                    target: receiver_type,
                    context: Arc::clone(&context),
                    receiver: None,
                    rule: ResolutionRule::ClassicProjectEnum,
                    property_call: false,
                });
            }
            let implicit_parameters = if target.kind == EntityKind::Property
                && !is_invoked_member(&occurrence.syntax_kind)
            {
                calls.implicit_property_parameters(
                    target,
                    access != Some(PropertyAccessorKind::Get),
                    &sources,
                    cancellation,
                    work,
                )?
            } else {
                None
            };
            if let Some(parameters) = &implicit_parameters {
                if let Some(inherited) = result.application_parameters.get(&occurrence.id) {
                    if !parameters.is_empty() {
                        let mut combined = BTreeMap::new();
                        for parameter in inherited.iter().chain(parameters.iter()) {
                            cancellation.check()?;
                            work.consume()?;
                            combined.insert(parameter.id, *parameter);
                        }
                        result
                            .application_parameters
                            .insert(occurrence.id, Arc::new(combined.into_values().collect()));
                    }
                } else if !parameters.is_empty() {
                    result
                        .application_parameters
                        .insert(occurrence.id, Arc::clone(parameters));
                }
            }
            let property_call = target.kind == EntityKind::Property
                && (implicit_parameters.is_some() || is_invoked_member(&occurrence.syntax_kind));
            result.proofs.insert(
                occurrence.id,
                Proof {
                    target,
                    context,
                    receiver: Some((owner, type_source)),
                    rule: ResolutionRule::ClassicProjectMember,
                    property_call,
                },
            );
            // Without arity evidence, a bare parameterized getter cannot be
            // used as the receiver for another member.
            if !is_invoked_member(&occurrence.syntax_kind)
                && (target.kind == EntityKind::Field
                    || (target.kind == EntityKind::Property
                        && access == Some(PropertyAccessorKind::Get)
                        && property_call))
            {
                resolved_members
                    .entry(span)
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(occurrence.id));
            }
        }
        standard::bind(
            &mut result,
            index,
            &module_qualifiers,
            &member_occurrences,
            &read_receivers,
            &memberships,
            &projects,
            &members,
            cancellation,
            work,
        )?;
        if automatic_types.is_empty() {
            return Ok(result);
        }
        let mut pending_automatic = Vec::new();
        let mut value_read_files = BTreeSet::new();
        for site in occurrences {
            cancellation.check()?;
            work.consume()?;
            if site.role != OccurrenceRole::Reference {
                continue;
            }
            let symbol = match site.target {
                OccurrenceTarget::Resolved { symbol } => Some(symbol),
                OccurrenceTarget::Unresolved { .. } => {
                    result.proof(site).map(|proof| proof.target.id)
                }
                _ => None,
            };
            let Some((variable, class, context, type_source)) = symbol
                .and_then(|symbol| value_types.get(&symbol))
                .and_then(Option::as_ref)
            else {
                continue;
            };
            if !automatic_types.contains(&(variable.id, type_source.clone())) {
                continue;
            }
            let (Some(Some(Some(initializer))), Some(class)) =
                (initializers.get(class), entities.get(class))
            else {
                continue;
            };
            value_read_files.insert(site.file);
            pending_automatic.push((
                site,
                AutomaticInitialization {
                    variable,
                    class,
                    initializer,
                    context: Arc::clone(context),
                    type_source: type_source.clone(),
                    read_sources: Vec::new(),
                },
            ));
        }
        for file in value_read_files {
            cancellation.check()?;
            work.consume()?;
            let Some((source, bytes)) = index
                .files
                .get(&file)
                .and_then(|file| file.evidence.source.as_ref())
                .zip(sources.get(&file))
            else {
                continue;
            };
            let Some(reads) = rootlight_vba::statement_value_reads(
                bytes,
                crate::MAX_RESOLUTION_WORK_LIMIT,
                crate::MAX_RESOLUTION_WORK_LIMIT,
                &mut || {
                    cancellation.check()?;
                    work.consume()
                },
            )?
            else {
                continue;
            };
            for read in reads {
                cancellation.check()?;
                work.consume()?;
                let value = with_members::source_ref(source, read.value())?;
                let statement = with_members::source_ref(source, read.statement())?;
                automatic_reads
                    .entry(value.span())
                    .or_insert_with(|| vec![statement]);
            }
        }
        for (site, mut proof) in pending_automatic {
            cancellation.check()?;
            work.consume()?;
            let Some(reads) = automatic_reads.get(&site.source.span()) else {
                continue;
            };
            for source in reads {
                cancellation.check()?;
                work.consume()?;
                proof.read_sources.push(source.clone());
            }
            proof.read_sources.sort_unstable();
            proof.read_sources.dedup();
            // This proves a conditional call at a value read, never eager
            // declaration-time construction or the variable's runtime value.
            result.automatic_initializers.insert(site.id, proof);
        }
        Ok(result)
    }

    pub(crate) fn proof(&self, occurrence: &OccurrenceRecord) -> Option<&Proof<'a>> {
        self.proofs.get(&occurrence.id)
    }

    pub(crate) fn initializer(&self, occurrence: &OccurrenceRecord) -> Option<&'a EntityRecord> {
        self.initializers.get(&occurrence.id).copied()
    }

    pub(crate) fn application_parameters(
        &self,
        occurrence: &OccurrenceRecord,
    ) -> &[&'a EntityRecord] {
        self.application_parameters
            .get(&occurrence.id)
            .map_or(&[], |parameters| parameters.as_slice())
    }

    pub(crate) fn receiver_sources(&self, occurrence: &OccurrenceRecord) -> &[SourceRef] {
        self.receiver_sources
            .get(&occurrence.id)
            .map_or(&[], |sources| sources.as_slice())
    }

    pub(crate) fn is_call(&self, occurrence: &OccurrenceRecord) -> bool {
        self.proof(occurrence).is_some_and(|proof| {
            proof.property_call
                || (matches!(
                    occurrence.syntax_kind.as_str(),
                    "vba.invoked_member.reference" | "vba.invoked_get_member.reference"
                ) && matches!(proof.target.kind, EntityKind::Method | EntityKind::Function))
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
                rule: proof.rule,
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

    #[test]
    fn member_paths_preserve_exact_receivers_and_share_work_limits() {
        let source = b"[Owner] _\n.[Child] _\r\n. _\n [Value]";
        let mut work = ResolutionWorkBudget::new(1024);
        assert_eq!(
            member_access_parts(source, &Cancellation::new(), &mut work).unwrap(),
            Some(("[Owner] _\n.[Child]", "Value"))
        );
        let consumed = work.consumed();
        assert!(consumed > 1);
        assert!(
            member_access_parts(
                source,
                &Cancellation::new(),
                &mut ResolutionWorkBudget::new(consumed)
            )
            .unwrap()
            .is_some()
        );
        assert!(matches!(
            member_access_parts(
                source,
                &Cancellation::new(),
                &mut ResolutionWorkBudget::new(consumed - 1)
            ),
            Err(ResolutionError::WorkLimit { .. })
        ));
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut unused = ResolutionWorkBudget::new(1024);
        assert!(matches!(
            member_access_parts(source, &cancelled, &mut unused),
            Err(ResolutionError::Cancelled(_))
        ));
        assert_eq!(unused.consumed(), 0);
    }

    #[test]
    fn member_paths_do_not_join_uncontinued_or_partial_expressions() {
        for source in [
            "Owner .Value",
            "Owner\n.Value",
            "Owner _\r.Value",
            "Owner.Value, next",
            "Owner.Value(0)",
            "Owner.Value'comment",
            "Owner",
            "Owner..Value",
            "[Owner.Value]",
            "Owner.[",
        ] {
            assert_eq!(
                member_access_parts(
                    source.as_bytes(),
                    &Cancellation::new(),
                    &mut ResolutionWorkBudget::new(1024)
                )
                .unwrap(),
                None,
                "{source:?}"
            );
        }
    }

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
