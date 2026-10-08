//! Joins literal ordered F# project declarations to admitted immutable sources.
//! Project claims are composition evidence, not MSBuild execution; incomplete
//! documents or partial admission cannot authorize a unique cross-file target.

use super::*;
use rootlight_ir::{FSHARP_BINDING_NAMESPACE, FSharpBinding, FSharpBindingEvidence};

pub(super) fn append_project_evidence(
    document: &mut NormalizedIrDocument,
    sources: &[RustSourceInput],
    limits: &IrLimits,
    cancellation: &Cancellation,
) -> Result<(), FirstSliceError> {
    if !document
        .extensions
        .iter()
        .any(|envelope| envelope.namespace == FSHARP_BINDING_NAMESPACE)
    {
        return Ok(());
    }
    let mut retained_sources = BTreeMap::new();
    for input in sources {
        check_cancellation(cancellation)?;
        retained_sources.insert(input.snapshot.file(), input.snapshot.content());
    }
    let mut paths = BTreeMap::new();
    for file in &document.files {
        check_cancellation(cancellation)?;
        paths
            .entry(file.path.as_str())
            .and_modify(|prior| *prior = None)
            .or_insert(Some(file));
    }
    let mut claims = Vec::new();
    let mut payload_bytes = 0_usize;
    for project in &document.files {
        check_cancellation(cancellation)?;
        if !project
            .path
            .rsplit_once('.')
            .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("fsproj"))
        {
            continue;
        }
        let Some(bytes) = retained_sources.get(&project.id) else {
            return Ok(());
        };
        if project.byte_length != u64::try_from(bytes.len()).map_err(|_| FirstSliceError::Limits)?
            || project.content_hash != content_hash(bytes)
        {
            return Err(FirstSliceError::Identity);
        }
        let members = match rootlight_discovery::ordered_fsharp_compile_members(
            bytes,
            &project.path,
            cancellation,
        )
        .map_err(|error| map_discovery_error(error, cancellation))?
        {
            rootlight_discovery::OrderedFSharpCompileMembers::Valid(members) => members,
            rootlight_discovery::OrderedFSharpCompileMembers::Rejected
            | rootlight_discovery::OrderedFSharpCompileMembers::Bounded => return Ok(()),
        };
        if members.is_empty() || members.len() > rootlight_ir::MAX_FSHARP_PROJECT_MEMBERS {
            return Ok(());
        }
        let mut files = Vec::new();
        files
            .try_reserve_exact(members.len())
            .map_err(|_| FirstSliceError::Limits)?;
        let mut seen = BTreeSet::new();
        let mut ordered_sources = Vec::new();
        ordered_sources
            .try_reserve_exact(members.len())
            .map_err(|_| FirstSliceError::Limits)?;
        for member in members {
            check_cancellation(cancellation)?;
            let Some(Some(file)) = paths.get(member.as_str()) else {
                return Ok(());
            };
            if !seen.insert(file.id) {
                return Ok(());
            }
            ordered_sources.push((file.path.as_str(), file.language.as_str()));
            files.push(file.id);
        }
        if !supported_signature_order(&ordered_sources, cancellation)? {
            return Ok(());
        }
        let source = project
            .evidence
            .source
            .as_ref()
            .filter(|source| {
                source.span().file() == project.id
                    && source.span().start_byte() == 0
                    && source.span().end_byte() == project.byte_length
                    && source.content_hash() == project.content_hash
            })
            .ok_or(FirstSliceError::Identity)?;
        let claim = rootlight_ir::new_fsharp_binding_envelope(
            document.repository,
            document.generation,
            project.provenance,
            source.clone(),
            FSharpBindingEvidence::new(source.span(), FSharpBinding::Project { members: files }),
        )
        .map_err(|_| FirstSliceError::Identity)?;
        payload_bytes = payload_bytes
            .checked_add(claim.payload.len())
            .ok_or(FirstSliceError::Limits)?;
        if claim.payload.len() > limits.max_extension_payload_bytes
            || claim.evidence.derivation.len() > limits.max_nested_items_per_record
            || payload_bytes > limits.max_total_extension_bytes
            || claims.len() >= limits.max_extensions
        {
            return Ok(());
        }
        claims.try_reserve(1).map_err(|_| FirstSliceError::Limits)?;
        claims.push(claim);
    }
    append_claims_within_limits(document, claims, limits)
}

