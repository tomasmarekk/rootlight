//! Exact F# module bindings within a source-declared project order.
//! Native contexts, conserved visibility/open graphs and project order are proofs;
//! neither spelling scores nor indexed presence establish a target or build closure.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityFlag, EntityKind, EntityRecord, EntityVisibility, ExtensionEnvelope,
    FSHARP_BINDING_NAMESPACE, FSharpBinding, FileRecord, OccurrenceRecord, OccurrenceRole,
    ProducerKind, ProvenanceRecord, SourceSpan, decode_fsharp_binding_envelope,
};

use crate::{
    CandidateExplanation, CompletenessAssumption, RESOLVER_PROVIDER_NAME,
    RESOLVER_PROVIDER_VERSION, ResolutionDecision, ResolutionError, ResolutionExplanation,
    ResolutionOutcome, ResolutionRule, ResolutionSignal,
};

#[derive(Clone, Copy)]
struct Export<'a> {
    entity: &'a EntityRecord,
    envelope: &'a ExtensionEnvelope,
    slots: Option<u32>,
    file: FileId,
}

struct Use<'a> {
    path: UsePath,
    slots: Option<u32>,
    envelope: &'a ExtensionEnvelope,
    proof: Option<Proof<'a>>,
}

enum UsePath {
    Qualified(Vec<ContentHash>),
    ImportedQualified(Vec<ContentHash>),
    Opened(ContentHash),
}

struct Proof<'a> {
    symbol: SymbolId,
    base: [&'a ExtensionEnvelope; 5],
    project_graph: Option<FileId>,
    imports: Vec<&'a ExtensionEnvelope>,
    signature_masked: bool,
    type_masked: bool,
    alias_masked: bool,
    opened_spelling: Option<[ContentHash; 2]>,
}

#[derive(Clone, Copy)]
struct SignatureValue<'a> {
    envelope: &'a ExtensionEnvelope,
    symbol: SymbolId,
}

#[derive(Clone, Copy)]
struct SignatureType<'a> {
    envelope: &'a ExtensionEnvelope,
    entity: &'a EntityRecord,
}

#[derive(Clone, Copy)]
struct OwnedType<'a> {
    entity: &'a EntityRecord,
    module: &'a EntityRecord,
    container: &'a ExtensionEnvelope,
}

type FileExports<'a> = BTreeMap<FileId, Vec<(Vec<ContentHash>, Export<'a>)>>;
type SignatureValues<'a> = BTreeMap<FileId, BTreeMap<Vec<ContentHash>, Option<SignatureValue<'a>>>>;
type SignatureTypes<'a> = BTreeMap<FileId, BTreeMap<Vec<ContentHash>, Option<SignatureType<'a>>>>;
type OwnedTypes<'a> = BTreeMap<FileId, BTreeMap<Vec<ContentHash>, Option<OwnedType<'a>>>>;
type ContainerPaths<'g, 'a> = BTreeMap<&'g [ContentHash], Option<(SourceSpan, &'g Container<'a>)>>;
type OrderedPair = (Option<(FileId, usize)>, Option<(FileId, usize)>);

#[derive(Default)]
struct SignatureSurface<'a> {
    values: SignatureValues<'a>,
    types: SignatureTypes<'a>,
    owned_types: OwnedTypes<'a>,
}

struct Container<'a> {
    path: Vec<ContentHash>,
    roots: Vec<ContentHash>,
    envelope: &'a ExtensionEnvelope,
}

struct Open<'a> {
    path: Vec<ContentHash>,
    owner: SourceSpan,
    envelope: &'a ExtensionEnvelope,
    target: Option<SourceSpan>,
}

struct Manifest<'a> {
    containers: Vec<SourceSpan>,
    opens: Vec<SourceSpan>,
    envelope: &'a ExtensionEnvelope,
}

#[derive(Default)]
struct GraphFile<'a> {
    containers: BTreeMap<SourceSpan, Option<Container<'a>>>,
    opens: BTreeMap<SourceSpan, Option<Open<'a>>>,
    manifest: Option<Option<Manifest<'a>>>,
    complete: bool,
}

#[derive(Default)]
struct ProjectGraph<'a> {
    complete: bool,
    evidence: Vec<&'a ExtensionEnvelope>,
    entities: Vec<&'a EntityRecord>,
}

#[derive(Clone, Copy)]
struct VisibleContainer {
    ordinal: usize,
    source: SourceSpan,
    components: usize,
    declaration_start: u64,
}

#[derive(Default)]
pub(crate) struct FSharpIndex<'a> {
    contexts: BTreeMap<FileId, Option<&'a ExtensionEnvelope>>,
    uses: BTreeMap<(SourceSpan, bool), Option<Use<'a>>>,
    membership: BTreeMap<FileId, Option<(FileId, usize)>>,
    projects: BTreeMap<FileId, Option<&'a ExtensionEnvelope>>,
    exports: BTreeMap<FileId, BTreeMap<Vec<ContentHash>, Option<Export<'a>>>>,
    project_graphs: BTreeMap<FileId, ProjectGraph<'a>>,
    signature_for: BTreeMap<FileId, FileId>,
    signature_files: BTreeSet<FileId>,
    mask_inputs: BTreeMap<FileId, Vec<&'a ExtensionEnvelope>>,
    mask_entities: BTreeMap<FileId, Vec<&'a EntityRecord>>,
    alias_mask_projects: BTreeSet<FileId>,
    masked_containers: BTreeMap<SourceSpan, SourceSpan>,
    preparation_work: usize,
}

