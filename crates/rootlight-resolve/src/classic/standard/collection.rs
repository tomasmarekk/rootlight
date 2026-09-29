//! Static Collection contracts extend project-proven receiver types.
//! The model supplies external identities, never repository definitions or
//! claims about a collection's runtime contents or successful execution.

use super::*;
use rootlight_vba::ProcedureKind;

pub(super) struct Member {
    pub(super) name: &'static str,
    pub(super) signature: &'static str,
    parameters: &'static [(&'static str, bool)],
    kind: ProcedureKind,
}

// MS-VBAL 6.1.3.1 specifies functions, including the default Item function.
// https://learn.microsoft.com/en-us/openspecs/microsoft_general_purpose_programming_languages/ms-vbal/31ec9e63-f71e-4521-863b-a7d12007b7cc
pub(super) const MEMBERS: &[Member] = &[
    Member {
        name: "Count",
        signature: "Function Count() As Long",
        parameters: &[],
        kind: ProcedureKind::Function,
    },
    Member {
        name: "Item",
        signature: "Function Item(Index As Variant) As Variant",
        parameters: &[("index", false)],
        kind: ProcedureKind::Function,
    },
    Member {
        name: "Add",
        signature: "Sub Add(Item As Variant, Optional Key As Variant, Optional Before As Variant, Optional After As Variant)",
        parameters: &[
            ("item", false),
            ("key", true),
            ("before", true),
            ("after", true),
        ],
        kind: ProcedureKind::Sub,
    },
    Member {
        name: "Remove",
        signature: "Sub Remove(Index As Variant)",
        parameters: &[("index", false)],
        kind: ProcedureKind::Sub,
    },
];

pub(super) fn qualified(name: &str) -> Option<&'static Member> {
    let name = name.strip_prefix("VBA.Collection.")?;
    MEMBERS.iter().find(|member| member.name == name)
}

pub(in crate::classic) fn target<'a>(
    index: &CandidateIndex<'a>,
    repository: rootlight_ids::RepositoryId,
    name: &str,
) -> Option<&'a EntityRecord> {
    index
        .entities
        .get(&claim(repository, name).symbol)
        .copied()
        .filter(|entity| {
            entity.kind == EntityKind::ExternalSymbol
                && entity.evidence.source.is_none()
                && index
                    .provenance
                    .get(&entity.provenance)
                    .is_some_and(|p| p.rule.as_deref() == Some(CATALOG_RULE))
        })
}

pub(in crate::classic) fn is_class(entity: &EntityRecord) -> bool {
    entity.kind == EntityKind::ExternalSymbol
        && entity.canonical_name == "VBA.Collection"
        && entity.id == claim(entity.repository, "VBA.Collection").symbol
        && entity.evidence.source.is_none()
}

pub(in crate::classic) fn type_target<'a>(
    index: &CandidateIndex<'a>,
    project: &ResolvedProject,
    site: &OccurrenceRecord,
    text: &[u8],
    local_types: &BTreeMap<(FileId, String), bool>,
    check: &mut impl FnMut() -> Result<(), ResolutionError>,
) -> Result<Option<&'a EntityRecord>, ResolutionError> {
    if !project.evidence.native || site.role != OccurrenceRole::TypeUse {
        return Ok(None);
    }
    let Some(path) = rootlight_vba::procedure_pointer_path(text, check)? else {
        return Ok(None);
    };
    if path.end() != text.len()
        || path.components() > 2
        || !path.last().eq_ignore_ascii_case(b"Collection")
        || (path.components() == 2 && !path.first().eq_ignore_ascii_case(b"VBA"))
    {
        return Ok(None);
    }
    let root = if path.components() == 2 {
        "vba"
    } else {
        "collection"
    };
    if project.name == root
        || project.types.contains_key(root)
        || project.modules.contains_key(root)
        // VBA is the mandatory standard-library qualifier (MS-VBAL 2.7.1,
        // 4.1); a retained reference bearing that name is not a shadow.
        || (path.components() == 1 && project.references.may_declare_name(root))
        || local_types.contains_key(&(site.file, root.to_owned()))
    {
        return Ok(None);
    }
    Ok(target(index, site.repository, "VBA.Collection"))
}