fn supported_signature_order(
    members: &[(&str, &str)],
    cancellation: &Cancellation,
) -> Result<bool, FirstSliceError> {
    let mut stems = BTreeMap::<&str, (Option<usize>, Option<usize>)>::new();
    for (ordinal, &(path, language)) in members.iter().enumerate() {
        check_cancellation(cancellation)?;
        let Some((stem, suffix)) = path.rsplit_once('.') else {
            return Ok(false);
        };
        let signature = suffix.eq_ignore_ascii_case("fsi") && language == "fsharp-signature";
        let implementation = suffix.eq_ignore_ascii_case("fs") && language == "fsharp";
        if !signature && !implementation {
            return Ok(false);
        }
        let pair = stems.entry(stem).or_default();
        let slot = if signature { &mut pair.0 } else { &mut pair.1 };
        if slot.replace(ordinal).is_some() {
            return Ok(false);
        }
    }
    // Exact authored stems are only a conservative composition prerequisite.
    // Resolver-owned native container identities corroborate every actual pair.
    for (signature, implementation) in stems.values() {
        check_cancellation(cancellation)?;
        if let Some(signature) = signature
            && !implementation.is_some_and(|implementation| *signature < implementation)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn append_claims_within_limits(
    document: &mut NormalizedIrDocument,
    claims: Vec<rootlight_ir::ExtensionEnvelope>,
    limits: &IrLimits,
) -> Result<(), FirstSliceError> {
    // Keeping only one of two competing project memberships would turn budget
    // pressure into false uniqueness. Composition is admitted atomically.
    document
        .extensions
        .try_reserve(claims.len())
        .map_err(|_| FirstSliceError::Limits)?;
    let original_len = document.extensions.len();
    document.extensions.extend(claims);
    if rootlight_ir::preflight_ir_document_limits(document, limits).is_err() {
        document.extensions.truncate(original_len);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::FactId;
    use rootlight_ir::{ExtensionEnvelope, SourceSpan};

    fn claim(file: u8, binding: FSharpBinding) -> ExtensionEnvelope {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let scope = SourceSpan::new(FileId::from_bytes([file; 20]), 0, 10).unwrap();
        rootlight_ir::new_fsharp_binding_envelope(
            repository,
            generation,
            FactId::from_bytes([3; 20]),
            SourceRef::new(repository, generation, scope, content_hash(b"source"), None),
            FSharpBindingEvidence::new(scope, binding),
        )
        .unwrap()
    }

    fn competing_projects() -> (NormalizedIrDocument, Vec<ExtensionEnvelope>) {
        let context = claim(9, FSharpBinding::Context);
        let mut document = NormalizedIrDocument::empty(context.repository, context.generation);
        document.extensions.push(context);
        let claims = [4, 5]
            .into_iter()
            .map(|file| {
                claim(
                    file,
                    FSharpBinding::Project {
                        members: vec![FileId::from_bytes([8; 20]), FileId::from_bytes([9; 20])],
                    },
                )
            })
            .collect();
        (document, claims)
    }

    fn assert_atomic_rejection(limits: IrLimits) {
        let (mut document, claims) = competing_projects();
        let original = document.clone();
        let mut one_project = document.clone();
        append_claims_within_limits(&mut one_project, vec![claims[0].clone()], &limits).unwrap();
        assert_eq!(one_project.extensions.len(), 2);
        append_claims_within_limits(&mut document, claims, &limits).unwrap();
        assert_eq!(document, original);
    }

    #[test]
    fn fsharp_project_nested_budget_keeps_competing_claims_atomic() {
        let mut limits = IrLimits::default();
        limits.max_total_nested_items = 4;
        assert_atomic_rejection(limits);
    }

    #[test]
    fn fsharp_project_string_budget_keeps_competing_claims_atomic() {
        let mut limits = IrLimits::default();
        limits.max_total_string_bytes =
            2 * (FSHARP_BINDING_NAMESPACE.len() + rootlight_ir::FSHARP_BINDING_VERSION.len());
        assert_atomic_rejection(limits);
    }

    #[test]
    fn fsharp_project_signatures_require_exact_earlier_authored_stems() {
        let cancellation = Cancellation::new();
        assert!(
            supported_signature_order(
                &[
                    ("src/Library.fsi", "fsharp-signature"),
                    ("Peer.fs", "fsharp"),
                    ("src/Library.fs", "fsharp"),
                    ("Consumer.fs", "fsharp"),
                ],
                &cancellation,
            )
            .unwrap()
        );
        for members in [
            vec![("Library.fsi", "fsharp-signature")],
            vec![
                ("Library.fs", "fsharp"),
                ("Library.fsi", "fsharp-signature"),
            ],
            vec![
                ("src/Library.fsi", "fsharp-signature"),
                ("other/Library.fs", "fsharp"),
            ],
            vec![
                ("library.fsi", "fsharp-signature"),
                ("Library.fs", "fsharp"),
            ],
            vec![("Library.fsi", "fsharp"), ("Library.fs", "fsharp")],
            vec![
                ("Library.fsi", "fsharp-signature"),
                ("Library.fsi", "fsharp-signature"),
                ("Library.fs", "fsharp"),
            ],
        ] {
            assert!(
                !supported_signature_order(&members, &cancellation).unwrap(),
                "{members:?}"
            );
        }
    }
}