impl<'a> FSharpIndex<'a> {
    pub(crate) fn build(
        files: &BTreeMap<FileId, &'a FileRecord>,
        entities: &BTreeMap<SymbolId, &'a EntityRecord>,
        provenance: &BTreeMap<FactId, &'a ProvenanceRecord>,
        extensions: &'a [ExtensionEnvelope],
        cancellation: &Cancellation,
    ) -> Result<Self, ResolutionError> {
        let mut index = Self::default();
        let mut exports = FileExports::new();
        let mut projects = BTreeMap::<FileId, (Vec<FileId>, &'a ExtensionEnvelope)>::new();
        let mut graph = BTreeMap::<FileId, GraphFile<'a>>::new();
        let mut signatures = SignatureSurface::default();
        for envelope in extensions {
            cancellation.check()?;
            if envelope.namespace != FSHARP_BINDING_NAMESPACE {
                continue;
            }
            index.charge(1)?;
            let claim = decode_fsharp_binding_envelope(envelope)
                .map_err(|_| ResolutionError::UnsupportedIdentityRemap)?;
            let source = envelope
                .evidence
                .source
                .as_ref()
                .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
            let file = files
                .get(&claim.scope().file())
                .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
            if claim.scope().start_byte() != 0
                || claim.scope().end_byte() != file.byte_length
                || source.content_hash() != file.content_hash
                || envelope.provenance != file.provenance
                || source.repository() != file.repository
                || source.generation() != file.generation
            {
                return Err(ResolutionError::UnsupportedIdentityRemap);
            }
            if let FSharpBinding::Project { members } = claim.binding() {
                index.charge(members.len())?;
                if !suffix(&file.path, "fsproj") {
                    return Err(ResolutionError::UnsupportedIdentityRemap);
                }
                // A second claim cannot silently select one compilation profile.
                index
                    .projects
                    .entry(file.id)
                    .and_modify(|project| *project = None)
                    .or_insert(Some(envelope));
                let mut retained = Vec::new();
                retained
                    .try_reserve_exact(members.len())
                    .map_err(|_| ResolutionError::CountOverflow)?;
                retained.extend_from_slice(members);
                projects.insert(file.id, (retained, envelope));
                continue;
            }
            if !matches!(file.language.as_str(), "fsharp" | "fsharp-signature")
                || !provenance.get(&envelope.provenance).is_some_and(|record| {
                    record.producer_kind == ProducerKind::Parser && record.language == file.language
                })
            {
                return Err(ResolutionError::UnsupportedIdentityRemap);
            }
            match claim.binding() {
                FSharpBinding::Context => {
                    index
                        .contexts
                        .entry(file.id)
                        .and_modify(|context| *context = None)
                        .or_insert(Some(envelope));
                }
                FSharpBinding::Export {
                    path,
                    symbol,
                    slots,
                } => {
                    if file.language != "fsharp" {
                        return Err(ResolutionError::UnsupportedIdentityRemap);
                    }
                    index.charge(path.len())?;
                    let entity = entities
                        .get(symbol)
                        .copied()
                        .filter(|entity| {
                            entity.language == "fsharp"
                                && matches!(
                                    entity.kind,
                                    EntityKind::Function | EntityKind::Variable
                                )
                                && !matches!(
                                    entity.visibility,
                                    EntityVisibility::Private | EntityVisibility::Restricted
                                )
                                && (entity.kind == EntityKind::Function || slots.is_none())
                                && entity.provenance == envelope.provenance
                                && canonical_identifier(&entity.canonical_name).is_some_and(
                                    |name| path.last() == Some(&content_hash(name.as_bytes())),
                                )
                                && entity.evidence.source.as_ref().is_some_and(|owner| {
                                    owner.repository() == source.repository()
                                        && owner.generation() == source.generation()
                                        && owner.content_hash() == source.content_hash()
                                        && owner.span().file() == file.id
                                        && owner.span().start_byte() <= source.span().start_byte()
                                        && source.span().end_byte() <= owner.span().end_byte()
                                })
                        })
                        .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
                    let entries = exports.entry(file.id).or_default();
                    entries
                        .try_reserve(1)
                        .map_err(|_| ResolutionError::CountOverflow)?;
                    entries.push((
                        copy_path(path)?,
                        Export {
                            entity,
                            envelope,
                            slots: *slots,
                            file: file.id,
                        },
                    ));
                }
                FSharpBinding::SignatureValue { path, symbol } => {
                    index.charge(path.len())?;
                    if file.language != "fsharp-signature"
                        || !entities.get(symbol).is_some_and(|entity| {
                            entity.language == "fsharp-signature"
                                && entity.kind == EntityKind::Variable
                                && !matches!(
                                    entity.visibility,
                                    EntityVisibility::Private | EntityVisibility::Restricted
                                )
                                && entity.provenance == envelope.provenance
                                && canonical_identifier(&entity.canonical_name).is_some_and(
                                    |name| path.last() == Some(&content_hash(name.as_bytes())),
                                )
                                && entity.evidence.source.as_ref().is_some_and(|owner| {
                                    owner.repository() == source.repository()
                                        && owner.generation() == source.generation()
                                        && owner.content_hash() == source.content_hash()
                                        && owner.span().file() == file.id
                                        && span_contains(owner.span(), source.span())
                                })
                        })
                    {
                        return Err(ResolutionError::UnsupportedIdentityRemap);
                    }
                    signatures
                        .values
                        .entry(file.id)
                        .or_default()
                        .entry(copy_path(path)?)
                        .and_modify(|prior| *prior = None)
                        .or_insert(Some(SignatureValue {
                            envelope,
                            symbol: *symbol,
                        }));
                }
                FSharpBinding::SignatureType { path, symbol }
                | FSharpBinding::SignatureAlias { path, symbol } => {
                    index.charge(path.len())?;
                    let expected_kind =
                        if matches!(claim.binding(), FSharpBinding::SignatureAlias { .. }) {
                            EntityKind::TypeAlias
                        } else {
                            EntityKind::Struct
                        };
                    let entity = entities
                        .get(symbol)
                        .copied()
                        .filter(|entity| {
                            file.language == "fsharp-signature"
                                && entity.language == file.language
                                && entity.kind == expected_kind
                                && !matches!(
                                    entity.visibility,
                                    EntityVisibility::Private | EntityVisibility::Restricted
                                )
                                && !entity.flags.contains(&EntityFlag::Synthetic)
                                && entity.provenance == envelope.provenance
                                && canonical_identifier(&entity.canonical_name).is_some_and(
                                    |name| path.last() == Some(&content_hash(name.as_bytes())),
                                )
                                && entity.evidence.source.as_ref().is_some_and(|owner| {
                                    owner.repository() == source.repository()
                                        && owner.generation() == source.generation()
                                        && owner.content_hash() == source.content_hash()
                                        && owner.span().file() == file.id
                                        && span_contains(source.span(), owner.span())
                                })
                        })
                        .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
                    signatures
                        .types
                        .entry(file.id)
                        .or_default()
                        .entry(copy_path(path)?)
                        .and_modify(|prior| *prior = None)
                        .or_insert(Some(SignatureType { envelope, entity }));
                }
                FSharpBinding::Reference { path }
                | FSharpBinding::Call { path, .. }
                | FSharpBinding::ImportedReference { path }
                | FSharpBinding::ImportedCall { path, .. } => {
                    if file.language != "fsharp" {
                        return Err(ResolutionError::UnsupportedIdentityRemap);
                    }
                    index.charge(path.len())?;
                    let slots = match claim.binding() {
                        FSharpBinding::Call { slots, .. }
                        | FSharpBinding::ImportedCall { slots, .. } => Some(*slots),
                        _ => None,
                    };
                    index
                        .uses
                        .entry((source.span(), slots.is_some()))
                        .and_modify(|prior| *prior = None)
                        .or_insert(Some(Use {
                            path: if matches!(
                                claim.binding(),
                                FSharpBinding::ImportedReference { .. }
                                    | FSharpBinding::ImportedCall { .. }
                            ) {
                                UsePath::ImportedQualified(copy_path(path)?)
                            } else {
                                UsePath::Qualified(copy_path(path)?)
                            },
                            slots,
                            envelope,
                            proof: None,
                        }));
                }
                FSharpBinding::OpenedReference { name }
                | FSharpBinding::OpenedCall { name, .. } => {
                    if file.language != "fsharp" {
                        return Err(ResolutionError::UnsupportedIdentityRemap);
                    }
                    index.charge(1)?;
                    let slots = match claim.binding() {
                        FSharpBinding::OpenedCall { slots, .. } => Some(*slots),
                        _ => None,
                    };
                    index
                        .uses
                        .entry((source.span(), slots.is_some()))
                        .and_modify(|prior| *prior = None)
                        .or_insert(Some(Use {
                            path: UsePath::Opened(*name),
                            slots,
                            envelope,
                            proof: None,
                        }));
                }
                FSharpBinding::Container { path, roots } => {
                    index.charge(
                        path.len()
                            .checked_add(roots.len())
                            .ok_or(ResolutionError::CountOverflow)?,
                    )?;
                    graph
                        .entry(file.id)
                        .or_default()
                        .containers
                        .entry(source.span())
                        .and_modify(|prior| *prior = None)
                        .or_insert(Some(Container {
                            path: copy_path(path)?,
                            roots: copy_path(roots)?,
                            envelope,
                        }));
                }
                FSharpBinding::Open { path, owner } => {
                    index.charge(path.len())?;
                    graph
                        .entry(file.id)
                        .or_default()
                        .opens
                        .entry(source.span())
                        .and_modify(|prior| *prior = None)
                        .or_insert(Some(Open {
                            path: copy_path(path)?,
                            owner: *owner,
                            envelope,
                            target: None,
                        }));
                }
                FSharpBinding::ImportContext { containers, opens } => {
                    index.charge(
                        containers
                            .len()
                            .checked_add(opens.len())
                            .ok_or(ResolutionError::CountOverflow)?,
                    )?;
                    let manifest = Manifest {
                        containers: copy_spans(containers)?,
                        opens: copy_spans(opens)?,
                        envelope,
                    };
                    let slot = &mut graph.entry(file.id).or_default().manifest;
                    *slot = if slot.is_none() {
                        Some(Some(manifest))
                    } else {
                        Some(None)
                    };
                }
                FSharpBinding::Project { .. } => {
                    unreachable!("project claims were handled before native claims")
                }
            }
        }
        for (&project, (members, _)) in &projects {
            cancellation.check()?;
            let mut complete = index.projects.get(&project).is_some_and(Option::is_some);
            for (ordinal, &member) in members.iter().enumerate() {
                cancellation.check()?;
                index.charge(1)?;
                complete &= files.get(&member).is_some_and(|file| {
                    (file.language == "fsharp" && suffix(&file.path, "fs"))
                        || (file.language == "fsharp-signature" && suffix(&file.path, "fsi"))
                }) && index.contexts.get(&member).is_some_and(Option::is_some);
                index
                    .membership
                    .entry(member)
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some((project, ordinal)));
            }
            if !complete {
                index.projects.insert(project, None);
            }
        }
        index.validate_graph(&mut graph, cancellation)?;
        if !signatures.types.is_empty() {
            signatures.owned_types =
                index.owned_types(files, entities, provenance, &graph, cancellation)?;
        }
        index.prepare_signature_masks(
            files,
            &projects,
            &exports,
            &signatures,
            &graph,
            cancellation,
        )?;
        for (file, entries) in exports {
            cancellation.check()?;
            let Some((project, _)) = index.membership.get(&file).copied().flatten() else {
                continue;
            };
            for (path, export) in entries {
                cancellation.check()?;
                index.charge(1)?;
                if let Some(signature) = index.signature_for.get(&file)
                    && !signatures.values.get(signature).is_some_and(|values| {
                        values.get(path.as_slice()).is_some_and(Option::is_some)
                    })
                {
                    continue;
                }
                index
                    .exports
                    .entry(project)
                    .or_default()
                    .entry(path)
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(export));
            }
        }
        index.prepare_import_graph(&projects, &mut graph, cancellation)?;
        let uses = std::mem::take(&mut index.uses);
        for (key, mut usage) in uses {
            cancellation.check()?;
            if let Some(usage) = usage.as_mut() {
                usage.proof = index.prepare_proof(key.0, usage, &graph, cancellation)?;
            }
            index.uses.insert(key, usage);
        }
        Ok(index)
    }

    fn owned_types(
        &mut self,
        files: &BTreeMap<FileId, &'a FileRecord>,
        entities: &BTreeMap<SymbolId, &'a EntityRecord>,
        provenance: &BTreeMap<FactId, &'a ProvenanceRecord>,
        graph: &BTreeMap<FileId, GraphFile<'a>>,
        cancellation: &Cancellation,
    ) -> Result<OwnedTypes<'a>, ResolutionError> {
        let mut types = OwnedTypes::new();
        let mut modules = BTreeMap::<SourceSpan, Option<&'a EntityRecord>>::new();
        for &module in entities.values() {
            cancellation.check()?;
            self.charge(1)?;
            if module.kind != EntityKind::Module
                || module.flags.contains(&EntityFlag::Synthetic)
                || matches!(
                    module.visibility,
                    EntityVisibility::Private | EntityVisibility::Restricted
                )
            {
                continue;
            }
            let Some(source) = module.evidence.source.as_ref() else {
                continue;
            };
            let Some(file) = files.get(&source.span().file()) else {
                continue;
            };
            if module.language != file.language || module.provenance != file.provenance {
                continue;
            }
            if graph
                .get(&file.id)
                .filter(|claims| claims.complete)
                .and_then(|claims| claims.containers.get(&source.span()))
                .and_then(Option::as_ref)
                .is_some_and(|container| {
                    container.envelope.evidence.source.as_ref() == Some(source)
                })
            {
                modules
                    .entry(source.span())
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(module));
            }
        }
        for &entity in entities.values() {
            cancellation.check()?;
            self.charge(1)?;
            if !matches!(entity.kind, EntityKind::Struct | EntityKind::TypeAlias)
                || entity.flags.contains(&EntityFlag::Synthetic)
                || matches!(
                    entity.visibility,
                    EntityVisibility::Private | EntityVisibility::Restricted
                )
            {
                continue;
            }
            let (Some(source), Some(ContainerRef::Entity(parent)), Some(name)) = (
                entity.evidence.source.as_ref(),
                entity.container,
                canonical_identifier(&entity.canonical_name),
            ) else {
                continue;
            };
            let Some(file) = files.get(&source.span().file()) else {
                continue;
            };
            if !matches!(file.language.as_str(), "fsharp" | "fsharp-signature")
                || entity.language != file.language
                || entity.provenance != file.provenance
                || !provenance.get(&file.provenance).is_some_and(|record| {
                    record.producer_kind == ProducerKind::Parser && record.language == file.language
                })
                || source.repository() != file.repository
                || source.generation() != file.generation
                || source.content_hash() != file.content_hash
            {
                continue;
            }
            let Some(module) = entities.get(&parent).copied().filter(|module| {
                module.kind == EntityKind::Module
                    && module.language == file.language
                    && module.provenance == file.provenance
                    && !module.flags.contains(&EntityFlag::Synthetic)
            }) else {
                continue;
            };
            let Some(owner) = module
                .evidence
                .source
                .as_ref()
                .filter(|owner| span_contains(owner.span(), source.span()))
            else {
                continue;
            };
            if modules
                .get(&owner.span())
                .copied()
                .flatten()
                .is_none_or(|unique| unique.id != module.id)
            {
                continue;
            }
            let Some(container) = graph
                .get(&file.id)
                .filter(|claims| claims.complete)
                .and_then(|claims| claims.containers.get(&owner.span()))
                .and_then(Option::as_ref)
                .filter(|container| container.envelope.evidence.source.as_ref() == Some(owner))
            else {
                continue;
            };
            let name = content_hash(name.as_bytes());
            if container.path.len() >= rootlight_ir::MAX_FSHARP_BINDING_COMPONENTS
                || container.roots.binary_search(&name).is_err()
            {
                continue;
            }
            self.charge(
                container
                    .path
                    .len()
                    .checked_add(1)
                    .ok_or(ResolutionError::CountOverflow)?,
            )?;
            let mut path = copy_path(&container.path)?;
            path.try_reserve(1)
                .map_err(|_| ResolutionError::CountOverflow)?;
            path.push(name);
            types
                .entry(file.id)
                .or_default()
                .entry(path)
                .and_modify(|prior| *prior = None)
                .or_insert(Some(OwnedType {
                    entity,
                    module,
                    container: container.envelope,
                }));
        }
        Ok(types)
    }

    fn prepare_signature_masks(
        &mut self,
        files: &BTreeMap<FileId, &'a FileRecord>,
        projects: &BTreeMap<FileId, (Vec<FileId>, &'a ExtensionEnvelope)>,
        exports: &FileExports<'a>,
        signatures: &SignatureSurface<'a>,
        graph: &BTreeMap<FileId, GraphFile<'a>>,
        cancellation: &Cancellation,
    ) -> Result<(), ResolutionError> {
        let mut export_paths = BTreeMap::new();
        for (&file, entries) in exports {
            cancellation.check()?;
            let mut paths = BTreeMap::<&[ContentHash], Option<Export<'a>>>::new();
            for (path, export) in entries {
                cancellation.check()?;
                self.charge(1)?;
                paths
                    .entry(path.as_slice())
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(*export));
            }
            export_paths.insert(file, paths);
        }
        for (&project, (members, _)) in projects {
            cancellation.check()?;
            if !self.projects.get(&project).is_some_and(Option::is_some) {
                continue;
            }
            let mut stems = BTreeMap::<&str, OrderedPair>::new();
            let mut complete = true;
            let mut has_signature = false;
            for (ordinal, &member) in members.iter().enumerate() {
                cancellation.check()?;
                self.charge(1)?;
                let file = files
                    .get(&member)
                    .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
                let (stem, extension) = file
                    .path
                    .rsplit_once('.')
                    .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
                let signature = extension.eq_ignore_ascii_case("fsi");
                has_signature |= signature;
                let pair = stems.entry(stem).or_default();
                let slot = if signature { &mut pair.0 } else { &mut pair.1 };
                complete &= slot.replace((member, ordinal)).is_none()
                    && self.membership.get(&member) == Some(&Some((project, ordinal)));
            }
            if !has_signature {
                continue;
            }
            let mut pairs = Vec::new();
            let mut inputs = Vec::new();
            let mut entity_inputs = Vec::new();
            let mut entity_ids = BTreeSet::new();
            let mut alias_masked = false;
            let mut containers = Vec::new();
            for &(signature, implementation) in stems.values() {
                cancellation.check()?;
                let Some((signature, signature_order)) = signature else {
                    continue;
                };
                let Some((implementation, implementation_order)) = implementation else {
                    complete = false;
                    continue;
                };
                if signature_order >= implementation_order {
                    complete = false;
                    continue;
                }
                let (Some(signature_graph), Some(implementation_graph)) = (
                    graph.get(&signature).filter(|claims| claims.complete),
                    graph.get(&implementation).filter(|claims| claims.complete),
                ) else {
                    complete = false;
                    continue;
                };
                let signature_containers =
                    self.containers_by_path(signature_graph, cancellation)?;
                let implementation_containers =
                    self.containers_by_path(implementation_graph, cancellation)?;
                let signature_outer = self.outer_paths(&signature_containers, cancellation)?;
                let implementation_outer =
                    self.outer_paths(&implementation_containers, cancellation)?;
                complete &= !signature_outer.is_empty() && signature_outer == implementation_outer;
                let mut roots = BTreeMap::<&[ContentHash], BTreeMap<ContentHash, usize>>::new();
                for (&path, container) in &signature_containers {
                    cancellation.check()?;
                    self.charge(1)?;
                    let (
                        Some((signature_span, signature_container)),
                        Some(Some((implementation_span, implementation_container))),
                    ) = (container, implementation_containers.get(path))
                    else {
                        complete = false;
                        continue;
                    };
                    roots.entry(path).or_default();
                    inputs
                        .try_reserve(2)
                        .map_err(|_| ResolutionError::CountOverflow)?;
                    inputs.extend([
                        signature_container.envelope,
                        implementation_container.envelope,
                    ]);
                    containers
                        .try_reserve(1)
                        .map_err(|_| ResolutionError::CountOverflow)?;
                    containers.push((*signature_span, *implementation_span));
                }
                // The roots authenticate absence as well as presence. A wholly
                // dropped val/type/module claim must not manufacture a hidden name.
                for &path in signature_containers.keys() {
                    cancellation.check()?;
                    self.charge(1)?;
                    if let Some((name, owner)) = path.split_last()
                        && let Some(names) = roots.get_mut(owner)
                    {
                        let count = names.entry(*name).or_default();
                        *count = count.checked_add(1).ok_or(ResolutionError::CountOverflow)?;
                    }
                }
                let mut value_sources = BTreeSet::new();
                let mut value_symbols = BTreeSet::new();
                if let Some(values) = signatures.values.get(&signature) {
                    for (path, value) in values {
                        cancellation.check()?;
                        self.charge(1)?;
                        let Some((name, owner)) = path.split_last() else {
                            complete = false;
                            continue;
                        };
                        let Some(names) = roots.get_mut(owner) else {
                            complete = false;
                            continue;
                        };
                        let count = names.entry(*name).or_default();
                        *count = count.checked_add(1).ok_or(ResolutionError::CountOverflow)?;
                        let (Some(value), Some(Some(export))) = (
                            value,
                            export_paths
                                .get(&implementation)
                                .and_then(|paths| paths.get(path.as_slice())),
                        ) else {
                            complete = false;
                            continue;
                        };
                        complete &= value_sources.insert(
                            value
                                .envelope
                                .evidence
                                .source
                                .as_ref()
                                .ok_or(ResolutionError::UnsupportedIdentityRemap)?
                                .span(),
                        ) && value_symbols.insert(value.symbol);
                        inputs
                            .try_reserve(2)
                            .map_err(|_| ResolutionError::CountOverflow)?;
                        inputs.extend([value.envelope, export.envelope]);
                    }
                }
                if let Some(types) = signatures.types.get(&signature) {
                    for (path, claim) in types {
                        cancellation.check()?;
                        self.charge(1)?;
                        let Some((name, owner)) = path.split_last() else {
                            complete = false;
                            continue;
                        };
                        let Some(names) = roots.get_mut(owner) else {
                            complete = false;
                            continue;
                        };
                        let count = names.entry(*name).or_default();
                        *count = count.checked_add(1).ok_or(ResolutionError::CountOverflow)?;
                        let (Some(claim), Some(Some(declaration)), Some(Some(counterpart))) = (
                            claim,
                            signatures
                                .owned_types
                                .get(&signature)
                                .and_then(|types| types.get(path.as_slice())),
                            signatures
                                .owned_types
                                .get(&implementation)
                                .and_then(|types| types.get(path.as_slice())),
                        ) else {
                            complete = false;
                            continue;
                        };
                        // Names and kinds conserve public roots, not alias RHS
                        // equivalence, expansion or executable member semantics.
                        complete &= declaration.entity.id == claim.entity.id
                            && declaration.entity.kind == counterpart.entity.kind
                            && value_sources.insert(
                                claim
                                    .envelope
                                    .evidence
                                    .source
                                    .as_ref()
                                    .ok_or(ResolutionError::UnsupportedIdentityRemap)?
                                    .span(),
                            )
                            && value_symbols.insert(claim.entity.id);
                        alias_masked |= claim.entity.kind == EntityKind::TypeAlias;
                        inputs
                            .try_reserve(3)
                            .map_err(|_| ResolutionError::CountOverflow)?;
                        inputs.extend([
                            claim.envelope,
                            declaration.container,
                            counterpart.container,
                        ]);
                        for entity in [
                            declaration.entity,
                            declaration.module,
                            counterpart.entity,
                            counterpart.module,
                        ] {
                            cancellation.check()?;
                            self.charge(1)?;
                            if entity_ids.insert(entity.id) {
                                entity_inputs
                                    .try_reserve(1)
                                    .map_err(|_| ResolutionError::CountOverflow)?;
                                entity_inputs.push(entity);
                            }
                        }
                    }
                }
                for (&path, container) in &signature_containers {
                    cancellation.check()?;
                    let Some((_, container)) = container else {
                        continue;
                    };
                    let Some(names) = roots.get(path) else {
                        complete = false;
                        continue;
                    };
                    complete &= names.len() == container.roots.len();
                    for (expected, (actual, count)) in container.roots.iter().zip(names) {
                        cancellation.check()?;
                        self.charge(1)?;
                        complete &= expected == actual && *count == 1;
                    }
                }
                pairs
                    .try_reserve(1)
                    .map_err(|_| ResolutionError::CountOverflow)?;
                pairs.push((implementation, signature));
            }
            if !complete {
                self.projects.insert(project, None);
                continue;
            }
            for (implementation, signature) in pairs {
                self.signature_for.insert(implementation, signature);
                self.signature_files.insert(signature);
            }
            for (signature, implementation) in containers {
                self.masked_containers.insert(signature, implementation);
            }
            self.mask_inputs.insert(project, inputs);
            if alias_masked {
                self.alias_mask_projects.insert(project);
            }
            if !entity_inputs.is_empty() {
                self.mask_entities.insert(project, entity_inputs);
            }
        }
        Ok(())
    }

    fn containers_by_path<'g>(
        &mut self,
        graph: &'g GraphFile<'a>,
        cancellation: &Cancellation,
    ) -> Result<ContainerPaths<'g, 'a>, ResolutionError> {
        let mut paths = BTreeMap::new();
        for (&source, container) in &graph.containers {
            cancellation.check()?;
            self.charge(1)?;
            if let Some(container) = container {
                paths
                    .entry(container.path.as_slice())
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some((source, container)));
            }
        }
        Ok(paths)
    }

    fn outer_paths<'g>(
        &mut self,
        paths: &ContainerPaths<'g, 'a>,
        cancellation: &Cancellation,
    ) -> Result<BTreeSet<&'g [ContentHash]>, ResolutionError> {
        let mut outer = BTreeSet::new();
        for &path in paths.keys() {
            cancellation.check()?;
            let mut has_parent = false;
            for end in 1..path.len() {
                cancellation.check()?;
                self.charge(1)?;
                has_parent |= path
                    .get(..end)
                    .is_some_and(|prefix| paths.contains_key(prefix));
            }
            if !has_parent {
                outer.insert(path);
            }
        }
        Ok(outer)
    }

    fn prepare_import_graph(
        &mut self,
        projects: &BTreeMap<FileId, (Vec<FileId>, &'a ExtensionEnvelope)>,
        graph: &mut BTreeMap<FileId, GraphFile<'a>>,
        cancellation: &Cancellation,
    ) -> Result<(), ResolutionError> {
        for (&project, (members, _)) in projects {
            cancellation.check()?;
            let mut proof = ProjectGraph {
                complete: self.projects.get(&project).is_some_and(Option::is_some),
                evidence: Vec::new(),
                entities: Vec::new(),
            };
            let mask_count = self.mask_inputs.get(&project).map_or(0, Vec::len);
            proof
                .evidence
                .try_reserve_exact(
                    members
                        .len()
                        .checked_mul(2)
                        .and_then(|count| count.checked_add(mask_count))
                        .ok_or(ResolutionError::CountOverflow)?,
                )
                .map_err(|_| ResolutionError::CountOverflow)?;
            for (ordinal, &member) in members.iter().enumerate() {
                cancellation.check()?;
                self.charge(1)?;
                let complete = graph.get(&member).filter(|file| file.complete);
                let context = self.contexts.get(&member).copied().flatten();
                let manifest = complete
                    .and_then(|file| file.manifest.as_ref())
                    .and_then(Option::as_ref);
                proof.complete &= self.membership.get(&member) == Some(&Some((project, ordinal)))
                    && context.is_some()
                    && manifest.is_some();
                if let (Some(context), Some(manifest)) = (context, manifest) {
                    proof.evidence.extend([context, manifest.envelope]);
                }
            }
            if let Some(inputs) = self.mask_inputs.get(&project) {
                proof.evidence.extend_from_slice(inputs);
            }
            self.charge(mask_count)?;
            if let Some(entities) = self.mask_entities.get(&project) {
                proof
                    .entities
                    .try_reserve_exact(entities.len())
                    .map_err(|_| ResolutionError::CountOverflow)?;
                proof.entities.extend_from_slice(entities);
            }
            self.charge(proof.entities.len())?;
            self.project_graphs.insert(project, proof);
        }
        // Index suffixes once: a written open must not silently select an exact
        // path while another visible authored container shares that suffix.
        let mut suffixes =
            BTreeMap::<FileId, BTreeMap<Vec<ContentHash>, Vec<VisibleContainer>>>::new();
        for (&file, original_claims) in graph.iter() {
            cancellation.check()?;
            let Some((project, ordinal)) = self.membership.get(&file).copied().flatten() else {
                continue;
            };
            if self.signature_files.contains(&file) {
                continue;
            }
            let claims = self
                .signature_for
                .get(&file)
                .and_then(|signature| graph.get(signature))
                .unwrap_or(original_claims);
            if !claims.complete
                || !self
                    .project_graphs
                    .get(&project)
                    .is_some_and(|proof| proof.complete)
            {
                continue;
            }
            for (&source, container) in &claims.containers {
                cancellation.check()?;
                let Some(container) = container else { continue };
                let declaration_start = if self.signature_for.contains_key(&file) {
                    self.masked_containers
                        .get(&source)
                        .map(|implementation| implementation.start_byte())
                        .ok_or(ResolutionError::UnsupportedIdentityRemap)?
                } else {
                    source.start_byte()
                };
                for start in 0..container.path.len() {
                    cancellation.check()?;
                    let path = container
                        .path
                        .get(start..)
                        .ok_or(ResolutionError::CountOverflow)?;
                    self.charge(
                        path.len()
                            .checked_add(1)
                            .ok_or(ResolutionError::CountOverflow)?,
                    )?;
                    let entries = suffixes
                        .entry(project)
                        .or_default()
                        .entry(copy_path(path)?)
                        .or_default();
                    entries
                        .try_reserve(1)
                        .map_err(|_| ResolutionError::CountOverflow)?;
                    entries.push(VisibleContainer {
                        ordinal,
                        source,
                        components: container.path.len(),
                        declaration_start,
                    });
                }
            }
        }
        for (&file, claims) in graph.iter_mut() {
            cancellation.check()?;
            let Some((project, caller_order)) = self.membership.get(&file).copied().flatten()
            else {
                continue;
            };
            if !claims.complete
                || !self
                    .project_graphs
                    .get(&project)
                    .is_some_and(|proof| proof.complete)
            {
                continue;
            }
            for (&clause, open) in claims
                .opens
                .iter_mut()
                .filter_map(|(source, open)| open.as_mut().map(|open| (source, open)))
            {
                cancellation.check()?;
                let Some(candidates) = suffixes
                    .get(&project)
                    .and_then(|paths| paths.get(open.path.as_slice()))
                else {
                    continue;
                };
                let mut count = 0usize;
                let mut target = None;
                for candidate in candidates {
                    cancellation.check()?;
                    self.charge(1)?;
                    if candidate.ordinal > caller_order {
                        continue;
                    }
                    if candidate.ordinal == caller_order {
                        if candidate.declaration_start <= clause.start_byte() {
                            count = count.checked_add(1).ok_or(ResolutionError::CountOverflow)?;
                        }
                        continue;
                    }
                    count = count.checked_add(1).ok_or(ResolutionError::CountOverflow)?;
                    if candidate.components == open.path.len() {
                        target = Some(candidate.source);
                    }
                }
                if count == 1 {
                    open.target = target;
                }
            }
        }
        Ok(())
    }

    fn validate_graph(
        &mut self,
        graph: &mut BTreeMap<FileId, GraphFile<'a>>,
        cancellation: &Cancellation,
    ) -> Result<(), ResolutionError> {
        for file in graph.values_mut() {
            cancellation.check()?;
            let Some(manifest) = file.manifest.as_ref().and_then(Option::as_ref) else {
                continue;
            };
            let mut complete = manifest.containers.len() == file.containers.len()
                && manifest.opens.len() == file.opens.len();
            for (expected, (actual, container)) in manifest.containers.iter().zip(&file.containers)
            {
                cancellation.check()?;
                self.charge(1)?;
                complete &= expected == actual && container.is_some();
            }
            for (expected, (actual, open)) in manifest.opens.iter().zip(&file.opens) {
                cancellation.check()?;
                self.charge(1)?;
                complete &= expected == actual
                    && open.as_ref().is_some_and(|open| {
                        file.containers
                            .get(&open.owner)
                            .is_some_and(Option::is_some)
                    });
            }
            file.complete = complete;
        }
        Ok(())
    }

    fn prepare_proof(
        &mut self,
        source: SourceSpan,
        usage: &Use<'a>,
        graph: &BTreeMap<FileId, GraphFile<'a>>,
        cancellation: &Cancellation,
    ) -> Result<Option<Proof<'a>>, ResolutionError> {
        cancellation.check()?;
        let (path, imported) = match &usage.path {
            UsePath::Qualified(path) => (path.as_slice(), false),
            UsePath::ImportedQualified(path) => (path.as_slice(), true),
            UsePath::Opened(name) => {
                return self.prepare_opened_proof(source, *name, usage, graph, cancellation);
            }
        };
        let Some((project, caller_order)) = self.membership.get(&source.file()).copied().flatten()
        else {
            return Ok(None);
        };
        let Some(project_envelope) = self.projects.get(&project).copied().flatten() else {
            return Ok(None);
        };
        let Some(export) = self
            .exports
            .get(&project)
            .and_then(|exports| exports.get(path))
            .copied()
            .flatten()
        else {
            return Ok(None);
        };
        let Some((target_project, target_order)) =
            self.membership.get(&export.file).copied().flatten()
        else {
            return Ok(None);
        };
        if project != target_project
            || target_order >= caller_order
            || export.file == source.file()
            || (usage.slots.is_some()
                && (export.entity.kind != EntityKind::Function || export.slots != usage.slots))
        {
            return Ok(None);
        }
        let (Some(caller_context), Some(target_context)) = (
            self.contexts.get(&source.file()).copied().flatten(),
            self.contexts.get(&export.file).copied().flatten(),
        ) else {
            return Ok(None);
        };
        let mut proof = Proof {
            symbol: export.entity.id,
            base: [
                usage.envelope,
                export.envelope,
                project_envelope,
                caller_context,
                target_context,
            ],
            project_graph: None,
            imports: Vec::new(),
            signature_masked: self.mask_inputs.contains_key(&project),
            type_masked: self.mask_entities.contains_key(&project),
            alias_masked: self.alias_mask_projects.contains(&project),
            opened_spelling: None,
        };
        let Some(claims) = graph.get(&source.file()) else {
            self.charge(proof.base.len())?;
            return Ok((!imported && !proof.signature_masked).then_some(proof));
        };
        if !claims.complete {
            return Ok(None);
        }
        if imported || proof.signature_masked {
            let Some(project_proof) = self
                .project_graphs
                .get(&project)
                .filter(|proof| proof.complete)
            else {
                return Ok(None);
            };
            let graph_work = project_proof
                .entities
                .len()
                .checked_mul(2)
                .and_then(|entities| entities.checked_add(project_proof.evidence.len()))
                .ok_or(ResolutionError::CountOverflow)?;
            self.charge(graph_work)?;
            proof.project_graph = Some(project);
        }
        let graph_count = proof
            .project_graph
            .and_then(|project| self.project_graphs.get(&project))
            .map(|graph| {
                graph
                    .entities
                    .len()
                    .checked_mul(2)
                    .and_then(|entities| entities.checked_add(graph.evidence.len()))
                    .ok_or(ResolutionError::CountOverflow)
            })
            .transpose()?
            .unwrap_or(0);
        // The occurrence and target add at most two distinct sources/parents.
        // Keep each public provenance record within the existing IR width.
        let maximum_evidence = rootlight_ir::IrLimits::default()
            .max_nested_items_per_record
            .saturating_sub(2);
        let Some(remaining_evidence) = maximum_evidence
            .checked_sub(proof.base.len())
            .and_then(|remaining| remaining.checked_sub(graph_count))
        else {
            return Ok(None);
        };
        let Some(root) = path.first() else {
            return Ok(None);
        };
        let mut active = 0usize;
        let mut inputs = BTreeSet::new();
        for (&clause, open) in &claims.opens {
            cancellation.check()?;
            self.charge(1)?;
            let Some(open) = open else { return Ok(None) };
            if clause.end_byte() > source.start_byte() || !span_contains(open.owner, source) {
                continue;
            }
            if !imported {
                return Ok(None);
            }
            active = active
                .checked_add(1)
                .ok_or(ResolutionError::CountOverflow)?;
            let Some(target) = open.target else {
                return Ok(None);
            };
            let Some(container) = graph
                .get(&target.file())
                .filter(|claims| claims.complete)
                .and_then(|claims| claims.containers.get(&target))
                .and_then(Option::as_ref)
            else {
                return Ok(None);
            };
            if container.roots.binary_search(root).is_ok() {
                return Ok(None);
            }
            let Some(owner) = claims.containers.get(&open.owner).and_then(Option::as_ref) else {
                return Ok(None);
            };
            for envelope in [open.envelope, owner.envelope, container.envelope] {
                cancellation.check()?;
                self.charge(1)?;
                if !inputs.contains(&envelope.id) {
                    if proof.imports.len() >= remaining_evidence {
                        return Ok(None);
                    }
                    inputs.insert(envelope.id);
                    proof
                        .imports
                        .try_reserve(1)
                        .map_err(|_| ResolutionError::CountOverflow)?;
                    proof.imports.push(envelope);
                }
            }
        }
        if imported && active == 0 {
            return Ok(None);
        }
        self.charge(proof.base.len())?;
        Ok(Some(proof))
    }

    fn prepare_opened_proof(
        &mut self,
        source: SourceSpan,
        name: ContentHash,
        usage: &Use<'a>,
        graph: &BTreeMap<FileId, GraphFile<'a>>,
        cancellation: &Cancellation,
    ) -> Result<Option<Proof<'a>>, ResolutionError> {
        cancellation.check()?;
        let Some((project, caller_order)) = self.membership.get(&source.file()).copied().flatten()
        else {
            return Ok(None);
        };
        let Some(project_envelope) = self.projects.get(&project).copied().flatten() else {
            return Ok(None);
        };
        let Some(caller_context) = self.contexts.get(&source.file()).copied().flatten() else {
            return Ok(None);
        };
        let Some(claims) = graph.get(&source.file()).filter(|claims| claims.complete) else {
            return Ok(None);
        };
        let Some(project_graph) = self
            .project_graphs
            .get(&project)
            .filter(|proof| proof.complete)
        else {
            return Ok(None);
        };
        let graph_work = project_graph
            .entities
            .len()
            .checked_mul(2)
            .and_then(|entities| entities.checked_add(project_graph.evidence.len()))
            .ok_or(ResolutionError::CountOverflow)?;
        self.charge(graph_work)?;
        let maximum_evidence = rootlight_ir::IrLimits::default()
            .max_nested_items_per_record
            .saturating_sub(2);
        let Some(remaining_evidence) = maximum_evidence
            .checked_sub(5)
            .and_then(|remaining| remaining.checked_sub(graph_work))
        else {
            return Ok(None);
        };
        let mut selected_owner = None;
        let mut selected_export = None;
        let mut inputs = BTreeSet::new();
        let mut imports = Vec::new();
        let mut path = Vec::new();
        for (&clause, open) in &claims.opens {
            cancellation.check()?;
            self.charge(1)?;
            let Some(open) = open else {
                return Ok(None);
            };
            if clause.end_byte() > source.start_byte() || !span_contains(open.owner, source) {
                continue;
            }
            let Some(target) = open.target else {
                return Ok(None);
            };
            let Some(container) = graph
                .get(&target.file())
                .filter(|claims| claims.complete)
                .and_then(|claims| claims.containers.get(&target))
                .and_then(Option::as_ref)
            else {
                return Ok(None);
            };
            if container.roots.binary_search(&name).is_ok() {
                // Conserve root ambiguity before checking value kind or arity.
                // A competing type/case/private root cannot disappear by filtering.
                if selected_owner.is_some_and(|owner| owner != target) {
                    return Ok(None);
                }
                if selected_owner.is_none() {
                    if container.path.len() >= rootlight_ir::MAX_FSHARP_BINDING_COMPONENTS {
                        return Ok(None);
                    }
                    self.charge(
                        container
                            .path
                            .len()
                            .checked_add(1)
                            .ok_or(ResolutionError::CountOverflow)?,
                    )?;
                    path.try_reserve(
                        container
                            .path
                            .len()
                            .checked_add(1)
                            .ok_or(ResolutionError::CountOverflow)?,
                    )
                    .map_err(|_| ResolutionError::CountOverflow)?;
                    path.extend_from_slice(&container.path);
                    path.push(name);
                    let Some(export) = self
                        .exports
                        .get(&project)
                        .and_then(|exports| exports.get(path.as_slice()))
                        .copied()
                        .flatten()
                    else {
                        return Ok(None);
                    };
                    let owner_file = self
                        .masked_containers
                        .get(&target)
                        .map_or(target.file(), |owner| owner.file());
                    if export.file != owner_file {
                        return Ok(None);
                    }
                    selected_owner = Some(target);
                    selected_export = Some(export);
                }
            }
            let Some(owner) = claims.containers.get(&open.owner).and_then(Option::as_ref) else {
                return Ok(None);
            };
            for envelope in [open.envelope, owner.envelope, container.envelope] {
                cancellation.check()?;
                self.charge(1)?;
                if !inputs.contains(&envelope.id) {
                    if imports.len() >= remaining_evidence {
                        return Ok(None);
                    }
                    inputs.insert(envelope.id);
                    imports
                        .try_reserve(1)
                        .map_err(|_| ResolutionError::CountOverflow)?;
                    imports.push(envelope);
                }
            }
        }
        let Some(export) = selected_export else {
            return Ok(None);
        };
        let Some((target_project, target_order)) =
            self.membership.get(&export.file).copied().flatten()
        else {
            return Ok(None);
        };
        if project != target_project
            || target_order >= caller_order
            || export.file == source.file()
            || usage.slots.is_some()
                && (export.entity.kind != EntityKind::Function || export.slots != usage.slots)
        {
            return Ok(None);
        }
        let Some(target_context) = self.contexts.get(&export.file).copied().flatten() else {
            return Ok(None);
        };
        let Some(canonical_name) = canonical_identifier(&export.entity.canonical_name) else {
            return Ok(None);
        };
        // Binding names omit backticks, but occurrences retain authored spelling.
        // Authenticate both spellings against the selected entity, not the claim.
        let quoted_length = canonical_name
            .len()
            .checked_add(4)
            .ok_or(ResolutionError::CountOverflow)?;
        self.charge(
            quoted_length
                .checked_add(canonical_name.len())
                .ok_or(ResolutionError::CountOverflow)?,
        )?;
        let mut quoted_name = String::new();
        quoted_name
            .try_reserve_exact(quoted_length)
            .map_err(|_| ResolutionError::CountOverflow)?;
        quoted_name.push_str("``");
        quoted_name.push_str(canonical_name);
        quoted_name.push_str("``");
        let proof = Proof {
            symbol: export.entity.id,
            base: [
                usage.envelope,
                export.envelope,
                project_envelope,
                caller_context,
                target_context,
            ],
            project_graph: Some(project),
            imports,
            signature_masked: self.mask_inputs.contains_key(&project),
            type_masked: self.mask_entities.contains_key(&project),
            alias_masked: self.alias_mask_projects.contains(&project),
            opened_spelling: Some([
                content_hash(canonical_name.as_bytes()),
                content_hash(quoted_name.as_bytes()),
            ]),
        };
        self.charge(proof.base.len())?;
        Ok(Some(proof))
    }

    fn charge(&mut self, amount: usize) -> Result<(), ResolutionError> {
        self.preparation_work = self
            .preparation_work
            .checked_add(amount)
            .ok_or(ResolutionError::CountOverflow)?;
        Ok(())
    }

    pub(crate) const fn preparation_work(&self) -> usize {
        self.preparation_work
    }

    fn proof(&self, occurrence: &OccurrenceRecord) -> Option<&Proof<'a>> {
        let call = match (occurrence.role, occurrence.syntax_kind.as_str()) {
            (OccurrenceRole::Reference, "fsharp.qualified_body_read.reference") => false,
            (OccurrenceRole::Reference, "fsharp.simple_function_body_read.reference") => false,
            (OccurrenceRole::CallSite, "fsharp.application.call") => true,
            _ => return None,
        };
        let usage = self.uses.get(&(occurrence.source.span(), call))?.as_ref()?;
        if !call
            && matches!(&usage.path, UsePath::Opened(_))
                != (occurrence.syntax_kind == "fsharp.simple_function_body_read.reference")
        {
            return None;
        }
        if usage.envelope.evidence.source.as_ref() != Some(&occurrence.source)
            || usage.envelope.provenance != occurrence.provenance
        {
            return None;
        }
        let proof = usage.proof.as_ref()?;
        if proof
            .opened_spelling
            .is_some_and(|spellings| !spellings.contains(&occurrence.syntactic_text_hash))
        {
            return None;
        }
        Some(proof)
    }

    pub(crate) fn eligible(&self, occurrence: &OccurrenceRecord) -> bool {
        self.proof(occurrence).is_some()
    }

    pub(crate) fn evidence<'s>(
        &'s self,
        occurrence: &OccurrenceRecord,
    ) -> Option<impl Iterator<Item = &'a ExtensionEnvelope> + 's> {
        let proof = self.proof(occurrence)?;
        let graph = match proof.project_graph {
            Some(project) => Some(&self.project_graphs.get(&project)?.evidence),
            None => None,
        };
        Some(
            proof
                .base
                .into_iter()
                .chain(graph.into_iter().flat_map(|inputs| inputs.iter().copied()))
                .chain(proof.imports.iter().copied()),
        )
    }

    pub(crate) fn rule(&self, occurrence: &OccurrenceRecord) -> Option<&'static str> {
        let proof = self.proof(occurrence)?;
        Some(if proof.opened_spelling.is_some() {
            "fsharp-opened-value-v6.owned_open_source_declared_project"
        } else if proof.alias_masked {
            "fsharp-qualified-module-v5.alias_signature_mask_source_declared_project"
        } else if proof.type_masked {
            "fsharp-qualified-module-v4.record_signature_mask_source_declared_project"
        } else if proof.signature_masked {
            "fsharp-qualified-module-v3.signature_mask_source_declared_project"
        } else if proof.project_graph.is_some() {
            "fsharp-qualified-module-v2.owned_open_source_declared_project"
        } else {
            "fsharp-qualified-module-v1.source_declared_project"
        })
    }

    pub(crate) fn entity_evidence<'s>(
        &'s self,
        occurrence: &OccurrenceRecord,
    ) -> Option<impl Iterator<Item = &'a EntityRecord> + 's> {
        let proof = self.proof(occurrence)?;
        let graph = match proof.project_graph {
            Some(project) => Some(&self.project_graphs.get(&project)?.entities),
            None => None,
        };
        Some(
            graph
                .into_iter()
                .flat_map(|entities| entities.iter().copied()),
        )
    }

    pub(crate) fn resolve(&self, occurrence: &OccurrenceRecord) -> Option<ResolutionDecision> {
        let symbol = self.proof(occurrence)?.symbol;
        let confidence =
            rootlight_ir::Confidence::new(900).expect("fixed structural confidence is valid");
        Some(ResolutionDecision {
            occurrence: occurrence.id,
            outcome: ResolutionOutcome::Resolved { symbol, confidence },
            explanation: ResolutionExplanation {
                rule: ResolutionRule::FSharpQualifiedModule,
                provider_name: RESOLVER_PROVIDER_NAME,
                provider_version: RESOLVER_PROVIDER_VERSION,
                candidates: vec![CandidateExplanation {
                    symbol,
                    score: confidence,
                    positive_signals: vec![
                        ResolutionSignal::NativeProjectMembership,
                        ResolutionSignal::NativeQualifiedHeader,
                    ],
                    penalties: Vec::new(),
                }],
                rejected_candidates: Vec::new(),
                rejected_total: 0,
                completeness_assumptions: vec![
                    CompletenessAssumption::ValidatedNormalizedDocument,
                    CompletenessAssumption::SingleGeneration,
                    CompletenessAssumption::NoRepositoryExecution,
                    CompletenessAssumption::SourceDeclaredProjectOrder,
                ],
            },
        })
    }
}

fn suffix(path: &str, expected: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case(expected))
}

fn span_contains(owner: SourceSpan, source: SourceSpan) -> bool {
    owner.file() == source.file()
        && owner.start_byte() <= source.start_byte()
        && source.end_byte() <= owner.end_byte()
}

fn canonical_identifier(name: &str) -> Option<&str> {
    // Native entity names retain delimiters; binding paths hash their contents.
    let canonical = if let Some(quoted) = name.strip_prefix("``") {
        let inner = quoted.strip_suffix("``")?;
        if inner.contains("``") {
            return None;
        }
        inner
    } else {
        if name.contains('`') {
            return None;
        }
        name
    };
    (!canonical.is_empty() && !canonical.chars().any(char::is_control)).then_some(canonical)
}

fn copy_path(path: &[ContentHash]) -> Result<Vec<ContentHash>, ResolutionError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(path.len())
        .map_err(|_| ResolutionError::CountOverflow)?;
    result.extend_from_slice(path);
    Ok(result)
}

fn copy_spans(spans: &[SourceSpan]) -> Result<Vec<SourceSpan>, ResolutionError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(spans.len())
        .map_err(|_| ResolutionError::CountOverflow)?;
    result.extend_from_slice(spans);
    Ok(result)
}