pub(in crate::classic) fn member_target<'a>(
    index: &CandidateIndex<'a>,
    site: &OccurrenceRecord,
    name: &str,
) -> Option<(&'a EntityRecord, bool)> {
    let member = MEMBERS
        .iter()
        .find(|member| member.name.eq_ignore_ascii_case(name))?;
    let bare = match (member.kind, site.syntax_kind.as_str()) {
        (ProcedureKind::Function, "vba.get_member.reference") if member.parameters.is_empty() => {
            true
        }
        (
            ProcedureKind::Function,
            "vba.invoked_get_member.reference"
            | "vba.invoked_member.reference"
            | "vba.member.reference",
        )
        | (ProcedureKind::Sub, "vba.invoked_member.reference" | "vba.member.reference") => false,
        _ => return None,
    };
    target(
        index,
        site.repository,
        &format!("VBA.Collection.{}", member.name),
    )
    .map(|target| (target, bare))
}

#[derive(Default)]
pub(in crate::classic) struct Calls<'s>(super::arguments::Cache<'s>);

impl<'s> Calls<'s> {
    pub(in crate::classic) fn prove<'a>(
        &mut self,
        site: &OccurrenceRecord,
        callee: &OccurrenceRecord,
        explicit: bool,
        proof: &Proof<'a>,
        sources: &BTreeMap<FileId, &'s [u8]>,
        check: &mut impl FnMut() -> Result<(), ResolutionError>,
    ) -> Result<Option<Proof<'a>>, ResolutionError> {
        if proof.rule != ResolutionRule::VbaCollectionMember {
            return Ok(None);
        }
        let Some(member) = qualified(&proof.target.canonical_name) else {
            return Ok(None);
        };
        let Some(tail) = callee
            .source
            .span()
            .end_byte()
            .checked_sub(site.source.span().start_byte())
            .and_then(|offset| usize::try_from(offset).ok())
            .and_then(|offset| site_bytes(site, sources)?.get(offset..))
        else {
            return Ok(None);
        };
        if !self.0.matches(
            super::arguments::Key {
                target: proof.target.id,
                source_hash: site.syntactic_text_hash,
                explicit,
            },
            tail,
            member.parameters,
            member.kind,
            check,
        )? {
            return Ok(None);
        }
        Ok(Some(Proof {
            target: proof.target,
            context: Arc::clone(&proof.context),
            receiver: proof.receiver.clone(),
            rule: ResolutionRule::VbaCollectionCall,
            property_call: true,
        }))
    }
}

#[derive(Default)]
pub(in crate::classic) struct ValueReads {
    files: BTreeMap<FileId, BTreeMap<rootlight_ir::SourceSpan, SourceRef>>,
}

impl ValueReads {
    pub(in crate::classic) fn witness(
        &mut self,
        site: &OccurrenceRecord,
        index: &CandidateIndex<'_>,
        sources: &BTreeMap<FileId, &[u8]>,
        check: &mut impl FnMut() -> Result<(), ResolutionError>,
    ) -> Result<Option<SourceRef>, ResolutionError> {
        if let std::collections::btree_map::Entry::Vacant(entry) = self.files.entry(site.file) {
            let Some((source, bytes)) = index
                .files
                .get(&site.file)
                .and_then(|file| file.evidence.source.as_ref())
                .zip(sources.get(&site.file))
            else {
                return Ok(None);
            };
            let mut witnesses = BTreeMap::new();
            if let Some(reads) = rootlight_vba::statement_application_reads(
                bytes,
                crate::MAX_RESOLUTION_WORK_LIMIT,
                crate::MAX_RESOLUTION_WORK_LIMIT,
                check,
            )? {
                for read in reads {
                    check()?;
                    let value = with_members::source_ref(source, read.value())?;
                    let statement = with_members::source_ref(source, read.statement())?;
                    witnesses.insert(value.span(), statement);
                }
            }
            entry.insert(witnesses);
        }
        Ok(self
            .files
            .get(&site.file)
            .and_then(|reads| reads.get(&site.source.span()))
            .cloned())
    }
}
