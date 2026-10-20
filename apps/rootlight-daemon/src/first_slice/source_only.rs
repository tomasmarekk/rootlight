//! Readiness for independently verified source-only durable generations.
//! Semantic recovery and VBA activity retain their existing readiness barrier.

use super::*;

pub(super) fn ready(
    service: &FirstSliceService,
    request: &FirstSliceIpcRequest,
    context: &FirstSliceIpcContext,
) -> Result<bool, PublicError> {
    let (repository, selected, references) = match request {
        FirstSliceIpcRequest::SourceFind(request) => (
            parse_repository(request.repository.as_ref())?,
            parse_generation_selector(request.generation.as_ref())?,
            Vec::new(),
        ),
        FirstSliceIpcRequest::SourceRead(request)
            if matches!(
                daemon::SourceReadEncoding::try_from(request.encoding),
                Ok(daemon::SourceReadEncoding::Bytes | daemon::SourceReadEncoding::Utf8)
            ) =>
        {
            let mut references = Vec::new();
            references
                .try_reserve_exact(request.references.len())
                .map_err(|_| resource_exhausted())?;
            for reference in &request.references {
                references.push(source_ref_from_wire(reference)?);
            }
            (
                parse_repository(request.repository.as_ref())?,
                parse_generation_selector(request.generation.as_ref())?,
                references,
            )
        }
        _ => return Ok(false),
    };
    let generation =
        match service.resolve_source_generation(repository, selected, &context.cancellation) {
            Ok(generation) => generation,
            Err(
                rootlight_service::FirstSliceError::GenerationNotFound
                | rootlight_service::FirstSliceError::RepositoryNotFound,
            ) => return Ok(false),
            Err(error) => return Err(service_error(error)),
        };
    service
        .source_only_read_available(repository, generation, &references, &context.cancellation)
        .map_err(service_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_runtime::RuntimePaths;

    #[test]
    fn recovering_source_requests_use_exact_publication_without_claiming_graph_readiness() {
        let storage = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let fixture = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        std::fs::write(
            fixture.path().join("lib.rs"),
            b"// ababa\npub fn lantern() {}\n",
        )
        .unwrap();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .unwrap();
        paths.prepare_owner().unwrap();
        let cancellation = Cancellation::new();
        let (receipt, reference) = {
            let mut writer =
                FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
            let receipt = writer
                .index_rust_fixture(fixture.path(), &cancellation)
                .unwrap();
            let reference = writer
                .code_locate(
                    receipt.generation,
                    "lantern".to_owned(),
                    rootlight_query::LocateMode::Exact,
                    10,
                    0,
                    &cancellation,
                )
                .unwrap()
                .data
                .hits[0]
                .source
                .clone()
                .unwrap();
            (receipt, reference)
        };
        let (service, _restore) =
            FirstSliceService::open_durable_deferred(2, paths.state_dir()).unwrap();
        let context = FirstSliceIpcContext {
            client_instance_id: ClientInstanceId::SYSTEM,
            selected_protocol_minor: rootlight_daemon_core::PROTOCOL_MINOR,
            cancellation,
            deadline: None,
            effective_budget: None,
            index_admission: None,
        };
        let selector = daemon::GenerationSelector {
            selector: Some(daemon::generation_selector::Selector::Generation(
                generation_to_wire(receipt.generation),
            )),
        };
        let request = daemon::SourceFindRequest {
            schema_version: Some(schema_version()),
            repository: Some(repository_to_wire(receipt.repository)),
            generation: Some(selector.clone()),
            file: Some(file_to_wire(reference.span().file())),
            literal: "aba".to_owned(),
            maximum_scan_bytes: 4096,
            maximum_matches: 10,
            cursor: None,
        };
        assert!(
            ready(
                &service,
                &FirstSliceIpcRequest::SourceFind(request.clone()),
                &context
            )
            .unwrap()
        );
        let found = source_find::execute(&service, request, &context).unwrap();
        assert_eq!(found.matches.len(), 2);
        assert_eq!(found.file.as_ref().unwrap().language, "rust");
        assert_eq!(
            found.generation,
            Some(generation_to_wire(receipt.generation))
        );
        let read = daemon::SourceReadRequest {
            repository: Some(repository_to_wire(receipt.repository)),
            generation: Some(selector),
            references: vec![found.matches[0].clone()],
            encoding: daemon::SourceReadEncoding::Bytes as i32,
            ..Default::default()
        };
        assert!(
            ready(
                &service,
                &FirstSliceIpcRequest::SourceRead(read.clone()),
                &context
            )
            .unwrap()
        );
        let response = source_read(&service, read.clone(), &context).unwrap();
        assert_eq!(response.chunks.len(), 1);
        assert_eq!(response.chunks[0].exact_content, b"aba");
        assert_eq!(response.chunks[0].language, "rust");
        let metadata = response.context.unwrap();
        assert_eq!(
            metadata.coverage_status,
            daemon::FirstSliceCoverageStatus::FirstSliceCoverageUnknown as i32
        );
        assert_eq!(
            metadata.tier,
            daemon::FirstSliceAnalysisTier::FirstSliceTierD as i32
        );
        assert_eq!(metadata.structural_freshness, "current");
        assert_eq!(metadata.semantic_freshness, "stale");
        assert!(metadata.active_generation);
        assert!(metadata.language_coverage.is_none());
        let mut lossless = read;
        lossless.encoding = daemon::SourceReadEncoding::LosslessText as i32;
        assert!(
            !ready(
                &service,
                &FirstSliceIpcRequest::SourceRead(lossless),
                &context
            )
            .unwrap()
        );
        assert!(
            !ready(
                &service,
                &FirstSliceIpcRequest::CodeLocate(daemon::CodeLocateRequest::default()),
                &context
            )
            .unwrap()
        );
        assert!(
            service
                .resolve_generation(receipt.repository, Some(receipt.generation))
                .is_err()
        );
        let directory = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(receipt.repository.to_string())
            .join(receipt.generation.to_string());
        std::fs::write(directory.join("source-authority.json"), b"{}").unwrap();
        // A fresh service must authenticate the artifact before bypassing recovery.
        let (service, _restore) =
            FirstSliceService::open_durable_deferred(2, paths.state_dir()).unwrap();
        assert!(
            ready(
                &service,
                &FirstSliceIpcRequest::SourceFind(daemon::SourceFindRequest {
                    repository: Some(repository_to_wire(receipt.repository)),
                    file: Some(file_to_wire(reference.span().file())),
                    literal: "aba".to_owned(),
                    maximum_scan_bytes: 4096,
                    maximum_matches: 10,
                    ..Default::default()
                }),
                &context
            )
            .is_err()
        );
    }
}
