//! Matches native project locators against the immutable indexed environment.
//! Version/name agreement and complete source inventories establish qualified
//! class access; paths never authorize filesystem reads or library execution.

use super::*;

pub(super) fn link(
    projects: &mut [Option<ResolvedProject>],
    native: &[NativeClassicProject],
    paths: &BTreeMap<String, Option<&rootlight_ir::FileRecord>>,
    files: &BTreeMap<FileId, &rootlight_ir::FileRecord>,
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<(), ResolutionError> {
    if native.is_empty() {
        return Ok(());
    }
    let mut targets = BTreeMap::new();
    for (index, project) in projects.iter().enumerate() {
        cancellation.check()?;
        work.consume()?;
        if let Some(project) = project.as_ref().filter(|project| project.evidence.native) {
            targets.insert(project.evidence.manifest, index);
        }
    }
    let mut versions = BTreeMap::new();
    for project in native {
        cancellation.check()?;
        work.consume()?;
        versions.insert(project.container, project.version);
    }
    let mut linked = Vec::new();
    for (index, project) in projects.iter().enumerate() {
        cancellation.check()?;
        work.consume()?;
        let Some(project) = project.as_ref().filter(|project| project.evidence.native) else {
            continue;
        };
        let owner = files
            .get(&project.evidence.manifest)
            .ok_or(ResolutionError::ClassicSourceIdentity)?;
        let mut references = BTreeMap::new();
        let mut evidence = None;
        for field in project.references.project_fields() {
            cancellation.check()?;
            work.consume()?;
            let Some(descriptor) = &field.project else {
                continue;
            };
            work.consume_many(descriptor.absolute.path.len().div_ceil(64).max(1))?;
            work.consume_many(descriptor.relative.path.len().div_ceil(64).max(1))?;
            let Some(path) = embedded_windows_path(&owner.path, descriptor) else {
                continue;
            };
            let Some(target_file) = paths.get(&path).copied().flatten() else {
                continue;
            };
            let Some(&target_index) = targets
                .get(&target_file.id)
                .filter(|target| **target != index)
            else {
                continue;
            };
            let Some(target) = projects.get(target_index).and_then(Option::as_ref) else {
                continue;
            };
            let Some(label) = field.name.as_deref().and_then(identifier) else {
                continue;
            };
            if target.name != label
                || versions.get(&target_file.id).copied().flatten()
                    != Some((descriptor.major_version, descriptor.minor_version))
            {
                continue;
            }
            let metadata = files
                .get(&field.source.file())
                .ok_or(ResolutionError::ClassicSourceIdentity)?;
            let source = SourceRef::new(
                metadata.repository,
                metadata.generation,
                field.source,
                metadata.content_hash,
                None,
            );
            work.consume_many(target.evidence.sources.len())?;
            work.consume_many(target.evidence.facts.len())?;
            if evidence.is_none() {
                work.consume_many(project.evidence.sources.len())?;
                work.consume_many(project.evidence.facts.len())?;
                evidence = Some((*project.evidence).clone());
            }
            let Some(evidence) = evidence.as_mut() else {
                continue;
            };
            evidence.sources.push(source);
            evidence
                .sources
                .extend(target.evidence.sources.iter().cloned());
            evidence.facts.push(FactRef::File(metadata.id));
            evidence.facts.extend(target.evidence.facts.iter().copied());
            references.insert(label, target_index);
        }
        let Some(mut evidence) = evidence else {
            continue;
        };
        // One shared root per enclosing project must include every reference
        // witness before occurrence traversal; cache order cannot select evidence.
        evidence.sources.sort_unstable();
        evidence.sources.dedup();
        evidence.facts.sort_unstable();
        evidence.facts.dedup();
        linked.push((index, references, Arc::new(evidence)));
    }
    for (index, references, evidence) in linked {
        cancellation.check()?;
        work.consume()?;
        if let Some(project) = projects.get_mut(index).and_then(Option::as_mut) {
            project.reference_projects = references;
            project.evidence = evidence;
        }
    }
    Ok(())
}

fn embedded_windows_path(owner: &str, reference: &NativeClassicProjectReference) -> Option<String> {
    if reference.kind != NativeClassicProjectReferenceKind::WindowsEmbedded
        || reference.absolute.path.len() > MAX_PATH_BYTES
        || reference
            .absolute
            .path
            .contains(['\0', '*', '?', '<', '>', '|', '"'])
    {
        return None;
    }
    let absolute = reference.absolute.path.as_bytes();
    let drive = absolute.first().is_some_and(u8::is_ascii_alphabetic)
        && absolute.get(1) == Some(&b':')
        && absolute
            .get(2)
            .is_some_and(|byte| matches!(byte, b'\\' | b'/'));
    let unc = reference.absolute.path.starts_with("\\\\");
    if !drive && !unc {
        return None;
    }
    let tail = reference.absolute.path.get(if drive { 3 } else { 2 }..)?;
    if tail.contains(':')
        || tail.split(['\\', '/']).any(|part| {
            part.is_empty() || part == "." || part == ".." || part.ends_with([' ', '.'])
        })
    {
        return None;
    }
    if unc && tail.split(['\\', '/']).count() < 3 {
        return None;
    }
    let path = member_path(owner, &reference.relative.path)?;
    let declared_name = tail.rsplit(['\\', '/']).next()?;
    let target_name = path.rsplit('/').next()?;
    declared_name
        .eq_ignore_ascii_case(target_name)
        .then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ir::SourceSpan;

    fn reference(absolute: &str, relative: &str) -> NativeClassicProjectReference {
        let file = FileId::from_bytes([7; 20]);
        NativeClassicProjectReference {
            kind: NativeClassicProjectReferenceKind::WindowsEmbedded,
            absolute: NativeClassicProjectLocator {
                source: SourceSpan::new(file, 0, 3).unwrap(),
                path: absolute.to_owned(),
            },
            relative: NativeClassicProjectLocator {
                source: SourceSpan::new(file, 3, 6).unwrap(),
                path: relative.to_owned(),
            },
            major_version: 1,
            minor_version: 0,
            version_source: SourceSpan::new(file, 6, 12).unwrap(),
        }
    }

    #[test]
    fn embedded_paths_use_declared_syntax_and_stay_inside_indexed_roots() {
        for (absolute, relative, expected) in [
            (
                "C:\\library\\target.xlsm",
                "../target.xlsm",
                Some("target.xlsm"),
            ),
            (
                "C:/library/TARGET.XLSM",
                "../TARGET.XLSM",
                Some("target.xlsm"),
            ),
            (
                "\\\\server\\share\\target.xlsm",
                ".\\nested\\target.xlsm",
                Some("clients/nested/target.xlsm"),
            ),
            ("C:\\library\\target.xlsm", "../../target.xlsm", None),
            ("C:\\library\\different.xlsm", "../target.xlsm", None),
            ("C:target.xlsm", "../target.xlsm", None),
            ("/library/target.xlsm", "../target.xlsm", None),
            ("\\\\server\\target.xlsm", "../target.xlsm", None),
            ("C:\\library\\target.xlsm", "C:\\target.xlsm", None),
            ("C:\\library\\target.xlsm", "\\target.xlsm", None),
            ("C:\\library\\target.xlsm", "../target.xlsm.", None),
            ("C:\\library\\..\\target.xlsm", "../target.xlsm", None),
            ("C:\\library\\target.xlsm:stream", "../target.xlsm", None),
            ("C:\\library\\target.xlsm", "", None),
        ] {
            assert_eq!(
                embedded_windows_path("clients/client.xlsm", &reference(absolute, relative))
                    .as_deref(),
                expected,
                "{absolute}, {relative}"
            );
        }
        for kind in [
            NativeClassicProjectReferenceKind::WindowsStandalone,
            NativeClassicProjectReferenceKind::MacintoshStandalone,
            NativeClassicProjectReferenceKind::MacintoshEmbedded,
        ] {
            let mut descriptor = reference("C:\\library\\target.xlsm", "../target.xlsm");
            descriptor.kind = kind;
            assert!(embedded_windows_path("clients/client.xlsm", &descriptor).is_none());
        }
    }
}
