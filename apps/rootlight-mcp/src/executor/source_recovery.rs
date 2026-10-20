//! Bind aggregate recovery to mapped source bytes without upgrading analysis.

use super::*;
use rootlight_mcp_contract::source_recovery::{
    SourceReadRecovery, SourceRecoveryFile, SourceTextSearchCoverage,
};

pub(super) fn validate_analysis(
    recovery: Option<&client::SourceReadRecovery>,
    context: &client::QueryContext,
    mut tiers: impl Iterator<Item = client::AnalysisTier>,
) -> Result<(), ToolExecutionError> {
    if recovery.is_some()
        && (context.tier != client::AnalysisTier::TierD
            || context.coverage_status != client::CoverageStatus::Unknown
            || tiers.any(|tier| tier != client::AnalysisTier::TierD))
    {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    Ok(())
}

fn convert(recovery: client::SourceReadRecovery) -> Result<SourceReadRecovery, ToolExecutionError> {
    if !recovery.cause.is_valid() || recovery.files.is_empty() || recovery.files.len() > 32 {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    let mut files = Vec::new();
    files
        .try_reserve_exact(recovery.files.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for file in recovery.files {
        files.push(SourceRecoveryFile {
            file: file.file,
            text_search: match file.text_search {
                client::SourceTextSearchCoverage::WholeSourceProjection => {
                    SourceTextSearchCoverage::WholeSourceProjection
                }
                client::SourceTextSearchCoverage::MetadataOnlyCaptured => {
                    SourceTextSearchCoverage::MetadataOnlyCaptured
                }
                client::SourceTextSearchCoverage::UnsupportedEncoding => {
                    SourceTextSearchCoverage::UnsupportedEncoding
                }
            },
        });
    }
    Ok(SourceReadRecovery {
        cause: recovery.cause,
        files,
    })
}

pub(super) fn validate_membership(
    recovery: &client::SourceReadRecovery,
    returned: &[rootlight_ids::FileId],
) -> Result<(), ToolExecutionError> {
    convert(recovery.clone())?
        .validate_files(returned)
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

pub(super) fn attach(
    output: &mut Map<String, Value>,
    recovery: Option<client::SourceReadRecovery>,
) -> Result<(), ToolExecutionError> {
    let Some(recovery) = recovery else {
        return Ok(());
    };
    let mapped = convert(recovery)?;
    let data = output
        .get_mut("data")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| internal(ToolExecutionFailure::InvalidResponse))?;
    let chunks = data
        .get("chunks")
        .and_then(Value::as_array)
        .ok_or_else(|| internal(ToolExecutionFailure::InvalidResponse))?;
    let mut returned = Vec::new();
    returned
        .try_reserve_exact(chunks.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for chunk in chunks {
        let source = chunk
            .get("source_ref")
            .ok_or_else(|| internal(ToolExecutionFailure::InvalidResponse))?;
        let source: rootlight_ir::SourceRef = serde_json::from_value(source.clone())
            .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
        returned.push(source.span().file());
    }
    mapped
        .validate_files(&returned)
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
    data.insert(
        "source_recovery".into(),
        serde_json::to_value(mapped).map_err(|_| internal(ToolExecutionFailure::Executor))?,
    );
    // The router's final measured publication includes these fields. It returns
    // a typed resource error if their real JSON/token cost does not fit.
    Ok(())
}

pub(super) fn retained_contract_error() -> ToolExecutionError {
    let error = PublicError::builder(
        ErrorCode::ProtocolMismatch,
        "tool contract version is unsupported",
    )
    .detail(
        DetailKey::parse("supported_version").expect("static public detail key"),
        PublicValue::Label(SafeLabel::parse("1.4").expect("static contract version")),
    )
    .next_action(NextAction::SelectSupportedVersion)
    .build()
    .expect("static protocol-mismatch error contract");
    ToolExecutionError::new(error)
}
