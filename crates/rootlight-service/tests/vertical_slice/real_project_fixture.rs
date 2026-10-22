//! Real parser/project adapter bridge for a bounded service-only Rust fixture.
//!
//! This explicitly permitted in-process test path does not qualify installed
//! process isolation, native memory enforcement, or a whole language.

use std::{path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisUnitId, BatchThresholds, BuildTargetId, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, ProjectAnalysisLimits,
    ProjectAnalysisRequest, ProjectSourceInput, StreamLimits, execute_project_analysis,
};
use rootlight_adapter_treesitter::{ParserSettings, RuntimeConfig, TreeSitterProvider};
use rootlight_adapters::{SemanticProjectAnalyzer, SemanticProjectLanguage};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, content_hash};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ExtensionSupport, IrLimits, ProducerIdentity, SourceRef,
    SourceSpan,
};
use rootlight_runtime::RuntimePaths;
use rootlight_service::{
    FirstSliceProjectAnalysis, FirstSliceProjectAnalysisError, FirstSliceProjectAnalysisRequest,
    FirstSliceProjectAnalyzer, FirstSliceService,
};
use rootlight_vfs::{RelativePath, SourceSnapshot};

struct RustProjectAnalyzer;

fn adapter_error(error: AdapterError) -> FirstSliceProjectAnalysisError {
    match error {
        AdapterError::Cancelled { reason } => FirstSliceProjectAnalysisError::Cancelled(reason),
        _ => FirstSliceProjectAnalysisError::Analysis,
    }
}

fn limits() -> AnalysisLimits {
    let batch = BatchThresholds::new(256, 4 * 1024 * 1024, 128, 128 * 1024).unwrap();
    let stream = StreamLimits::new(
        256,
        16 * 1024,
        4 * 1024 * 1024,
        1024,
        1024 * 1024,
        4 * 1024 * 1024,
        batch,
    )
    .unwrap();
    AnalysisLimits::new(
        64 * 1024,
        64 * 1024,
        4096,
        256,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap()
    .with_project_limits(
        ProjectAnalysisLimits::new(16, 512 * 1024, 64 * 1024, 128, 128 * 1024, 256, 256).unwrap(),
    )
}

impl FirstSliceProjectAnalyzer for RustProjectAnalyzer {
    fn provider_identity(&self) -> ContentHash {
        content_hash(b"rootlight-service-test-real-rust-project/1")
    }

    fn analyze(
        &self,
        request: FirstSliceProjectAnalysisRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceProjectAnalysis, FirstSliceProjectAnalysisError> {
        let failed = || FirstSliceProjectAnalysisError::Analysis;
        if request.language() != "rust" || request.inputs().len() > 16 {
            return Err(failed());
        }
        let snapshots = request
            .inputs()
            .iter()
            .map(|input| {
                if input.source().len() > 64 * 1024 {
                    return Err(failed());
                }
                SourceSnapshot::from_persisted(
                    request.repository(),
                    RelativePath::parse(Path::new(input.path())).map_err(|_| failed())?,
                    input.file(),
                    input.content_hash(),
                    input.source().to_vec(),
                )
                .map_err(|_| failed())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let references = snapshots
            .iter()
            .map(|snapshot| {
                SourceRef::new(
                    request.repository(),
                    request.generation(),
                    SourceSpan::new(
                        snapshot.file(),
                        0,
                        u64::try_from(snapshot.content().len()).unwrap(),
                    )
                    .unwrap(),
                    snapshot.content_hash(),
                    None,
                )
            })
            .collect::<Vec<_>>();
        let mut inputs = snapshots
            .iter()
            .zip(&references)
            .zip(request.inputs())
            .map(|((snapshot, source), input)| {
                Ok(ProjectSourceInput::new(
                    GenerationBoundSnapshot::new(snapshot, source).map_err(|_| failed())?,
                    LanguageId::new("rust").map_err(|_| failed())?,
                    EncodingId::utf8(),
                    input.generated(),
                    input.origins().to_vec(),
                ))
            })
            .collect::<Result<Vec<_>, FirstSliceProjectAnalysisError>>()?;
        inputs.sort_by(|left, right| {
            left.source()
                .path()
                .identity_bytes()
                .cmp(right.source().path().identity_bytes())
        });
        let limits = limits();
        let project = ProjectAnalysisRequest::new(
            AnalysisUnitId::new("service.rust-fixture").map_err(|_| failed())?,
            BuildTargetId::new("//service:rust-fixture").map_err(|_| failed())?,
            BuildContextIdentity::new(request.build_context()),
            content_hash(request.context_manifest()),
            request.context_manifest(),
            inputs,
            AnalysisTier::TierB,
            &limits,
        )
        .map_err(|_| failed())?;
        let runtime = RuntimeConfig::new(
            64 * 1024,
            64 * 1024,
            4096,
            256,
            256,
            1,
            16 * 1024 * 1024,
            ParserSettings::new(4096).unwrap(),
        )
        .unwrap();
        let parser = Arc::new(TreeSitterProvider::new(runtime).map_err(|_| failed())?);
        let analyzer = SemanticProjectAnalyzer::new(
            SemanticProjectLanguage::Rust,
            parser,
            ProducerIdentity::new(
                "rootlight-service-real-project-test",
                "1.0.0",
                request.build_context(),
            )
            .map_err(|_| failed())?,
            self.provider_identity(),
            BuildContextIdentity::new(request.build_context()),
        )
        .map_err(|_| failed())?;
        let output = execute_project_analysis(
            &analyzer,
            &project,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            cancellation,
        )
        .map_err(adapter_error)?;
        // The fixture caller permits this reviewed in-process adapter; this flag
        // does not assert that an external isolated adapter process was observed.
        Ok(FirstSliceProjectAnalysis::new(
            output.document().clone(),
            true,
        ))
    }
}

/// Creates a bounded Deep fixture service using genuine parser and project output.
pub(super) fn service(state: &Path, cancellation: &Cancellation) -> FirstSliceService {
    // SQLite's no-follow open rejects ancestor aliases such as macOS's `/var`.
    // Resolve the existing fixture-owned parent before deriving durable paths.
    let state = state
        .canonicalize()
        .expect("real project fixture state parent canonicalizes");
    let paths = RuntimePaths::new(state.join("state"), state.join("runtime"))
        .expect("real project fixture runtime paths are valid");
    paths
        .prepare_owner()
        .expect("account-private fixture paths prepare");
    FirstSliceService::new_durable_with_project_analyzer(
        2,
        paths.state_dir(),
        Arc::new(RustProjectAnalyzer),
        cancellation,
    )
    .expect("real project fixture service initializes")
}
